use anyhow::{anyhow, Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use regex::Regex;
use std::{
    collections::HashMap,
    collections::HashSet,
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tree_sitter::{Parser, Query, QueryCursor};

#[derive(Debug)]
struct PathAliases {
    aliases: HashMap<String, PathBuf>,
}

impl PathAliases {
    fn new(git_root: &Path) -> Self {
        let mut aliases = HashMap::new();
        aliases.insert("@".to_string(), git_root.to_path_buf());
        Self { aliases }
    }

    fn resolve_path(&self, import_path: &str, file_dir: &Path) -> Option<PathBuf> {
        if let Some((alias, rest)) = import_path.split_once('/') {
            if let Some(alias_path) = self.aliases.get(alias) {
                return Some(alias_path.join(rest));
            }
        }

        if import_path.starts_with('.') {
            return Some(file_dir.join(import_path));
        }

        None
    }
}

#[derive(Debug)]
struct ProjectContext {
    git_root: PathBuf,
    gitignore: Gitignore,
    path_aliases: PathAliases,
}

impl ProjectContext {
    fn new(input_file: &Path) -> Result<Self> {
        let git_root = find_git_root(input_file)?;
        let gitignore = load_gitignore(&git_root)?;
        let path_aliases = PathAliases::new(&git_root);
        Ok(Self {
            git_root,
            gitignore,
            path_aliases,
        })
    }

    fn is_ignored(&self, path: &Path) -> bool {
        if !path
            .canonicalize()
            .map(|p| p.starts_with(&self.git_root))
            .unwrap_or(false)
        {
            return true;
        }

        self.gitignore
            .matched_path_or_any_parents(path, path.is_dir())
            .is_ignore()
    }
}

fn find_git_root(start_path: &Path) -> Result<PathBuf> {
    let mut current_dir = start_path.canonicalize()?;
    if !current_dir.is_dir() {
        current_dir = current_dir
            .parent()
            .ok_or_else(|| anyhow!("No parent directory found"))?
            .to_path_buf();
    }

    loop {
        if current_dir.join(".git").is_dir() {
            return Ok(current_dir);
        }
        if !current_dir.pop() {
            return Err(anyhow!("Not in a git repository"));
        }
    }
}

fn load_gitignore(git_root: &Path) -> Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(git_root);
    let gitignore_path = git_root.join(".gitignore");
    if gitignore_path.exists() {
        builder.add(gitignore_path);
    }
    Ok(builder.build()?)
}

struct LanguageConfig {
    language: tree_sitter::Language,
    query: &'static str,
} 

fn get_imports(file_path: &Path, project_ctx: &ProjectContext) -> Result<Vec<PathBuf>> {
    let content = fs::read_to_string(file_path)?;
    let file_dir = file_path.parent().unwrap_or(Path::new(""));
    let extension = file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();

    let config = match extension.as_str() {
        "py" => Some(LanguageConfig {
            language: tree_sitter_python::language(),
            query: r#"
                (import_statement
                    name: (dotted_name) @import)
                (import_from_statement
                    module_name: (dotted_name) @import)
            "#,
        }),
        "js" | "ts" | "jsx" | "tsx" => Some(LanguageConfig {
            language: tree_sitter_typescript::language(),
            query: r#"
                (import_statement
                    source: (string) @import)
                (call_expression
                    function: (identifier) @function
                    arguments: (arguments (string) @import)
                    (#eq? @function "require"))
            "#,
        }),
        _ => None,
    };

    let Some(config) = config else {
        return Ok(vec![]);
    };

    let mut parser = Parser::new();
    parser.set_language(config.language)?;

    let tree = parser.parse(&content, None)
        .ok_or_else(|| anyhow!("Failed to parse file"))?;

    let query = Query::new(config.language, config.query)?;
    let mut cursor = QueryCursor::new();
    let matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

    let imports = matches
        .filter_map(|m| {
            let capture = m.captures[0];
            let import_text = capture.node.utf8_text(content.as_bytes()).ok()?;
            
            // Clean up the import text (remove quotes, etc)
            let clean_import = import_text.trim_matches(|c| c == '"' || c == '\'' || c == '`');
            
            match extension.as_str() {
                "py" => Some(resolve_python_import(clean_import, file_dir, &project_ctx.git_root)),
                "js" | "ts" | "jsx" | "tsx" => {
                    project_ctx.path_aliases
                        .resolve_path(clean_import, file_dir)
                        .or_else(|| Some(resolve_js_import(clean_import, file_dir)))
                }
                _ => None,
            }
        })
        .flatten()
        .collect::<Vec<_>>();

    Ok(imports.into_iter().filter(|path| path.exists()).collect())
}

fn resolve_python_import(import: &str, file_dir: &Path, git_root: &Path) -> Option<PathBuf> {
    if import.starts_with('.') {
        Some(file_dir.join(import.replace('.', "/")).with_extension("py"))
    } else {
        Some(git_root.join(import.replace('.', "/")).with_extension("py"))
    }
}

fn resolve_js_import(import: &str, file_dir: &Path) -> PathBuf {
    let base_path = file_dir.join(import);
    
    // Return the base path - the existence check in get_imports will handle
    // checking various extensions and index files
    base_path
}

fn copy_to_clipboard<P: AsRef<Path>>(paths: &[P]) -> Result<()> {
    let mut all_contents = String::new();

    for path in paths {
        let path = path.as_ref();
        let relative_path = path
            .strip_prefix(env::current_dir()?)
            .unwrap_or(path)
            .display();

        all_contents.push_str(&format!("\n<file>{}</file>\n", relative_path));
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;
        all_contents.push_str(&content);
        all_contents.push_str("\n");
    }

    let mut pbcopy = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .context("Failed to start pbcopy")?;

    if let Some(mut stdin) = pbcopy.stdin.take() {
        use std::io::Write;
        stdin.write_all(all_contents.as_bytes())?;
    }

    pbcopy.wait()?;

    Ok(())
}

fn process_file(
    file_path: &Path,
    project_ctx: &ProjectContext,
    processed: &mut HashSet<PathBuf>,
) -> Result<()> {
    let canonical_path = file_path.canonicalize()?;

    if processed.contains(&canonical_path) || project_ctx.is_ignored(file_path) {
        return Ok(());
    }

    processed.insert(canonical_path);

    for import in get_imports(file_path, project_ctx)? {
        process_file(&import, project_ctx, processed)?;
    }

    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        return Err(anyhow!("Usage: {} <file>", args[0]));
    }

    let input_file = PathBuf::from(&args[1]);
    if !input_file.exists() {
        return Err(anyhow!("File not found: {}", input_file.display()));
    }

    let project_ctx = ProjectContext::new(&input_file)?;
    let mut processed_files = HashSet::new();

    process_file(&input_file, &project_ctx, &mut processed_files)?;

    println!("\nFiles to be copied:");
    for file in &processed_files {
        println!("- {}", file.display());
    }
    println!();

    copy_to_clipboard(&processed_files.into_iter().collect::<Vec<_>>())?;
    println!("File and dependencies copied to clipboard");

    Ok(())
}

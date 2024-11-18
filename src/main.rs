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

fn get_imports(file_path: &Path, project_ctx: &ProjectContext) -> Result<Vec<PathBuf>> {
    let content = fs::read_to_string(file_path)?;
    let file_dir = file_path.parent().unwrap_or(Path::new(""));
    let extension = file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();

    let imports = match extension.as_str() {
        "py" => get_python_imports(&content, file_dir, &project_ctx.git_root),
        "js" | "ts" | "jsx" | "tsx" => get_js_imports(&content, file_dir, project_ctx),
        _ => vec![],
    };

    Ok(imports
        .into_iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>())
}

fn get_python_imports(content: &str, file_dir: &Path, git_root: &Path) -> Vec<PathBuf> {
    let import_re = Regex::new(r#"^(?:from\s+(?P<from>\.{0,2}[^.\s]+(?:\.[^.\s]+)*)\s+import|import\s+(?P<import>[^.\s]+(?:\.[^.\s]+)*))"#).unwrap();

    content
        .lines()
        .filter_map(|line| import_re.captures(line.trim()))
        .filter_map(|caps| {
            caps.name("from")
                .or_else(|| caps.name("import"))
                .map(|m| m.as_str().to_string())
        })
        .map(|module| {
            if module.starts_with('.') {
                let module_path = module.replace('.', "/");
                file_dir.join(module_path).with_extension("py")
            } else {
                git_root.join(module.replace('.', "/")).with_extension("py")
            }
        })
        .collect()
}

fn get_js_imports(content: &str, file_dir: &Path, project_ctx: &ProjectContext) -> Vec<PathBuf> {
    let import_re = Regex::new(r#"(?:import.*from\s+['"]|require\(['"])([^'"]+)['"]"#).unwrap();

    content
        .lines()
        .filter_map(|line| {
            import_re
                .captures(line.trim())
                .and_then(|caps| caps.get(1))
                .map(|m| m.as_str().to_string())
        })
        .flat_map(|module| {
            let base_path = project_ctx
                .path_aliases
                .resolve_path(&module, file_dir)
                .unwrap_or_else(|| file_dir.join(&module));

            let mut paths = vec![];

            for ext in &[".js", ".jsx", ".ts", ".tsx"] {
                paths.push(base_path.with_extension(&ext[1..]));
            }

            if base_path.is_dir() {
                for ext in &[".js", ".jsx", ".ts", ".tsx"] {
                    paths.push(base_path.join(format!("index{}", ext)));
                }
            }

            paths
        })
        .collect()
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

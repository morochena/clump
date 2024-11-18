# Clump

A command-line tool that copies a source file and all its application-level dependencies to the clipboard. Currently supports Python and JavaScript/TypeScript files.

## Features

- Recursively finds and copies all imported dependencies
- Supports Python import statements (`import` and `from ... import`)
- Supports JavaScript/TypeScript imports (ES6 imports and require statements) 
- Respects `.gitignore` rules
- Supports path aliases (currently `@` alias for git root)
- Maintains project structure in copied output

## Supported File Types

| Language | Extensions |
|----------|------------|
| Python | `.py` |
| JavaScript | `.js`, `.jsx` |
| TypeScript | `.ts`, `.tsx` |

## Usage

```bash
clump path/to/your/file.py
```

## Limitations
- Currently only supports macOS clipboard operations
- Path resolution is based on Git repository root
- Limited to Python and JavaScript/TypeScript file types

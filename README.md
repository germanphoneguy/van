# Van
Van (formerly VimnANo) is a simple terminal code editor with vim-like commands.

## Installation

Requires curl (for AI features) and cargo.

```bash
cargo install van-editor
```

Or build from source:
```bash
git clone https://github.com/germanphoneguy/van
cd van
cargo install --path .
```

## Usage

```bash
van [FILENAME]
```

## Controls

| Key | Action |
|-----|--------|
| `Ctrl+S` | Save |
| `Ctrl+F` | Find |
| `Ctrl+Z` | Undo |
| `Ctrl+X` | Exit |
| `Esc` | Command mode |

## Commands (after `Esc`)

| Command | Action |
|---------|--------|
| `:w` | Save |
| `:q` | Quit (if clean) |
| `:q!` | Quit without saving |
| `:wq` / `:wq!` | Save and quit |
| `:N` | Jump to line N |
| `:chmod` | Make `.sh` file executable |
| `:syntax on/off` | Toggle syntax highlighting |
| `:!cmd` | Run shell command |
| `:ai ...` | Ask Groq AI |

## Features

- Syntax highlighting for Rust, Python, C, Shell scripts
- Language-aware auto-indentation
- Search with highlighting
- Undo support
- Groq AI integration (`:ai`)

use crossterm::style::Color;
use std::path::PathBuf;
use crate::config::SyntaxColors;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Language {
    PlainText,
    Python,
    C,
    Rust,
    Shell,
    Markdown,
}

pub fn detect_language(filename: &str) -> Language {
    let ext = PathBuf::from(filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "py" => Language::Python,
        "rs" => Language::Rust,
        "c" | "h" | "cpp" | "hpp" => Language::C,
        "sh" | "bash" => Language::Shell,
        "md" | "markdown" => Language::Markdown,
        _ => Language::PlainText,
    }
}

fn rust_keywords() -> &'static [&'static str] {
    &["as", "async", "await", "break", "const", "continue", "crate",
      "dyn", "else", "enum", "extern", "false", "fn", "for", "if",
      "impl", "in", "let", "loop", "match", "mod", "move", "mut",
      "pub", "ref", "return", "self", "Self", "static", "struct",
      "super", "trait", "true", "type", "unsafe", "use", "where",
      "while", "abstract", "become", "box", "do", "final", "macro",
      "override", "priv", "typeof", "unsized", "virtual", "yield"]
}

fn c_keywords() -> &'static [&'static str] {
    &["auto", "break", "case", "const", "continue", "default", "do",
      "else", "enum", "extern", "for", "goto", "if", "register",
      "return", "signed", "sizeof", "static", "struct", "switch",
      "typedef", "union", "unsigned", "volatile", "while", "void",
      "int", "char", "float", "double", "long", "short"]
}

fn python_keywords() -> &'static [&'static str] {
    &["False", "None", "True", "and", "as", "assert", "async",
      "await", "break", "class", "continue", "def", "del", "elif",
      "else", "except", "finally", "for", "from", "global", "if",
      "import", "in", "is", "lambda", "nonlocal", "not", "or",
      "pass", "raise", "return", "try", "while", "with", "yield"]
}

fn shell_keywords() -> &'static [&'static str] {
    &["if", "then", "else", "elif", "fi", "for", "while", "do",
      "done", "case", "esac", "in", "function", "return", "exit",
      "export", "local", "declare", "source", "select"]
}

fn shell_builtins() -> &'static [&'static str] {
    &["echo", "cd", "ls", "rm", "mv", "cp", "mkdir", "rmdir",
      "cat", "grep", "sed", "awk", "printf", "read", "test",
      "kill", "wait", "alias", "type", "command", "exec",
      "shift", "unset", "unalias", "times", "trap", "ulimit",
      "umask", "dirs", "pushd", "popd", "hash", "help",
      "bind", "builtin", "caller", "compgen", "complete",
      "coproc", "disown", "enable", "fc", "getopts", "history",
      "jobs", "let", "logout", "suspend", "shopt"]
}

fn python_builtins() -> &'static [&'static str] {
    &["print", "len", "range", "type", "int", "float", "str",
      "list", "dict", "set", "tuple", "bool", "bytes", "bytearray",
      "open", "input", "enumerate", "zip", "map", "filter",
      "sorted", "reversed", "super", "isinstance", "hasattr",
      "getattr", "setattr", "repr", "format", "min", "max",
      "sum", "any", "all", "abs", "round", "pow", "divmod",
      "hex", "oct", "bin", "ord", "chr", "callable", "classmethod",
      "staticmethod", "property", "delattr", "dir", "eval",
      "exec", "globals", "locals", "hash", "help", "id",
      "issubclass", "iter", "next", "object", "memoryview",
      "frozenset", "vars"]
}

fn rust_types() -> &'static [&'static str] {
    &["i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64",
      "u128", "isize", "usize", "f32", "f64", "bool", "char", "str",
      "String", "Vec", "HashMap", "Box", "Option", "Result", "Arc",
      "Rc", "Mutex", "RefCell", "Path", "PathBuf", "Duration"]
}

fn python_types() -> &'static [&'static str] {
    &["int", "float", "str", "bool", "list", "dict", "tuple", "set",
      "bytes", "bytearray", "NoneType", "type", "object"]
}

pub fn is_keyword(word: &str, lang: Language) -> bool {
    let keywords = match lang {
        Language::Rust => rust_keywords(),
        Language::C => c_keywords(),
        Language::Python => python_keywords(),
        Language::Shell => shell_keywords(),
        Language::PlainText | Language::Markdown => &[],
    };
    keywords.contains(&word)
}

pub fn is_type(word: &str, lang: Language) -> bool {
    let types = match lang {
        Language::Rust => rust_types(),
        Language::C => &[],
        Language::Python => python_types(),
        Language::Shell => &[],
        Language::PlainText | Language::Markdown => &[],
    };
    types.contains(&word)
}

fn is_builtin(word: &str, lang: Language) -> bool {
    let builtins = match lang {
        Language::Shell => shell_builtins(),
        Language::Python => python_builtins(),
        _ => &[],
    };
    builtins.contains(&word)
}

pub fn tokenize(line: &str, lang: Language, colors: &SyntaxColors) -> Vec<(String, Option<Color>)> {
    let mut out: Vec<(String, Option<Color>)> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let mut i = 0;

    let comment_start = match lang {
        Language::Python | Language::Shell => "#",
        Language::PlainText | Language::Markdown => "",
        _ => "//",
    };

    while i < len {
        // Language-specific comments (existing logic)
        if !comment_start.is_empty() && i + comment_start.len() <= len {
            let rest: String = chars[i..].iter().collect();
            if rest.starts_with(comment_start) {
                let text: String = chars[i..].iter().collect();
                out.push((sanitize_str(&text), colors.comment));
                break;
            }
        }
        if lang != Language::PlainText && lang != Language::Shell && lang != Language::Markdown
            && i + 1 < len && chars[i] == '/' && chars[i + 1] == '/'
        {
            let text: String = chars[i..].iter().collect();
            out.push((sanitize_str(&text), colors.comment));
            break;
        }

        // Markdown heading
        if lang == Language::Markdown && i == 0 && chars[i] == '#' {
            let mut j = i;
            while j < len && chars[j] == '#' { j += 1; }
            if j < len && chars[j] == ' ' {
                let text: String = chars[i..].iter().collect();
                out.push((sanitize_str(&text), colors.markdown_heading));
                break;
            }
        }

        // Markdown inline code (`code`)
        if lang == Language::Markdown && chars[i] == '`' {
            let rest: String = chars[i + 1..].iter().collect();
            if let Some(close) = rest.find('`') {
                let end = i + 1 + close + 1;
                let text: String = chars[i..=end].iter().collect();
                out.push((sanitize_str(&text), colors.markdown_code));
                i = end + 1;
                continue;
            }
        }

        // Markdown **bold**
        if lang == Language::Markdown && i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            let rest: String = chars[i + 2..].iter().collect();
            if let Some(close) = rest.find("**") {
                let end = i + 2 + close + 1;
                let text: String = chars[i..=end].iter().collect();
                out.push((sanitize_str(&text), colors.markdown_bold));
                i = end + 1;
                continue;
            }
        }

        // Markdown *italic*
        if lang == Language::Markdown && chars[i] == '*' {
            let rest: String = chars[i + 1..].iter().collect();
            if let Some(close) = rest.find('*') {
                let end = i + 1 + close;
                let text: String = chars[i..=end].iter().collect();
                out.push((sanitize_str(&text), colors.markdown_bold));
                i = end + 1;
                continue;
            }
        }

        // Markdown [text](url) link
        if lang == Language::Markdown && chars[i] == '[' {
            let rest: String = chars[i + 1..].iter().collect();
            if let Some(cb) = rest.find(']') {
                if cb + 1 < rest.len() && rest[cb + 1..].starts_with('(') {
                    let url_rest = &rest[cb + 2..];
                    if let Some(cp) = url_rest.find(')') {
                        let end = i + 1 + cb + 1 + 1 + cp;
                        let text: String = chars[i..=end].iter().collect();
                        out.push((sanitize_str(&text), colors.markdown_link));
                        i = end + 1;
                        continue;
                    }
                }
            }
        }

        // # comment in non-Markdown languages
        if lang != Language::Markdown && chars[i] == '#' {
            let text: String = chars[i..].iter().collect();
            out.push((sanitize_str(&text), colors.comment));
            break;
        }

        // Rust lifetime annotations
        if lang == Language::Rust && chars[i] == '\'' && i + 1 < len
            && chars[i + 1].is_alphabetic()
        {
            let mut s = String::new();
            s.push('\'');
            i += 1;
            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                s.push(chars[i]);
                i += 1;
            }
            out.push((s, colors.lifetime));
            continue;
        }

        // Strings
        if chars[i] == '"' || chars[i] == '\'' {
            let quote = chars[i];

            // Shell double-quoted: parse variables inside
            if lang == Language::Shell && quote == '"' {
                let mut acc = String::new();
                acc.push('"');
                i += 1;

                loop {
                    if i >= len { break; }
                    if chars[i] == '\\' && i + 1 < len {
                        acc.push(chars[i]); i += 1;
                        acc.push(chars[i]); i += 1;
                    } else if chars[i] == '$' {
                        if !acc.is_empty() {
                            out.push((sanitize_str(&acc), colors.string_double));
                            acc = String::new();
                        }
                        let mut var = String::new();
                        var.push('$');
                        i += 1;
                        if i < len && chars[i] == '{' {
                            var.push('{'); i += 1;
                            while i < len && chars[i] != '}' {
                                var.push(chars[i]); i += 1;
                            }
                            if i < len { var.push('}'); i += 1; }
                        } else if i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                                var.push(chars[i]); i += 1;
                            }
                        } else if i < len {
                            var.push(chars[i]); i += 1;
                        }
                        out.push((sanitize_str(&var), colors.variable));
                    } else if chars[i] == '"' {
                        acc.push('"'); i += 1;
                        break;
                    } else {
                        acc.push(chars[i]); i += 1;
                    }
                }

                if !acc.is_empty() {
                    out.push((sanitize_str(&acc), colors.string_double));
                }
                continue;
            }

            // All other strings: simple whole-string token
            let mut s = String::new();
            s.push(quote);
            i += 1;
            while i < len {
                s.push(chars[i]);
                if chars[i] == '\\' && i + 1 < len {
                    i += 1;
                    s.push(chars[i]);
                } else if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }

            let color = if quote == '"' { colors.string_double } else { colors.string_single };
            out.push((sanitize_str(&s), color));
            continue;
        }

        // Bash $ variable (outside strings)
        if lang == Language::Shell && chars[i] == '$' {
            i += 1;
            let mut s = String::new();
            s.push('$');
            if i < len && chars[i] == '(' {
                s.push('('); i += 1;
                let mut depth = 1;
                while i < len && depth > 0 {
                    if chars[i] == '(' { depth += 1; }
                    else if chars[i] == ')' { depth -= 1; }
                    if depth > 0 { s.push(chars[i]); }
                    i += 1;
                }
                s.push(')');
            } else if i < len && chars[i] == '{' {
                s.push('{'); i += 1;
                while i < len && chars[i] != '}' {
                    s.push(chars[i]); i += 1;
                }
                if i < len { s.push('}'); i += 1; }
            } else if i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    s.push(chars[i]); i += 1;
                }
            } else {
                out.push(("$".to_string(), None));
                continue;
            }
            out.push((sanitize_str(&s), colors.variable));
            continue;
        }

        // Python decorator
        if lang == Language::Python && chars[i] == '@' {
            let mut s = String::new();
            s.push('@');
            i += 1;
            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                s.push(chars[i]);
                i += 1;
            }
            out.push((s, colors.decorator));
            continue;
        }

        // Numbers
        if chars[i].is_ascii_digit()
            || (chars[i] == '.' && i + 1 < len && chars[i + 1].is_ascii_digit())
        {
            let mut s = String::new();
            while i < len
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                s.push(chars[i]);
                i += 1;
            }
            out.push((s, colors.number));
            continue;
        }

        // Identifiers / words
        if chars[i].is_alphabetic() || chars[i] == '_' {
            let mut s = String::new();
            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                s.push(chars[i]);
                i += 1;
            }
            let color = if is_keyword(&s, lang) {
                colors.keyword
            } else if is_type(&s, lang) {
                colors.type_name
            } else if is_builtin(&s, lang) {
                colors.builtin
            } else if (lang == Language::Python) && (s == "self" || s == "cls") {
                colors.builtin
            } else if (lang == Language::Python) && s.starts_with("__") && s.ends_with("__") && s.len() > 4 {
                colors.builtin
            } else {
                None
            };
            out.push((s, color));
            continue;
        }

        // Rust-specific: &'lifetime and 'lifetime after fallthrough
        let mut s = String::new();
        s.push(chars[i]);
        i += 1;

        if lang == Language::Rust {
            if s == "&" && i < len && chars[i] == '\'' {
                s.push(chars[i]);
                i += 1;
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    s.push(chars[i]);
                    i += 1;
                }
                out.push((s, colors.lifetime));
                continue;
            }
            if s == "'" && i < len && chars[i].is_alphabetic() {
                s.push(chars[i]);
                i += 1;
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    s.push(chars[i]);
                    i += 1;
                }
                out.push((s, colors.lifetime));
                continue;
            }
        }

        out.push((s, None));
    }

    out
}

pub fn sanitize_str(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            if c == '\x1b' {
                Some('␛')
            } else if c.is_control() && !['\t', '\n', '\r'].contains(&c) {
                None
            } else {
                Some(c)
            }
        })
        .collect()
}

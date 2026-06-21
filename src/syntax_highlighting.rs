use crossterm::style::Color;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Language {
    PlainText,
    Python,
    C,
    Rust,
    Shell,
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
        "sh" => Language::Shell,
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
        Language::PlainText => &[],
    };
    keywords.contains(&word)
}

pub fn is_type(word: &str, lang: Language) -> bool {
    let types = match lang {
        Language::Rust => rust_types(),
        Language::C => &[],
        Language::Python => python_types(),
        Language::Shell => &[],
        Language::PlainText => &[],
    };
    types.contains(&word)
}

pub fn tokenize(line: &str, lang: Language) -> Vec<(String, Option<Color>)> {
    let mut out: Vec<(String, Option<Color>)> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let mut i = 0;

    let comment_start = match lang {
        Language::Python | Language::Shell => "#",
        Language::PlainText => "",
        _ => "//",
    };

    while i < len {
        if !comment_start.is_empty() && i + comment_start.len() <= len {
            let rest: String = chars[i..].iter().collect();
            if rest.starts_with(comment_start) {
                let text: String = chars[i..].iter().collect();
                out.push((sanitize_str(&text), Some(Color::DarkGreen)));
                break;
            }
        }
        if lang != Language::PlainText && lang != Language::Shell && i + 1 < len && chars[i] == '/' && chars[i + 1] == '/' {
            let text: String = chars[i..].iter().collect();
            out.push((sanitize_str(&text), Some(Color::DarkGreen)));
            break;
        }

        if lang == Language::Rust && chars[i] == '\'' && i + 1 < len && chars[i + 1].is_alphabetic() {
            let mut s = String::new();
            s.push('\'');
            i += 1;
            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                s.push(chars[i]);
                i += 1;
            }
            out.push((s, Some(Color::Cyan)));
            continue;
        }

        if chars[i] == '"' || chars[i] == '\'' {
            let quote = chars[i];
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
            out.push((sanitize_str(&s), Some(Color::Green)));
            continue;
        }

        if chars[i].is_ascii_digit() || (chars[i] == '.' && i + 1 < len && chars[i + 1].is_ascii_digit()) {
            let mut s = String::new();
            while i < len && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_') {
                s.push(chars[i]);
                i += 1;
            }
            out.push((s, Some(Color::Magenta)));
            continue;
        }

        if chars[i].is_alphabetic() || chars[i] == '_' {
            let mut s = String::new();
            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                s.push(chars[i]);
                i += 1;
            }
            let color = if is_keyword(&s, lang) {
                Some(Color::Blue)
            } else if is_type(&s, lang) {
                Some(Color::Cyan)
            } else {
                None
            };
            out.push((s, color));
            continue;
        }

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
                out.push((s, Some(Color::Cyan)));
                continue;
            }
            if s == "'" && i < len && chars[i].is_alphabetic() {
                s.push(chars[i]);
                i += 1;
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    s.push(chars[i]);
                    i += 1;
                }
                out.push((s, Some(Color::Cyan)));
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
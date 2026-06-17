use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};
use std::{
    cmp,
    collections::HashMap,
    env,
    fs,
    io::{self, stdout, Stdout, Write},
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
use memmap2::Mmap;
use serde::{Deserialize, Serialize};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SEARCH_STATUS_SECONDS: u64 = 5;
const MESSAGE_STATUS_SECONDS: u64 = 3;
const AI_STATUS_SECONDS: u64 = 9;
const POLL_FALLBACK_MS: u64 = 250;
const INDENT_WIDTH: usize = 4;

const PROVIDER_INFO: &[(&str, &str, &[&str])] = &[
    ("groq", "https://api.groq.com/openai/v1/chat/completions",
     &["llama-3.3-70b-versatile", "mixtral-8x7b-32768", "gemma2-9b-it", "llama-3.1-8b-instant"]),
    ("openai", "https://api.openai.com/v1/chat/completions",
     &["gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "gpt-3.5-turbo"]),
    ("anthropic", "https://api.anthropic.com/v1/messages",
     &["claude-3-5-sonnet-20241022", "claude-3-opus-20240229", "claude-3-haiku-20240307"]),
    ("gemini", "https://generativelanguage.googleapis.com/v1beta/models/",
     &["gemini-1.5-flash", "gemini-1.5-pro", "gemini-2.0-flash-exp"]),
    ("openrouter", "https://openrouter.ai/api/v1/chat/completions",
     &["openai/gpt-4o"]),
    ("opencode-zen", "https://opencode.ai/zen/v1/chat/completions",
     &["big-pickle", "deepseek-v4-flash-free", "gpt-5.4", "gpt-5.4-mini", "claude-sonnet-4"]),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AiConfig {
    provider: String,
    models: HashMap<String, String>,
    anthropic_version: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    local_keys: HashMap<String, String>,
}

impl Default for AiConfig {
    fn default() -> Self {
        let mut models = HashMap::new();
        for (name, _, model_list) in PROVIDER_INFO {
            if let Some(m) = model_list.first() {
                models.insert(name.to_string(), m.to_string());
            }
        }
        Self { provider: "groq".to_string(), models, anthropic_version: "2023-06-01".to_string(), local_keys: HashMap::new() }
    }
}

impl AiConfig {
    fn save(&self) -> io::Result<()> {
        let path = ai_config_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no config path"))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, serde_json::to_string_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    fn endpoint(&self) -> &str {
        PROVIDER_INFO.iter()
            .find(|(n, _, _)| *n == self.provider)
            .map(|(_, e, _)| *e)
            .unwrap_or("https://api.groq.com/openai/v1/chat/completions")
    }

    fn active_model(&self) -> String {
        self.models.get(&self.provider)
            .cloned()
            .or_else(|| PROVIDER_INFO.iter()
                .find(|(n, _, m)| *n == self.provider && !m.is_empty())
                .and_then(|(_, _, m)| m.first().map(|s| s.to_string())))
            .unwrap_or_else(|| "llama-3.3-70b-versatile".to_string())
    }
}

fn ai_config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("van").join("ai_config.json"))
}

fn config_dir() -> Option<PathBuf> {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(xdg))
    } else if let Some(home) = env::var_os("HOME") {
        Some(PathBuf::from(home).join(".config"))
    } else {
        env::var_os("USERPROFILE").map(PathBuf::from)
    }
}

fn load_ai_config() -> AiConfig {
    let path = match ai_config_path() {
        Some(p) => p,
        None => return AiConfig::default(),
    };
    let mut migrated = false;

    match fs::read_to_string(&path) {
        Ok(c) => {
            let raw: serde_json::Value = serde_json::from_str(&c).unwrap_or_default();
            let mut config: AiConfig = serde_json::from_value(raw.clone()).unwrap_or_default();

            if let Some(keys) = raw.get("api_keys").and_then(|k| k.as_object()) {
                for (prov, key) in keys {
                    if let Some(k) = key.as_str() {
                        if !k.is_empty() && !config.local_keys.contains_key(prov) {
                            migrate_key_to_keyring(prov, k);
                            config.local_keys.remove(prov);
                            migrated = true;
                        }
                    }
                }
            }

            let old_groq = load_old_groq_key();
            if let Some(k) = old_groq {
                if !config.local_keys.contains_key("groq") {
                    migrate_key_to_keyring("groq", &k);
                    migrated = true;
                }
                let _ = fs::remove_file(config_dir().map(|d| d.join("van_groq_api_key")).unwrap());
            }

            if migrated {
                let _ = config.save();
            }
            config
        }
        Err(_) => {
            let c = AiConfig::default();
            let _ = c.save();
            c
        }
    }
}

fn migrate_key_to_keyring(provider: &str, key: &str) {
    if let Ok(entry) = keyring::Entry::new("van-editor", provider) {
        let _ = entry.set_password(key);
    }
}

fn load_old_groq_key() -> Option<String> {
    let base = config_dir()?;
    let path = base.join("van_groq_api_key");
    let key = fs::read_to_string(path).ok()?;
    let trimmed = key.trim().to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Language {
    PlainText,
    Python,
    C,
    Rust,
    Shell, // Added Shell variant
}

fn detect_language(filename: &str) -> Language {
    let ext = PathBuf::from(filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "py" => Language::Python,
        "rs" => Language::Rust,
        "c" | "h" | "cpp" | "hpp" => Language::C,
        "sh" => Language::Shell, // Detect .sh files
        _ => Language::PlainText,
    }
}

fn is_keyword(word: &str, lang: Language) -> bool {
    match lang {
        Language::Rust => matches!(word,
            "as" | "async" | "await" | "break" | "const" | "continue" | "crate"
            | "dyn" | "else" | "enum" | "extern" | "false" | "fn" | "for" | "if"
            | "impl" | "in" | "let" | "loop" | "match" | "mod" | "move" | "mut"
            | "pub" | "ref" | "return" | "self" | "Self" | "static" | "struct"
            | "super" | "trait" | "true" | "type" | "unsafe" | "use" | "where"
            | "while" | "abstract" | "become" | "box" | "do" | "final" | "macro"
            | "override" | "priv" | "typeof" | "unsized" | "virtual" | "yield"),
        Language::C => matches!(word,
            "auto" | "break" | "case" | "const" | "continue" | "default" | "do"
            | "else" | "enum" | "extern" | "for" | "goto" | "if" | "register"
            | "return" | "signed" | "sizeof" | "static" | "struct" | "switch"
            | "typedef" | "union" | "unsigned" | "volatile" | "while" | "void"
            | "int" | "char" | "float" | "double" | "long" | "short"),
        Language::Python => matches!(word,
            "False" | "None" | "True" | "and" | "as" | "assert" | "async"
            | "await" | "break" | "class" | "continue" | "def" | "del" | "elif"
            | "else" | "except" | "finally" | "for" | "from" | "global" | "if"
            | "import" | "in" | "is" | "lambda" | "nonlocal" | "not" | "or"
            | "pass" | "raise" | "return" | "try" | "while" | "with" | "yield"),
        Language::Shell => matches!(word,
            "if" | "then" | "else" | "elif" | "fi" | "for" | "while" | "do"
            | "done" | "case" | "esac" | "in" | "function" | "return" | "exit"
            | "export" | "local" | "declare" | "source" | "select"),
        Language::PlainText => false,
    }
}

fn is_type(word: &str, lang: Language) -> bool {
    match lang {
        Language::Rust => matches!(word,
            "i8" | "i16" | "i32" | "i64" | "i128" | "u8" | "u16" | "u32" | "u64"
            | "u128" | "isize" | "usize" | "f32" | "f64" | "bool" | "char" | "str"
            | "String" | "Vec" | "HashMap" | "Box" | "Option" | "Result" | "Arc"
            | "Rc" | "Mutex" | "RefCell" | "Path" | "PathBuf" | "Duration"),
        Language::C => false,
        Language::Python => matches!(word,
            "int" | "float" | "str" | "bool" | "list" | "dict" | "tuple" | "set"
            | "bytes" | "bytearray" | "NoneType" | "type" | "object"),
        Language::Shell => false,
        Language::PlainText => false,
    }
}

fn tokenize(line: &str, lang: Language) -> Vec<(String, Option<Color>)> {
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

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() > 1 {
        match args[1].as_str() {
            "--version" | "-v" => {
                println!(r#"__      __
\ \    / /  
 \ \  / /_ _ _ __  
  \ \/ / _` | '_ \ 
   \  / (_| | | | |
    \/ \__,_|_| |_|"#);

                println!("van editor version {}", VERSION);
                return Ok(());
            }
            "--help" | "-h" => {
                println!("van editor - a lightweight rust text editor");
                println!("\nUsage: van [FILENAME]");
                println!("\nControls:");
                println!("  Ctrl+S : Save");
                println!("  Ctrl+F : Find");
                println!("  Ctrl+Z : Undo");
                println!("  Ctrl+X : Exit");
                println!("  Esc    : Toggle command mode");
                println!("\nCommand mode:");
                println!("  :w      Save");
                println!("  :q      Quit if clean");
                println!("  :q!     Quit without saving");
                println!("  :wq     Save and quit");
                println!("  :wq!    Save and quit");
                println!("  :line   Jump to line");
                println!("  :chmod  Make .sh file executable");
                println!("  :syntax on/off  Toggle syntax highlighting");
                println!("  :!cmd   Run shell command");
                println!("  :ai <prompt>   Ask AI (Groq/OpenAI/Anthropic/Gemini/OpenRouter/OpenCode Zen)");
                println!("  :ai -l N-M <prompt>  Ask AI about specific lines (1-indexed)");
                println!("  :ai --config   Open AI config TUI");
                return Ok(());
            }
            _ => {}
        }
    }

    let filename = if args.len() > 1 {
        args[1].clone()
    } else {
        "untitled.txt".to_string()
    };

    let mut out = stdout();
    let _guard = TerminalGuard::enter(&mut out)?;

    let mut editor = Editor::open(filename);
    editor.render(&mut out)?;

    loop {
        let timeout = editor.poll_timeout();
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if editor.handle_key(key) {
                        break;
                    }
                }
                Event::Resize(_, _) => {
                    editor.request_full_redraw();
                }
                Event::Paste(text) => {
                    editor.handle_paste_event(text);
                }
                _ => {}
            }
        }

        if editor.tick() {
            editor.request_redraw();
        }

        if editor.needs_redraw() {
            editor.render(&mut out)?;
        }
    }

    Ok(())
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter(out: &mut Stdout) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(out, terminal::EnterAlternateScreen, event::EnableBracketedPaste)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        let _ = execute!(out, event::DisableBracketedPaste, cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

#[derive(Clone)]
enum UndoAction {
    InsertChar {
        y: usize,
        x: usize,
        ch: char,
    },
    DeleteChar {
        y: usize,
        x: usize,
        ch: char,
    },
    InsertNewline {
        y: usize,
        x: usize,
        right: String,
    },
    JoinLines {
        y: usize,
        x: usize,
        removed: String,
    },
    PasteBlock {
        saved_lines: Vec<String>,
        saved_cursor_y: usize,
        saved_cursor_x: usize,
        saved_dirty: bool,
    },
}

#[derive(Clone)]
struct UndoEntry {
    action: UndoAction,
    cursor_x: usize,
    cursor_y: usize,
    offset_x: usize,
    offset_y: usize,
    dirty: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Insert,
    Command,
    AwaitAiKey,
    AiConfig,
}

#[derive(Clone)]
enum LineSource {
    Base(usize),
    Overlay(String),
}

struct FileBuffer {
    mmap: Option<Mmap>,
    base_line_starts: Vec<usize>,
    base_line_ends: Vec<usize>,
    lines: Vec<LineSource>,
    dirty: bool,
}

impl FileBuffer {
    fn load(path: &str) -> Self {
        let file = match fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return Self::new_empty(),
        };
        let mmap = match unsafe { Mmap::map(&file) } {
            Ok(m) => m,
            Err(_) => return Self::new_empty(),
        };
        let mut line_starts = Vec::new();
        let mut line_ends = Vec::new();
        line_starts.push(0);
        for i in 0..mmap.len() {
            if mmap[i] == b'\n' {
                let end = if i > 0 && mmap[i - 1] == b'\r' { i - 1 } else { i };
                line_ends.push(end);
                line_starts.push(i + 1);
            }
        }
        line_ends.push(mmap.len());
        let count = line_starts.len();
        let lines: Vec<LineSource> = (0..count).map(LineSource::Base).collect();
        Self {
            mmap: Some(mmap),
            base_line_starts: line_starts,
            base_line_ends: line_ends,
            lines,
            dirty: false,
        }
    }

    fn new_empty() -> Self {
        Self {
            mmap: None,
            base_line_starts: Vec::new(),
            base_line_ends: Vec::new(),
            lines: vec![LineSource::Overlay(String::new())],
            dirty: false,
        }
    }

    fn len(&self) -> usize {
        self.lines.len()
    }

    fn char_len(&self, n: usize) -> usize {
        self.get_line(n).chars().count()
    }

    fn is_last_empty(&self) -> bool {
        self.lines.last().map_or(true, |l| match l {
            LineSource::Base(idx) => self.base_line_ends[*idx] == self.base_line_starts[*idx],
            LineSource::Overlay(s) => s.is_empty(),
        })
    }

    fn get_line(&self, n: usize) -> &str {
        match &self.lines[n] {
            LineSource::Overlay(s) => s.as_str(),
            LineSource::Base(idx) => {
                let mmap = self.mmap.as_ref().expect("mmap missing for base line");
                let start = self.base_line_starts[*idx];
                let end = self.base_line_ends[*idx];
                unsafe { std::str::from_utf8_unchecked(&mmap[start..end]) }
            }
        }
    }

    fn get_line_mut(&mut self, n: usize) -> &mut String {
        if matches!(self.lines[n], LineSource::Base(_)) {
            let idx = match &self.lines[n] {
                LineSource::Base(i) => *i,
                _ => unreachable!(),
            };
            let mmap = self.mmap.as_ref().expect("mmap missing for base line");
            let start = self.base_line_starts[idx];
            let end = self.base_line_ends[idx];
            let text = unsafe { String::from_utf8_unchecked(mmap[start..end].to_vec()) };
            self.lines[n] = LineSource::Overlay(text);
            self.dirty = true;
        }
        match &mut self.lines[n] {
            LineSource::Overlay(s) => s,
            _ => unreachable!(),
        }
    }

    fn push(&mut self, s: String) {
        self.lines.push(LineSource::Overlay(s));
    }

    fn insert(&mut self, n: usize, s: String) {
        self.lines.insert(n, LineSource::Overlay(s));
    }

    fn remove(&mut self, n: usize) -> String {
        match self.lines.remove(n) {
            LineSource::Overlay(s) => s,
            LineSource::Base(idx) => {
                let mmap = self.mmap.as_ref().expect("mmap missing for base line");
                let start = self.base_line_starts[idx];
                let end = self.base_line_ends[idx];
                unsafe { String::from_utf8_unchecked(mmap[start..end].to_vec()) }
            }
        }
    }

    fn to_file_string(&self) -> String {
        let mut out = String::new();
        for i in 0..self.lines.len() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(self.get_line(i));
        }
        out
    }

    fn clone_all(&self) -> Vec<String> {
        (0..self.lines.len()).map(|i| self.get_line(i).to_string()).collect()
    }

    fn restore_from_snapshot(&mut self, snapshot: Vec<String>, dirty: bool) {
        self.lines = snapshot.into_iter().map(LineSource::Overlay).collect();
        self.dirty = dirty;
    }
}

struct Editor {
    language: Language,
    filename: String,
    buffer: FileBuffer,

    cursor_x: usize,
    cursor_y: usize,
    offset_x: usize,
    offset_y: usize,

    search_input: String,
    search_highlight: String,
    in_search: bool,

    confirm_exit: bool,

    mode: InputMode,
    command_buffer: String,

    ai_config: AiConfig,
    config_key_buffer: String,
    pending_ai_request: Option<String>,
    pending_ai_line_range: Option<(usize, usize)>,
    ai_config_field: usize,
    ai_config_editing: bool,

    temp_status: Option<(String, Instant)>,

    undo_stack: Vec<UndoEntry>,

    needs_redraw: bool,
    force_full_redraw: bool,

    last_rendered_rows: Vec<String>,
    last_size: (u16, u16),

    ai_output: Option<Vec<String>>,
    ai_scroll: usize,

    syntax_highlight: bool,
}

impl Editor {
    fn open(filename: String) -> Self {
        let language = detect_language(&filename);
        let buffer = FileBuffer::load(&filename);

        Self {
            language,
            filename,
            buffer,
            cursor_x: 0,
            cursor_y: 0,
            offset_x: 0,
            offset_y: 0,
            search_input: String::new(),
            search_highlight: String::new(),
            in_search: false,
            confirm_exit: false,
            mode: InputMode::Insert,
            command_buffer: String::new(),
            ai_config: load_ai_config(),
            config_key_buffer: String::new(),
            pending_ai_request: None,
            pending_ai_line_range: None,
            ai_config_field: 0,
            ai_config_editing: false,
            temp_status: None,
            undo_stack: Vec::new(),
            needs_redraw: true,
            force_full_redraw: true,
            last_rendered_rows: Vec::new(),
            last_size: (0, 0),

            ai_output: None,
            ai_scroll: 0,
            syntax_highlight: true,
        } 
    }

    fn request_redraw(&mut self) {
        self.needs_redraw = true;
    }

    fn request_full_redraw(&mut self) {
        self.needs_redraw = true;
        self.force_full_redraw = true;
    }

    fn needs_redraw(&self) -> bool {
        self.needs_redraw
    }

    fn poll_timeout(&self) -> Duration {
        if let Some((_, until)) = &self.temp_status {
            let now = Instant::now();
            if *until > now {
                return until
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(POLL_FALLBACK_MS));
            }
        }
        Duration::from_millis(POLL_FALLBACK_MS)
    }

    fn tick(&mut self) -> bool {
        if let Some((_, until)) = &self.temp_status {
            if Instant::now() >= *until {
                self.temp_status = None;
                return true;
            }
        }
        false
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.ai_output.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.ai_output = None;
                    self.request_full_redraw();
                }
                KeyCode::Up => {
                    if self.ai_scroll > 0 {
                        self.ai_scroll -= 1;
                        self.request_redraw();
                    }
                }
                KeyCode::Down => {
                    let max_scroll = self.ai_output
                        .as_ref()
                        .map(|lines| lines.len().saturating_sub(1))
                        .unwrap_or(0);

                    if self.ai_scroll < max_scroll {
                        self.ai_scroll += 1;
                        self.request_redraw();
                    }
                }
                _ => {}
            }
            return false;
        }

        if self.confirm_exit {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return true,
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    if self.save().is_ok() {
                        return true;
                    }
                    self.set_temp_status("Save failed".to_string(), MESSAGE_STATUS_SECONDS);
                    self.request_redraw();
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm_exit = false;
                    self.set_temp_status("Exit cancelled".to_string(), MESSAGE_STATUS_SECONDS);
                    self.request_redraw();
                }
                _ => {}
            }
            return false;
        }

        if self.in_search {
            match key.code {
                KeyCode::Enter => {
                    let query = self.search_input.clone();
                    self.in_search = false;

                    if query.is_empty() {
                        self.search_input.clear();
                        self.request_redraw();
                        return false;
                    }

                    self.search_highlight = query.clone();

                    if let Some((y, x)) = self.find_first(&query) {
                        self.cursor_y = y;
                        self.cursor_x = x;
                        self.set_temp_status(format!("Found '{}'", query), SEARCH_STATUS_SECONDS);
                    } else {
                        self.set_temp_status(format!("'{}' not found", query), SEARCH_STATUS_SECONDS);
                    }

                    self.request_full_redraw();
                }
                KeyCode::Esc => {
                    self.in_search = false;
                    self.search_input.clear();
                    self.set_temp_status("Find cancelled".to_string(), MESSAGE_STATUS_SECONDS);
                    self.request_redraw();
                }
                KeyCode::Backspace => {
                    self.search_input.pop();
                    self.request_redraw();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.search_input.push(c);
                    self.request_redraw();
                }
                _ => {}
            }
            return false;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('x') => {
                    if !self.buffer.dirty {
                        return true;
                    }
                    self.confirm_exit = true;
                    self.request_redraw();
                }

                KeyCode::Char('s') => {
                    if self.save().is_ok() {
                        self.set_temp_status(format!("SAVED: {}", self.filename), MESSAGE_STATUS_SECONDS);
                    } else {
                        self.set_temp_status(format!("Save failed: {}", self.filename), MESSAGE_STATUS_SECONDS);
                    }
                    self.request_redraw();
                }

                KeyCode::Char('f') => {
                    self.in_search = true;
                    if self.search_highlight.is_empty() {
                        self.search_input.clear();
                    } else {
                        self.search_input = self.search_highlight.clone();
                    }
                    self.request_redraw();
                }

                KeyCode::Char('z') => {
                    if self.undo() {
                        self.set_temp_status("Undid last edit".to_string(), MESSAGE_STATUS_SECONDS);
                    } else {
                        self.set_temp_status("Nothing to undo".to_string(), MESSAGE_STATUS_SECONDS);
                    }
                    self.request_full_redraw();
                }

                _ => {}
            }
        }

        match self.mode {
            InputMode::AwaitAiKey => {
                match key.code {
                    KeyCode::Esc => {
                        self.mode = InputMode::Insert;
                        self.config_key_buffer.clear();
                        self.pending_ai_request = None;
                        self.pending_ai_line_range = None;
                        self.set_temp_status("API key entry cancelled".to_string(), MESSAGE_STATUS_SECONDS);
                        self.request_redraw();
                    }
                    KeyCode::Enter => {
                        let key_value = self.config_key_buffer.trim().to_string();
                        if key_value.is_empty() {
                            self.set_temp_status("API key cannot be empty".to_string(), MESSAGE_STATUS_SECONDS);
                            self.request_redraw();
                            return false;
                        }

                        let prov = self.ai_config.provider.clone();
                        if self.save_api_key(&prov, &key_value).is_ok() {
                            self.mode = InputMode::Insert;
                            self.config_key_buffer.clear();
                            self.set_temp_status(format!("{} API key saved", prov), MESSAGE_STATUS_SECONDS);
                            self.request_redraw();

                            if let Some(req) = self.pending_ai_request.take() {
                                let range = self.pending_ai_line_range.take();
                                self.run_ai_command(req, range);
                            }
                        } else {
                            self.set_temp_status("Failed to save API key".to_string(), MESSAGE_STATUS_SECONDS);
                            self.request_redraw();
                        }
                    }
                    KeyCode::Backspace => {
                        self.config_key_buffer.pop();
                        self.request_redraw();
                    }
                    KeyCode::Char(c) => {
                        self.config_key_buffer.push(c);
                        self.request_redraw();
                    }
                    _ => {}
                }
                return false;
            }

            InputMode::Command => {
                match key.code {
                    KeyCode::Esc => {
                        self.mode = InputMode::Insert;
                        self.command_buffer.clear();
                        self.set_temp_status("Command cancelled".to_string(), MESSAGE_STATUS_SECONDS);
                        self.request_redraw();
                    }
                    KeyCode::Enter => {
                        let command = std::mem::take(&mut self.command_buffer);
                        self.mode = InputMode::Insert;
                        self.request_redraw();
                        if self.execute_command(&command) {
                            return true;
                        }
                    }
                    KeyCode::Backspace => {
                        if self.command_buffer.len() > 1 {
                            self.command_buffer.pop();
                        }
                        self.request_redraw();
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.command_buffer.push(c);
                        self.request_redraw();
                    }
                    _ => {}
                }
                return false;
            }

            InputMode::AiConfig => {
                if self.ai_config_editing {
                    match key.code {
                        KeyCode::Esc => {
                            self.ai_config_editing = false;
                            self.config_key_buffer.clear();
                            self.request_redraw();
                        }
                        KeyCode::Enter => {
                            let value = self.config_key_buffer.trim().to_string();
                            if !value.is_empty() {
                                let prov = self.ai_config.provider.clone();
                                match self.ai_config_field {
                                    1 => {
                                        if value.is_empty() {
                                            self.delete_api_key(&prov);
                                        } else {
                                            let _ = self.save_api_key(&prov, &value);
                                        }
                                    }
                                    2 => { self.ai_config.models.insert(prov, value); }
                                    _ => {}
                                }
                                let _ = self.ai_config.save();
                            }
                            self.ai_config_editing = false;
                            self.config_key_buffer.clear();
                            self.request_full_redraw();
                        }
                        KeyCode::Backspace => {
                            self.config_key_buffer.pop();
                            self.request_redraw();
                        }
                        KeyCode::Char(c) => {
                            self.config_key_buffer.push(c);
                            self.request_redraw();
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Esc => {
                            self.mode = InputMode::Insert;
                            self.request_full_redraw();
                        }
                        KeyCode::Up => {
                            self.ai_config_field = self.ai_config_field.saturating_sub(1);
                            self.request_redraw();
                        }
                        KeyCode::Down => {
                            if self.ai_config_field < 2 { self.ai_config_field += 1; }
                            self.request_redraw();
                        }
                        KeyCode::Left | KeyCode::Right => {
                            if self.ai_config_field == 0 {
                                let names: Vec<&str> = PROVIDER_INFO.iter().map(|(n, _, _)| *n).collect();
                                let idx = names.iter().position(|n| **n == self.ai_config.provider).unwrap_or(0);
                                let new_idx = if key.code == KeyCode::Right {
                                    (idx + 1) % names.len()
                                } else {
                                    (idx + names.len() - 1) % names.len()
                                };
                                self.ai_config.provider = names[new_idx].to_string();
                                let _ = self.ai_config.save();
                                self.request_full_redraw();
                            }
                        }
                        KeyCode::Enter => {
                            if self.ai_config_field == 0 {
                                let names: Vec<&str> = PROVIDER_INFO.iter().map(|(n, _, _)| *n).collect();
                                let idx = names.iter().position(|n| **n == self.ai_config.provider).unwrap_or(0);
                                let new_idx = (idx + 1) % names.len();
                                self.ai_config.provider = names[new_idx].to_string();
                                let _ = self.ai_config.save();
                                self.request_full_redraw();
                            } else {
                                self.ai_config_editing = true;
                                self.config_key_buffer.clear();
                                if self.ai_config_field == 1 {
                                    if let Some(k) = self.get_api_key(&self.ai_config.provider) {
                                        self.config_key_buffer = k;
                                    }
                                } else if self.ai_config_field == 2 {
                                    self.config_key_buffer = self.ai_config.active_model();
                                }
                                self.request_redraw();
                            }
                        }
                        _ => {}
                    }
                }
                return false;
            }

            InputMode::Insert => {}
        }

        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Command;
                self.command_buffer.clear();
                self.command_buffer.push(':');
                self.set_temp_status("Command mode".to_string(), MESSAGE_STATUS_SECONDS);
                self.request_redraw();
            }

            KeyCode::Up => {
                if self.cursor_y > 0 {
                    self.cursor_y -= 1;
                    self.cursor_x = cmp::min(self.cursor_x, self.line_len(self.cursor_y));
                    self.request_redraw();
                }
            }

            KeyCode::Down => {
                if self.cursor_y + 1 < self.buffer.len() {
                    self.cursor_y += 1;
                } else if self.cursor_y + 1 == self.buffer.len()
                    && !self.buffer.is_last_empty()
                {
                    self.buffer.push(String::new());
                    self.cursor_y += 1;
                }
                self.cursor_x = cmp::min(self.cursor_x, self.buffer.char_len(self.cursor_y));
                self.request_redraw();
            }

            KeyCode::Left => {
                if self.cursor_x > 0 {
                    self.cursor_x -= 1;
                } else if self.cursor_y > 0 {
                    self.cursor_y -= 1;
                    self.cursor_x = self.buffer.char_len(self.cursor_y);
                }
                self.request_redraw();
            }

            KeyCode::Right => {
                if self.cursor_x < self.buffer.char_len(self.cursor_y) {
                    self.cursor_x += 1;
                } else if self.cursor_y + 1 < self.buffer.len() {
                    self.cursor_y += 1;
                    self.cursor_x = 0;
                }
                self.request_redraw();
            }

            KeyCode::Backspace => {
                self.backspace();
                self.request_full_redraw();
            }

            KeyCode::Enter => {
                self.insert_newline();
                self.request_full_redraw();
            }

            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_char(c);
                self.request_full_redraw();
            }

            _ => {}
        }

        false
    }

    fn execute_command(&mut self, command: &str) -> bool {
        let raw = command.trim();
        let raw = raw.strip_prefix(':').unwrap_or(raw).trim();

        if raw.is_empty() {
            self.set_temp_status("Empty command".to_string(), MESSAGE_STATUS_SECONDS);
            return false;
        }
        if let Ok(line_num) = raw.parse::<usize>() {
            if line_num > 0 && line_num <= self.buffer.len() {
                self.cursor_y = line_num - 1;
                self.cursor_x = 0;             
                self.request_full_redraw();
                self.set_temp_status(format!("Jumped to line {}", line_num), MESSAGE_STATUS_SECONDS);
            } else {
                self.set_temp_status(
                    format!("Line {} is out of bounds (max: {})", line_num, self.buffer.len()), 
                    MESSAGE_STATUS_SECONDS
                );
            }
            return false;
        }

        match raw {
            "w" => {
                if self.save().is_ok() {
                    self.set_temp_status(format!("SAVED: {}", self.filename), MESSAGE_STATUS_SECONDS);
                } else {
                    self.set_temp_status(format!("Save failed: {}", self.filename), MESSAGE_STATUS_SECONDS);
                }
                return false;
            }
            "q" => {
                if self.buffer.dirty {
                    self.set_temp_status(
                        "Unsaved changes. Use :q! to quit anyway.".to_string(),
                        MESSAGE_STATUS_SECONDS,
                    );
                    return false;
                }
                return true;
            }
            "q!" => {
                return true;
            }
            "wq" | "x" | "wq!" => {
                if self.save().is_ok() {
                    self.set_temp_status(format!("SAVED: {}", self.filename), MESSAGE_STATUS_SECONDS);
                    return true;
                } else {
                    self.set_temp_status(format!("Save failed: {}", self.filename), MESSAGE_STATUS_SECONDS);
                    return false;
                }
            }
            "syntax" => {
                self.syntax_highlight = !self.syntax_highlight;
                let status = if self.syntax_highlight { "on" } else { "off" };
                self.set_temp_status(format!("Syntax highlighting {}", status), MESSAGE_STATUS_SECONDS);
                self.request_full_redraw();
                return false;
            }
            "syntax on" | "syntax enable" => {
                self.syntax_highlight = true;
                self.set_temp_status("Syntax highlighting enabled".to_string(), MESSAGE_STATUS_SECONDS);
                self.request_full_redraw();
                return false;
            }
            "syntax off" | "syntax disable" => {
                self.syntax_highlight = false;
                self.set_temp_status("Syntax highlighting disabled".to_string(), MESSAGE_STATUS_SECONDS);
                self.request_full_redraw();
                return false;
            }
            "chmod" => {
                // Feature: Only allow chmod on shell scripts detected via filename
                if self.language != Language::Shell {
                    self.set_temp_status("Error: :chmod only works for .sh files".to_string(), MESSAGE_STATUS_SECONDS);
                    return false;
                }

                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    match fs::metadata(&self.filename) {
                        Ok(metadata) => {
                            let mut perms = metadata.permissions();
                            let mode = perms.mode();
                            // Add executable bit for user, group, and others (equivalent to chmod +x)
                            perms.set_mode(mode | 0o111); 
                            
                            if fs::set_permissions(&self.filename, perms).is_ok() {
                                self.set_temp_status("Permission worked: +x applied".to_string(), MESSAGE_STATUS_SECONDS);
                            } else {
                                self.set_temp_status("Permission failed: Failed to write permissions".to_string(), MESSAGE_STATUS_SECONDS);
                            }
                        }
                        Err(_) => {
                            self.set_temp_status("Permission failed: Save the file first!".to_string(), MESSAGE_STATUS_SECONDS);
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    self.set_temp_status("Permission failed: chmod not supported on this OS".to_string(), MESSAGE_STATUS_SECONDS);
                }
                return false;
            }
            _ => {}
        }

        if let Some(shell_cmd) = raw.strip_prefix('!') {
            self.run_shell_command(shell_cmd.trim());
            return false;
        }

        if let Some(rest) = raw.strip_prefix("ai") {
            let rest = rest.trim();
            if rest == "--config" || rest.starts_with("--config ") {
                self.mode = InputMode::AiConfig;
                self.ai_config_field = 0;
                self.ai_config_editing = false;
                self.request_full_redraw();
                return false;
            }

            let mut line_range: Option<(usize, usize)> = None;
            let mut prompt = rest.to_string();

            if let Some(loc) = rest.find("-l ") {
                let after_flags = rest[loc + 3..].trim_start();
                let range_end = after_flags.find(' ').unwrap_or(after_flags.len());
                let range_str = &after_flags[..range_end];
                if let Some((a, b)) = range_str.split_once('-') {
                    if let (Ok(s), Ok(e)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                        if s >= 1 && e >= s {
                            line_range = Some((s - 1, e - 1));
                        }
                    }
                } else if let Ok(n) = range_str.parse::<usize>() {
                    if n >= 1 {
                        line_range = Some((n - 1, n - 1));
                    }
                }
                let after_range = loc + 3 + range_end;
                prompt = rest[after_range..].trim().to_string();
            }

            self.run_ai_command(prompt, line_range);
            return false;
        }

        self.set_temp_status(format!("Unknown command: :{}", raw), MESSAGE_STATUS_SECONDS);
        false
    }

    fn run_shell_command(&mut self, shell_cmd: &str) {
        if shell_cmd.trim().is_empty() {
            self.set_temp_status("Usage: :!<shell command>".to_string(), MESSAGE_STATUS_SECONDS);
            return;
        }

        let output = if cfg!(target_os = "windows") {
            Command::new("cmd").args(["/C", shell_cmd]).output()
        } else {
            Command::new("sh").arg("-c").arg(shell_cmd).output()
        };

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();

                let msg = match (stdout.is_empty(), stderr.is_empty()) {
                    (true, true) => "[shell command produced no output]".to_string(),
                    (false, true) => stdout,
                    (true, false) => stderr,
                    (false, false) => format!("{} | {}", stdout, stderr),
                };

                self.set_temp_status(msg, MESSAGE_STATUS_SECONDS);
            }
            Err(e) => {
                self.set_temp_status(format!("Shell command failed: {}", e), MESSAGE_STATUS_SECONDS);
            }
        }
    }

    fn run_ai_command(&mut self, request: String, line_range: Option<(usize, usize)>) {
        let request = if request.trim().is_empty() {
            "Review this file and suggest fixes.".to_string()
        } else {
            request
        };

        let prov = self.ai_config.provider.clone();
        let has_key = self.get_api_key(&prov).is_some();

        if !has_key {
            self.pending_ai_request = Some(request);
            self.pending_ai_line_range = line_range;
            self.mode = InputMode::AwaitAiKey;
            self.config_key_buffer.clear();
            self.set_temp_status(format!("Enter {} API key", prov), MESSAGE_STATUS_SECONDS);
            self.request_redraw();
            return;
        }

        self.set_temp_status("AI thinking...".to_string(), AI_STATUS_SECONDS);
        self.needs_redraw = true;
        let _ = self.render(&mut stdout());

        match self.call_ai_api(&request, line_range) {
            Ok(reply) => {
                let wrap_width = terminal::size().ok().map(|(w, _)| w as usize).unwrap_or(80);
                let lines: Vec<String> = reply.lines()
                    .flat_map(|line| {
                        if line.chars().count() <= wrap_width {
                            vec![line.to_string()]
                        } else {
                            line.chars()
                                .collect::<Vec<_>>()
                                .chunks(wrap_width)
                                .map(|c| c.iter().collect())
                                .collect()
                        }
                    })
                    .collect();
                self.ai_output = Some(lines);
                self.ai_scroll = 0;
                self.request_full_redraw();
            }
            Err(e) => {
                self.set_temp_status(format!("AI error: {}", e), AI_STATUS_SECONDS);
            }
        }
    }

    fn get_api_key(&self, provider: &str) -> Option<String> {
        if let Ok(entry) = keyring::Entry::new("van-editor", provider) {
            if let Ok(key) = entry.get_password() {
                return Some(key);
            }
        }
        self.ai_config.local_keys.get(provider).cloned()
    }

    fn save_api_key(&mut self, provider: &str, key: &str) -> io::Result<()> {
        match keyring::Entry::new("van-editor", provider) {
            Ok(entry) => {
                entry.set_password(key)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                self.ai_config.local_keys.remove(provider);
            }
            Err(_) => {
                self.ai_config.local_keys.insert(provider.to_string(), key.to_string());
            }
        }
        let _ = self.ai_config.save();
        Ok(())
    }

    fn delete_api_key(&mut self, provider: &str) {
        if let Ok(entry) = keyring::Entry::new("van-editor", provider) {
            let _ = entry.delete_credential();
        }
        self.ai_config.local_keys.remove(provider);
        let _ = self.ai_config.save();
    }

    fn call_ai_api(&self, request: &str, line_range: Option<(usize, usize)>) -> io::Result<String> {
        let file_text = match line_range {
            Some((start, end)) => {
                let end = end.min(self.buffer.len().saturating_sub(1));
                (start..=end)
                    .map(|i| self.buffer.get_line(i))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            None => self.buffer.to_file_string(),
        };
        let system_prompt = "You are a concise coding assistant. Be practical and direct.";
        let user_content = format!("Current file:\n\n{}\n\nUser request:\n{}", file_text, request);
        let prov = &self.ai_config.provider;
        let api_key = self.get_api_key(prov)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, format!("no API key for {}", prov)))?;
        let model = self.ai_config.active_model();
        let endpoint = self.ai_config.endpoint();

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        let json: serde_json::Value = match prov.as_str() {
            "anthropic" => {
                let body = serde_json::json!({
                    "model": model,
                    "max_tokens": 4096,
                    "messages": [{"role": "user", "content": user_content}]
                });
                let resp = client.post(endpoint)
                    .header("x-api-key", api_key)
                    .header("anthropic-version", &self.ai_config.anthropic_version)
                    .json(&body)
                    .send()
                    .and_then(|r| r.error_for_status())
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                resp.json::<serde_json::Value>()
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            }
            "gemini" => {
                let url = format!("{}:generateContent?key={}", endpoint, api_key);
                let body = serde_json::json!({
                    "contents": [{"parts": [{"text": user_content}]}]
                });
                let resp = client.post(&url)
                    .json(&body)
                    .send()
                    .and_then(|r| r.error_for_status())
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                resp.json::<serde_json::Value>()
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            }
            _ => {
                let body = serde_json::json!({
                    "model": model,
                    "messages": [
                        {"role": "system", "content": system_prompt},
                        {"role": "user", "content": user_content}
                    ],
                    "temperature": 0.2
                });
                let resp = client.post(endpoint)
                    .header("Authorization", format!("Bearer {}", api_key))
                    .json(&body)
                    .send()
                    .and_then(|r| r.error_for_status())
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                resp.json::<serde_json::Value>()
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
            }
        };

        let reply = match prov.as_str() {
            "anthropic" => json["content"][0]["text"].as_str(),
            "gemini" => json["candidates"][0]["content"]["parts"][0]["text"].as_str(),
            _ => json["choices"][0]["message"]["content"].as_str(),
        };

        reply
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "empty AI response"))
    }

    fn push_undo(&mut self, action: UndoAction) {
        self.undo_stack.push(UndoEntry {
            action,
            cursor_x: self.cursor_x,
            cursor_y: self.cursor_y,
            offset_x: self.offset_x,
            offset_y: self.offset_y,
            dirty: self.buffer.dirty,
        });
    }

    fn undo(&mut self) -> bool {
        let Some(entry) = self.undo_stack.pop() else {
            return false;
        };

        match entry.action {
            UndoAction::InsertChar { y, x, .. } => {
                if y < self.buffer.len() {
                    let line = self.buffer.get_line_mut(y);
                    let byte_idx = char_to_byte_idx(line, x);
                    if byte_idx < line.len() {
                        line.remove(byte_idx);
                    }
                }
            }
            UndoAction::DeleteChar { y, x, ch } => {
                if y < self.buffer.len() {
                    let line = self.buffer.get_line_mut(y);
                    let byte_idx = char_to_byte_idx(line, x);
                    line.insert(byte_idx, ch);
                }
            }
            UndoAction::InsertNewline { y, x: _, right } => {
                if y + 1 < self.buffer.len() {
                    self.buffer.remove(y + 1);
                    self.buffer.get_line_mut(y).push_str(&right);
                }
            }
            UndoAction::JoinLines { y, x, .. } => {
                if y > 0 && y - 1 < self.buffer.len() {
                    let prev = self.buffer.get_line_mut(y - 1);
                    let right = prev.split_off(x);
                    self.buffer.insert(y, right);
                }
            }
            UndoAction::PasteBlock { saved_lines, .. } => {
                self.buffer.restore_from_snapshot(saved_lines, entry.dirty);
            }
        }

        self.cursor_x = entry.cursor_x;
        self.cursor_y = entry.cursor_y;
        self.offset_x = entry.offset_x;
        self.offset_y = entry.offset_y;
        self.buffer.dirty = entry.dirty;
        self.request_full_redraw();
        true
    }

    fn save(&mut self) -> io::Result<()> {
        let text = self.buffer.to_file_string();
        fs::write(&self.filename, text)?;
        self.buffer.dirty = false;
        Ok(())
    }

    fn set_temp_status(&mut self, msg: String, seconds: u64) {
        self.temp_status = Some((msg, Instant::now() + Duration::from_secs(seconds)));
    }

    fn current_status(&self) -> String {
        if self.confirm_exit {
            return "Exit without saving? (y = quit, s = save & quit, n = cancel)".to_string();
        }

        if self.in_search {
            return format!("Search: {}", self.search_input);
        }

        if self.mode == InputMode::Command {
            return format!("Command: {}", self.command_buffer);
        }

        if self.mode == InputMode::AwaitAiKey {
            let masked = "*".repeat(self.config_key_buffer.chars().count());
            let prov = &self.ai_config.provider;
            return format!(
                "{} API key: {} | Enter = save | Esc = cancel",
                prov, masked
            );
        }

        if let Some((msg, until)) = &self.temp_status {
            if Instant::now() < *until {
                return msg.clone();
            }
        }

        let dirty = if self.buffer.dirty { "*" } else { "" };
        format!(
            "{}{} | Ctrl+S Save | Ctrl+F Find | Ctrl+Z Undo | Ctrl+X Exit | Esc Command",
            dirty, self.filename
        )
    }

    fn update_scroll(&mut self, width: usize, height: usize) {
        let text_rows = height.saturating_sub(1);

        if self.cursor_y < self.offset_y {
            self.offset_y = self.cursor_y;
        } else if self.cursor_y >= self.offset_y + text_rows {
            self.offset_y = self.cursor_y.saturating_sub(text_rows.saturating_sub(1));
        }

        if self.cursor_x < self.offset_x {
            self.offset_x = self.cursor_x;
        } else if self.cursor_x >= self.offset_x + width {
            self.offset_x = self.cursor_x.saturating_sub(width.saturating_sub(1));
        }
    }

    fn render(&mut self, out: &mut Stdout) -> io::Result<()> {
        let (w_u16, h_u16) = terminal::size()?;
        let width = w_u16 as usize;
        let height = h_u16 as usize;

        if self.mode == InputMode::AiConfig {
            queue!(out, Clear(ClearType::All))?;
            let text_rows = height.saturating_sub(1);
            let mut cfg_lines: Vec<String> = Vec::new();

            cfg_lines.push(" AI config".to_string());
            cfg_lines.push(String::new());

            let prov = &self.ai_config.provider;
            let pmark = if self.ai_config_field == 0 && !self.ai_config_editing { ">" } else { " " };
            cfg_lines.push(format!("{} Provider: {}", pmark, prov));

            let kmark = if self.ai_config_field == 1 && !self.ai_config_editing { ">" } else { " " };
            let key_disp = if self.get_api_key(prov).is_some() { "********" } else { "not set" };
            cfg_lines.push(format!("{} API Key: {}", kmark, key_disp));

            let mmark = if self.ai_config_field == 2 && !self.ai_config_editing { ">" } else { " " };
            cfg_lines.push(format!("{} Model: {}", mmark, self.ai_config.active_model()));

            cfg_lines.push(String::new());

            if self.ai_config_editing {
                let label = match self.ai_config_field {
                    1 => "API Key",
                    2 => "Model",
                    _ => "Value",
                };
                cfg_lines.push(format!("{}: {}", label, self.config_key_buffer));
                cfg_lines.push(String::new());
            }

            cfg_lines.push("↑/↓ select  ←/→ cycle provider  Enter edit  Esc exit".to_string());

            for i in 0..text_rows {
                let line = cfg_lines.get(i).map(|s| truncate_to_width(s, width)).unwrap_or_default();
                queue!(out, cursor::MoveTo(0, i as u16), Print(line))?;
            }

            let status = "AI CONFIG";
            let padded = pad_to_width(&truncate_to_width(status, width), width);
            if height > 0 {
                queue!(out, cursor::MoveTo(0, (height - 1) as u16),
                    SetAttribute(Attribute::Reverse), Print(padded), SetAttribute(Attribute::Reset))?;
            }

            out.flush()?;
            self.needs_redraw = false;
            self.force_full_redraw = false;
            return Ok(());
        }

        if let Some(ai_lines) = &self.ai_output {
            queue!(out, Clear(ClearType::All))?;

            let text_rows = height.saturating_sub(1);

            for i in 0..text_rows {
                let idx = self.ai_scroll + i;
                if idx < ai_lines.len() {
                    let line = truncate_to_width(&ai_lines[idx], width);
                    queue!(out, cursor::MoveTo(0, i as u16), Print(line))?;
                }
            }

            let status = "[AI VIEW] ↑/↓ scroll | Esc to exit";
            let padded = pad_to_width(&truncate_to_width(status, width), width);

            if height > 0 {
                queue!(
                    out,
                    cursor::MoveTo(0, (height - 1) as u16),
                    SetAttribute(Attribute::Reverse),
                    Print(padded),
                    SetAttribute(Attribute::Reset)
                )?;
            }

            out.flush()?;
            self.needs_redraw = false;
            self.force_full_redraw = false;
            return Ok(());
        }

        self.update_scroll(width, height);

        let text_rows = height.saturating_sub(1);
        let status_row = height.saturating_sub(1);

        let current_rows = self.build_rows(width, text_rows);

        let size_changed = self.last_size != (w_u16, h_u16);
        let full_redraw = self.force_full_redraw
            || size_changed
            || self.last_rendered_rows.len() != current_rows.len();

        for row in 0..text_rows {
            let new_text = current_rows.get(row).map(String::as_str).unwrap_or("");
            let old_text = self.last_rendered_rows.get(row).map(String::as_str).unwrap_or("");

            if full_redraw || new_text != old_text {
                queue!(
                    out,
                    cursor::MoveTo(0, row as u16),
                    Clear(ClearType::CurrentLine)
                )?;
                self.draw_visible_line(out, row, width)?;
            }
        }

        let status = self.current_status();
        let padded_status = pad_to_width(&truncate_to_width(&status, width), width);
        let old_status = self
            .last_rendered_rows
            .get(text_rows)
            .map(String::as_str)
            .unwrap_or("");

        if full_redraw || padded_status != old_status {
            queue!(
                out,
                cursor::MoveTo(0, status_row as u16),
                SetAttribute(Attribute::Reverse),
                Clear(ClearType::CurrentLine),
                Print(&padded_status),
                SetAttribute(Attribute::Reset)
            )?;
        }

        if height > 0 {
            let cx = self
                .cursor_x
                .saturating_sub(self.offset_x)
                .min(width.saturating_sub(1)) as u16;
            let cy = self
                .cursor_y
                .saturating_sub(self.offset_y)
                .min(text_rows.saturating_sub(1)) as u16;
            queue!(out, cursor::MoveTo(cx, cy))?;
        }

        out.flush()?;

        self.last_rendered_rows = current_rows;
        if self.last_rendered_rows.len() == text_rows {
            self.last_rendered_rows.push(padded_status);
        } else {
            if self.last_rendered_rows.len() > text_rows {
                self.last_rendered_rows.truncate(text_rows);
            }
            self.last_rendered_rows.push(padded_status);
        }

        self.last_size = (w_u16, h_u16);
        self.force_full_redraw = false;
        self.needs_redraw = false;

        Ok(())
    }

    fn build_rows(&self, width: usize, text_rows: usize) -> Vec<String> {
        let mut rows = Vec::with_capacity(text_rows + 1);

        for i in 0..text_rows {
            let line_idx = self.offset_y + i;
            if line_idx < self.buffer.len() {
                rows.push(self.visible_plain_text(self.buffer.get_line(line_idx), width));
            } else {
                rows.push(String::new());
            }
        }

        rows.push(self.current_status());
        rows
    }

    fn draw_visible_line(&self, out: &mut Stdout, row: usize, width: usize) -> io::Result<()> {
        let line_idx = self.offset_y + row;
        if line_idx >= self.buffer.len() {
            return Ok(());
        }

        let line = self.buffer.get_line(line_idx);
        let start_byte = char_to_byte_idx(line, self.offset_x);
        let end_byte = char_to_byte_idx(line, self.offset_x + width);
        let visible = &line[start_byte..end_byte];

        if self.search_highlight.is_empty() && self.syntax_highlight {
            return self.write_colored(out, visible);
        }

        if self.search_highlight.is_empty() {
            queue!(out, Print(sanitize_str(visible)))?;
            return Ok(());
        }

        let query = self.search_highlight.as_str();
        let mut idx = 0;

        while idx < visible.len() {
            if let Some(pos) = visible[idx..].find(query) {
                let abs = idx + pos;
    
                if abs > idx {
                    queue!(out, Print(sanitize_str(&visible[idx..abs])))?;
                }

                let end = abs + query.len();
                queue!(
                    out,
                    SetAttribute(Attribute::Reverse),
                    Print(sanitize_str(&visible[abs..end])),
                    SetAttribute(Attribute::Reset)
                )?;

                idx = end;
            } else {
                queue!(out, Print(sanitize_str(&visible[idx..])))?;
                break;
            }
        }

        Ok(())
    }

    fn write_colored(&self, out: &mut Stdout, visible: &str) -> io::Result<()> {
        let segments = tokenize(visible, self.language);
        for (text, color) in &segments {
            if let Some(c) = color {
                queue!(out, SetForegroundColor(*c), Print(text.as_str()), ResetColor)?;
            } else {
                queue!(out, Print(text.as_str()))?;
            }
        }
        Ok(())
    }

    fn visible_plain_text(&self, line: &str, width: usize) -> String {
        let start_byte = char_to_byte_idx(line, self.offset_x);
        let end_byte = char_to_byte_idx(line, self.offset_x + width);
        sanitize_str(&line[start_byte..end_byte])
    }

    fn line_len(&self, y: usize) -> usize {
        self.buffer.char_len(y)
    }

    fn insert_char(&mut self, c: char) {
        let y = self.cursor_y;
        let x = self.cursor_x;
        self.push_undo(UndoAction::InsertChar { y, x, ch: c });

        let byte_idx = char_to_byte_idx(self.buffer.get_line(y), x);
        self.buffer.get_line_mut(y).insert(byte_idx, c);
        self.cursor_x += 1;
        self.buffer.dirty = true;
    }

    fn backspace(&mut self) {
        if self.cursor_x > 0 {
            let y = self.cursor_y;
            let x = self.cursor_x - 1;
            if let Some(ch) = self.buffer.get_line(y).chars().nth(x) {
                self.push_undo(UndoAction::DeleteChar { y, x, ch });

                let line = self.buffer.get_line_mut(y);
                let byte_idx = char_to_byte_idx(line, x);
                line.remove(byte_idx);
                self.cursor_x -= 1;
                self.buffer.dirty = true;
            }
        } else if self.cursor_y > 0 {
            let y = self.cursor_y;
            let x = self.buffer.char_len(y - 1);
            let removed = self.buffer.get_line(y).to_string();
            self.push_undo(UndoAction::JoinLines { y, x, removed });

            let current = self.buffer.remove(y);
            self.cursor_y -= 1;
            let prev_len = self.buffer.char_len(self.cursor_y);
            self.buffer.get_line_mut(self.cursor_y).push_str(&current);
            self.cursor_x = prev_len;
            self.buffer.dirty = true;
        }
    }
    fn leading_indent(line: &str) -> usize {
        line.chars().take_while(|c| *c == ' ').count()
    }

    fn compute_indent(&self, left: &str, right: &str) -> usize {
        let base = Self::leading_indent(left);
        let left_trim = left.trim_end();
        let right_trim = right.trim_start();

        match self.language {
            Language::Python => {
                let mut indent = base;

                if left_trim.ends_with(':') {
                indent += INDENT_WIDTH;
                }

                if right_trim.starts_with("elif ")
                    || right_trim.starts_with("else:")
                    || right_trim.starts_with("except")
                {
                    indent = indent.saturating_sub(INDENT_WIDTH);
                }

                indent
            }

            Language::C | Language::Rust => {
                let mut indent = base;

                if left_trim.ends_with('{') {
                    indent += INDENT_WIDTH;
                }

                if right_trim.starts_with('}') {
                    indent = indent.saturating_sub(INDENT_WIDTH);
                }

                indent
            }

            Language::PlainText | Language::Shell => base,
        }
    }
    fn insert_newline(&mut self) {
        let y = self.cursor_y;
        let x = self.cursor_x;

        let line_content = self.buffer.get_line(y);
        let split_byte = char_to_byte_idx(line_content, x);
        let left = line_content[..split_byte].to_string();
        let right = line_content[split_byte..].to_string();

        let indent = self.compute_indent(&left, &right);

        self.push_undo(UndoAction::InsertNewline {
            y,
            x,
            right: right.clone(),
        });

        *self.buffer.get_line_mut(y) = left;

        let mut new_line = " ".repeat(indent);
        new_line.push_str(&right);

        self.buffer.insert(y + 1, new_line);
        self.cursor_y += 1;
        self.cursor_x = indent;
        self.buffer.dirty = true;
    }

    fn handle_paste_event(&mut self, text: String) {
        match self.mode {
            InputMode::AwaitAiKey => {
                for c in text.chars().filter(|c| *c != '\r') {
                    self.config_key_buffer.push(c);
                }
                self.request_redraw();
                return;
            }
            InputMode::AiConfig if self.ai_config_editing => {
                for c in text.chars().filter(|c| *c != '\r') {
                    self.config_key_buffer.push(c);
                }
                self.request_redraw();
                return;
            }
            _ => {}
        }
        self.handle_paste(text);
    }

    fn handle_paste(&mut self, text: String) {
        let saved_lines = self.buffer.clone_all();
        let saved_cursor_y = self.cursor_y;
        let saved_cursor_x = self.cursor_x;
        let saved_dirty = self.buffer.dirty;

        for ch in text.chars() {
            match ch {
                '\n' | '\r' => {
                    let y = self.cursor_y;
                    let x = self.cursor_x;
                    let cur = self.buffer.get_line(y);
                    let split_byte = char_to_byte_idx(cur, x);
                    let right = cur[split_byte..].to_string();
                    self.buffer.get_line_mut(y).truncate(split_byte);
                    self.buffer.insert(y + 1, right);
                    self.cursor_y += 1;
                    self.cursor_x = 0;
                    self.buffer.dirty = true;
                }
                '\t' => {
                    for _ in 0..4 {
                        let line = self.buffer.get_line_mut(self.cursor_y);
                        line.insert(char_to_byte_idx(line, self.cursor_x), ' ');
                        self.cursor_x += 1;
                    }
                    self.buffer.dirty = true;
                }
                c if !c.is_control() => {
                    let line = self.buffer.get_line_mut(self.cursor_y);
                    line.insert(char_to_byte_idx(line, self.cursor_x), c);
                    self.cursor_x += 1;
                    self.buffer.dirty = true;
                }
                _ => {}
            }
        }
        self.undo_stack.push(UndoEntry {
            action: UndoAction::PasteBlock { saved_lines, saved_cursor_y, saved_cursor_x, saved_dirty },
            cursor_x: saved_cursor_x,
            cursor_y: saved_cursor_y,
            offset_x: self.offset_x,
            offset_y: self.offset_y,
            dirty: saved_dirty,
        });
        self.request_full_redraw();
    }

    fn find_first(&self, query: &str) -> Option<(usize, usize)> {
        for y in 0..self.buffer.len() {
            let line = self.buffer.get_line(y);
            if let Some(byte_idx) = line.find(query) {
                let char_idx = line[..byte_idx].chars().count();
                return Some((y, char_idx));
            }
        }
        None
    }
}

fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 0;
    }

    match s.char_indices().nth(char_idx) {
        Some((byte_idx, _)) => byte_idx,
        None => s.len(),
    }
}

fn truncate_to_width(s: &str, width: usize) -> String {
    s.chars().take(width).collect()
}

fn pad_to_width(s: &str, width: usize) -> String {
    let mut out = s.to_string();
    let len = out.chars().count();
    if len < width {
        out.push_str(&" ".repeat(width - len));
    }
    out
}

fn sanitize_str(s: &str) -> String {
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
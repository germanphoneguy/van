use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    execute, queue,
    style::{Print, ResetColor, SetBackgroundColor, SetForegroundColor},
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

mod config;
mod syntax_highlighting;
use syntax_highlighting::{Language, detect_language, tokenize, sanitize_str};

fn key_event_to_string(key: &KeyEvent) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".to_string());
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt".to_string());
    }
    let key_name = match key.code {
        KeyCode::Char(c) if c.is_ascii_alphanumeric() => c.to_ascii_lowercase().to_string(),
        _ => return None,
    };
    if parts.is_empty() {
        return None;
    }
    parts.push(key_name);
    Some(parts.join("+"))
}

const VERSION: &str = env!("CARGO_PKG_VERSION");
const VAN_LOGO: &[&str] = &[
    "░██    ░██                       ",
    "░██    ░██                       ",
    "░██    ░██  ░██████   ░████████  ",
    "░██    ░██       ░██  ░██    ░██ ",
    " ░██  ░██   ░███████  ░██    ░██ ",
    "  ░██░██   ░██   ░██  ░██    ░██ ",
    "   ░███     ░███████  ░██    ░██ ",
];
const CONFIG_LOGO: &[&str] = &[
    "  ░██████                            ░████ ░██           ",
    " ░██   ░██                          ░██                  ",
    "░██         ░███████  ░████████  ░████████ ░██ ░████████",
    "░██        ░██    ░██ ░██    ░██    ░██    ░██░██    ░██",
    "░██        ░██    ░██ ░██    ░██    ░██    ░██░██    ░██",
    " ░██   ░██ ░██    ░██ ░██    ░██    ░██    ░██░██   ░███",
    "  ░██████   ░███████  ░██    ░██    ░██    ░██ ░█████░██ ",
    "                                                     ░██",
    "                                               ░███████ ",
];
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
    config::config_dir().map(|d| d.join("van").join("ai_config.json"))
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
                let _ = fs::remove_file(config::config_dir().map(|d| d.join("van_groq_api_key")).unwrap());
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
    let base = config::config_dir()?;
    let path = base.join("van_groq_api_key");
    let key = fs::read_to_string(path).ok()?;
    let trimmed = key.trim().to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
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
                println!("  :lines on/off    Toggle line numbers");
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
        Some(args[1].clone())
    } else {
        None
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
                Event::Mouse(mouse) => {
                    if editor.handle_mouse(mouse) {
                        break;
                    }
                }
                Event::Resize(w, h) => {
                    if editor.handle_resize(w, h) {
                        editor.request_full_redraw();
                    }
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
        execute!(out,
            terminal::EnterAlternateScreen,
            event::EnableBracketedPaste,
        )?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        let _ = execute!(
            out,
            event::DisableBracketedPaste,
            cursor::Show,
            terminal::LeaveAlternateScreen
        );
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
    FilePicker,
    ConfigTui,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FilePickerView {
    Simple,
    Manager,
}

struct FilePickerEntry {
    name: String,
    display: String,
    is_dir: bool,
    size: u64,
}

enum PendingFileOp {
    Copy { source: PathBuf },
    Move { source: PathBuf },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PromptState {
    None,
    ConfirmDelete { entry_idx: usize },
    ConfirmOverwrite { path: usize, is_move: bool },
    InputRename { entry_idx: usize },
    InputCreateFile,
    InputCreateDir,
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

    config: config::VanConfig,
    syntax_highlight: bool,
    show_line_numbers: bool,

    file_picker_entries: Vec<FilePickerEntry>,
    file_picker_selection: usize,
    file_picker_current_dir: PathBuf,
    file_picker_view: FilePickerView,
    prompt_state: PromptState,
    prompt_input: String,
    pending_file_op: Option<PendingFileOp>,
    show_hidden: bool,
    git_branch: Option<String>,
    git_refreshed: Instant,
    config_tui: ConfigTuiState,
}

#[derive(Clone)]
struct ConfigTuiState {
    cursor: usize,
    scroll: usize,
    expanded: [bool; 5],
    edit_mode: Option<ConfigEditMode>,
    edit_buffer: String,
    edit_cursor: usize,
}

#[derive(Clone)]
enum ConfigEditMode {
    StyleField,
    ColorField(usize),
    KeybindField(String),
}

#[derive(Clone, Copy)]
enum ConfigTuiItem {
    Section(usize),
    Style,
    Position,
    ContentToggle(usize),
    ColorField(usize),
    KeybindField(usize),
    MenuToggle(usize),
    Button(&'static str),
}

impl Editor {
    fn open(filename: Option<String>) -> Self {
        let (fname, mode) = match filename {
            Some(f) => (f, InputMode::Insert),
            None => (String::new(), InputMode::FilePicker),
        };
        let language = if !fname.is_empty() { detect_language(&fname) } else { Language::PlainText };
        let buffer = if !fname.is_empty() { FileBuffer::load(&fname) } else { FileBuffer::new_empty() };

        let mut editor = Self {
            language,
            filename: fname,
            buffer,
            cursor_x: 0,
            cursor_y: 0,
            offset_x: 0,
            offset_y: 0,
            search_input: String::new(),
            search_highlight: String::new(),
            in_search: false,
            confirm_exit: false,
            mode,
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

            config: config::load_config(),
            ai_output: None,
            ai_scroll: 0,
            syntax_highlight: true,
            show_line_numbers: false,
            file_picker_entries: Vec::new(),
            file_picker_selection: 0,
            file_picker_current_dir: PathBuf::new(),
            file_picker_view: FilePickerView::Simple,
            prompt_state: PromptState::None,
            prompt_input: String::new(),
            pending_file_op: None,
            show_hidden: false,
            git_branch: None,
            git_refreshed: Instant::now(),
            config_tui: ConfigTuiState {
                cursor: 0,
                scroll: 0,
                expanded: [true, false, false, false, false],
                edit_mode: None,
                edit_buffer: String::new(),
                edit_cursor: 0,
            },
        };

        if editor.mode == InputMode::FilePicker {
            editor.file_picker_current_dir = env::current_dir().unwrap_or_default();
            editor.refresh_file_picker();
        }

        editor
    }

    fn request_redraw(&mut self) {
        self.needs_redraw = true;
    }

    fn request_full_redraw(&mut self) {
        self.needs_redraw = true;
        self.force_full_redraw = true;
    }

    fn handle_resize(&mut self, w: u16, h: u16) -> bool {
        let changed = self.last_size != (w, h);
        if changed {
            self.request_redraw();
        }
        changed
    }

    fn refresh_file_picker(&mut self) {
        let mut entries = Vec::new();
        if let Ok(read_dir) = fs::read_dir(&self.file_picker_current_dir) {
            for entry in read_dir {
                if let Ok(entry) = entry {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if !self.show_hidden && name.starts_with('.') { continue; }
                    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    let display = if is_dir { format!("{}/", name) } else { name.clone() };
                    entries.push(FilePickerEntry { name, display, is_dir, size });
                }
            }
        }
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.display.cmp(&b.display)));
        self.file_picker_entries = entries;
        self.file_picker_selection = 0;
    }

    fn open_file_picker_selection(&mut self) {
        let Some(entry) = self.file_picker_entries.get(self.file_picker_selection) else { return };
        let path = self.file_picker_current_dir.join(&entry.name);
        if entry.is_dir {
            self.file_picker_current_dir = path;
            self.prompt_state = PromptState::None;
            self.prompt_input.clear();
            self.refresh_file_picker();
            self.request_full_redraw();
        } else {
            let path_str = path.to_string_lossy().to_string();
            self.filename = path_str.clone();
            self.language = detect_language(&path_str);
            self.buffer = FileBuffer::load(&path_str);
            self.mode = InputMode::Insert;
            self.cursor_x = 0;
            self.cursor_y = 0;
            self.offset_x = 0;
            self.offset_y = 0;
            self.request_full_redraw();
        }
    }

    fn go_to_parent_dir(&mut self) {
        if let Some(parent) = self.file_picker_current_dir.parent() {
            if parent.as_os_str().is_empty() {
                self.file_picker_current_dir = PathBuf::from("/");
            } else {
                self.file_picker_current_dir = parent.to_path_buf();
            }
            self.prompt_state = PromptState::None;
            self.prompt_input.clear();
            self.refresh_file_picker();
            self.request_full_redraw();
        }
    }

    fn handle_file_picker_key(&mut self, key: KeyEvent) -> bool {
        let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match self.prompt_state {
            PromptState::ConfirmDelete { entry_idx } => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        let entry = &self.file_picker_entries[entry_idx];
                        let path = self.file_picker_current_dir.join(&entry.name);
                        let result = if entry.is_dir { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
                        match result {
                            Ok(_) => self.set_temp_status(format!("Deleted: {}", entry.name), MESSAGE_STATUS_SECONDS),
                            Err(e) => self.set_temp_status(format!("Delete failed: {}", e), MESSAGE_STATUS_SECONDS),
                        }
                        self.prompt_state = PromptState::None;
                        self.refresh_file_picker();
                        self.request_full_redraw();
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        self.prompt_state = PromptState::None;
                        self.set_temp_status("Delete cancelled".to_string(), MESSAGE_STATUS_SECONDS);
                        self.request_redraw();
                    }
                    _ => {}
                }
                return false;
            }
            PromptState::ConfirmOverwrite { path: idx, is_move } => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        let target = self.file_picker_current_dir.join(&self.file_picker_entries[idx].name);
                        if let Some(op) = self.pending_file_op.take() {
                            let (src, op_name) = match &op {
                                PendingFileOp::Copy { source } => (source.clone(), "Copy"),
                                PendingFileOp::Move { source } => (source.clone(), "Move"),
                            };
                            let result = if is_move { fs::rename(&src, &target) } else { fs::copy(&src, &target).map(|_| ()) };
                            match result {
                                Ok(_) => self.set_temp_status(format!("{}: {} done", op_name, src.file_name().unwrap_or_default().to_string_lossy()), MESSAGE_STATUS_SECONDS),
                                Err(e) => self.set_temp_status(format!("{} failed: {}", op_name, e), MESSAGE_STATUS_SECONDS),
                            }
                        }
                        self.prompt_state = PromptState::None;
                        self.pending_file_op = None;
                        self.refresh_file_picker();
                        self.request_full_redraw();
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        self.prompt_state = PromptState::None;
                        self.set_temp_status("Overwrite cancelled".to_string(), MESSAGE_STATUS_SECONDS);
                        self.request_redraw();
                    }
                    _ => {}
                }
                return false;
            }
            PromptState::InputRename { .. } | PromptState::InputCreateFile | PromptState::InputCreateDir => {
                match key.code {
                    KeyCode::Esc => {
                        self.prompt_state = PromptState::None;
                        self.prompt_input.clear();
                        self.set_temp_status("Cancelled".to_string(), MESSAGE_STATUS_SECONDS);
                        self.request_redraw();
                    }
                    KeyCode::Enter => {
                        let input = self.prompt_input.trim().to_string();
                        if input.is_empty() {
                            self.set_temp_status("Name cannot be empty".to_string(), MESSAGE_STATUS_SECONDS);
                            self.request_redraw();
                            return false;
                        }
                        match self.prompt_state {
                            PromptState::InputRename { entry_idx } => {
                                let entry = &self.file_picker_entries[entry_idx];
                                let src = self.file_picker_current_dir.join(&entry.name);
                                let dst = self.file_picker_current_dir.join(&input);
                                match fs::rename(&src, &dst) {
                                    Ok(_) => self.set_temp_status(format!("Renamed to: {}", input), MESSAGE_STATUS_SECONDS),
                                    Err(e) => self.set_temp_status(format!("Rename failed: {}", e), MESSAGE_STATUS_SECONDS),
                                }
                            }
                            PromptState::InputCreateFile => {
                                let path = self.file_picker_current_dir.join(&input);
                                match fs::File::create(&path) {
                                    Ok(_) => self.set_temp_status(format!("Created: {}", input), MESSAGE_STATUS_SECONDS),
                                    Err(e) => self.set_temp_status(format!("Create failed: {}", e), MESSAGE_STATUS_SECONDS),
                                }
                            }
                            PromptState::InputCreateDir => {
                                let path = self.file_picker_current_dir.join(&input);
                                match fs::create_dir(&path) {
                                    Ok(_) => self.set_temp_status(format!("Created dir: {}", input), MESSAGE_STATUS_SECONDS),
                                    Err(e) => self.set_temp_status(format!("Create dir failed: {}", e), MESSAGE_STATUS_SECONDS),
                                }
                            }
                            _ => {}
                        }
                        self.prompt_state = PromptState::None;
                        self.prompt_input.clear();
                        self.refresh_file_picker();
                        self.request_full_redraw();
                    }
                    KeyCode::Backspace => {
                        self.prompt_input.pop();
                        self.request_redraw();
                    }
                    KeyCode::Char(c) if !is_ctrl => {
                        self.prompt_input.push(c);
                        self.request_redraw();
                    }
                    _ => {}
                }
                return false;
            }
            PromptState::None => {}
        }

        match self.file_picker_view {
            FilePickerView::Simple => {
                match key.code {
                    KeyCode::Tab => {
                        self.file_picker_view = FilePickerView::Manager;
                        self.request_full_redraw();
                    }
                    KeyCode::Up => {
                        self.file_picker_selection = self.file_picker_selection.saturating_sub(1);
                        self.request_redraw();
                    }
                    KeyCode::Down => {
                        let max = self.file_picker_entries.len().saturating_sub(1);
                        self.file_picker_selection = cmp::min(self.file_picker_selection + 1, max);
                        self.request_redraw();
                    }
                    KeyCode::Enter => {
                        self.open_file_picker_selection();
                    }
                    KeyCode::Backspace => {
                        self.go_to_parent_dir();
                    }
                    KeyCode::Esc => {
                        self.mode = InputMode::Insert;
                        self.set_temp_status("Opened new buffer".to_string(), MESSAGE_STATUS_SECONDS);
                        self.request_full_redraw();
                    }
                    KeyCode::Char('s') | KeyCode::Char('S') => {
                        if !is_ctrl {
                            self.mode = InputMode::ConfigTui;
                            let _ = execute!(stdout(), event::EnableMouseCapture);
                            self.config_tui = ConfigTuiState {
                                cursor: 0,
                                scroll: 0,
                                expanded: [true, true, false, false, false],
                                edit_mode: None,
                                edit_buffer: String::new(),
                                edit_cursor: 0,
                            };
                            self.request_full_redraw();
                        }
                    }
                    _ => {}
                }
            }
            FilePickerView::Manager => {
                if let Some(op) = &self.pending_file_op {
                    match key.code {
                        KeyCode::Char('x') | KeyCode::Char('X') => {
                            let src = match op {
                                PendingFileOp::Copy { source } => source.clone(),
                                PendingFileOp::Move { source } => source.clone(),
                            };
                            let target = self.file_picker_current_dir.join(
                                src.file_name().unwrap()
                            );
                            if target.exists() {
                                let idx = self.file_picker_entries.iter().position(|e| {
                                    self.file_picker_current_dir.join(&e.name) == target
                                });
                                if let Some(i) = idx {
                                    self.prompt_state = PromptState::ConfirmOverwrite { path: i, is_move: matches!(op, PendingFileOp::Move { .. }) };
                                    self.request_redraw();
                                    return false;
                                }
                            }
                            let op_name = match &self.pending_file_op {
                                Some(PendingFileOp::Copy { .. }) => { self.pending_file_op = None; "Copy" }
                                Some(PendingFileOp::Move { .. }) => { self.pending_file_op = None; "Move" }
                                _ => unreachable!(),
                            };
                            let is_move = op_name == "Move";
                            let result = if is_move { fs::rename(&src, &target) } else { fs::copy(&src, &target).map(|_| ()) };
                            match result {
                                Ok(_) => self.set_temp_status(format!("{}: {} done", op_name, src.file_name().unwrap_or_default().to_string_lossy()), MESSAGE_STATUS_SECONDS),
                                Err(e) => self.set_temp_status(format!("{} failed: {}", op_name, e), MESSAGE_STATUS_SECONDS),
                            }
                            self.refresh_file_picker();
                            self.request_full_redraw();
                        }
                        KeyCode::Esc | KeyCode::Tab => {
                            self.pending_file_op = None;
                            self.set_temp_status("Operation cancelled".to_string(), MESSAGE_STATUS_SECONDS);
                            self.file_picker_view = FilePickerView::Simple;
                            self.request_redraw();
                        }
                        KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Backspace => {
                            match key.code {
                                KeyCode::Up => {
                                    self.file_picker_selection = self.file_picker_selection.saturating_sub(1);
                                    self.request_redraw();
                                }
                                KeyCode::Down => {
                                    let max = self.file_picker_entries.len().saturating_sub(1);
                                    self.file_picker_selection = cmp::min(self.file_picker_selection + 1, max);
                                    self.request_redraw();
                                }
                                KeyCode::Enter => {
                                    self.open_file_picker_selection();
                                }
                                KeyCode::Backspace => {
                                    self.go_to_parent_dir();
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Tab => {
                            self.file_picker_view = FilePickerView::Simple;
                            self.request_full_redraw();
                        }
                        KeyCode::Up => {
                            self.file_picker_selection = self.file_picker_selection.saturating_sub(1);
                            self.request_redraw();
                        }
                        KeyCode::Down => {
                            let max = self.file_picker_entries.len().saturating_sub(1);
                            self.file_picker_selection = cmp::min(self.file_picker_selection + 1, max);
                            self.request_redraw();
                        }
                        KeyCode::Enter => {
                            self.open_file_picker_selection();
                        }
                        KeyCode::Backspace => {
                            self.go_to_parent_dir();
                        }
                        KeyCode::Esc => {
                            self.file_picker_view = FilePickerView::Simple;
                            self.request_full_redraw();
                        }
                        KeyCode::Char('h') => {
                            self.show_hidden = !self.show_hidden;
                            self.refresh_file_picker();
                            self.request_full_redraw();
                        }
                        KeyCode::Char('s') | KeyCode::Char('S') => {
                            self.mode = InputMode::ConfigTui;
                            let _ = execute!(stdout(), event::EnableMouseCapture);
                            self.config_tui = ConfigTuiState {
                                cursor: 0,
                                scroll: 0,
                                expanded: [true, true, false, false, false],
                                edit_mode: None,
                                edit_buffer: String::new(),
                                edit_cursor: 0,
                            };
                            self.request_full_redraw();
                        }
                        KeyCode::Char('r') => {
                            self.refresh_file_picker();
                            self.set_temp_status("Refreshed".to_string(), MESSAGE_STATUS_SECONDS);
                            self.request_full_redraw();
                        }
                        KeyCode::Char('n') => {
                            self.prompt_state = PromptState::InputCreateFile;
                            self.prompt_input.clear();
                            self.set_temp_status("New file name:".to_string(), MESSAGE_STATUS_SECONDS);
                            self.request_redraw();
                        }
                        KeyCode::Char('N') if !is_ctrl => {
                            self.prompt_state = PromptState::InputCreateDir;
                            self.prompt_input.clear();
                            self.set_temp_status("New directory name:".to_string(), MESSAGE_STATUS_SECONDS);
                            self.request_redraw();
                        }
                        KeyCode::Char('d') | KeyCode::Char('D') => {
                            if self.file_picker_entries.is_empty() {
                                self.set_temp_status("Nothing selected".to_string(), MESSAGE_STATUS_SECONDS);
                                self.request_redraw();
                            } else {
                                let name = self.file_picker_entries[self.file_picker_selection].name.clone();
                                self.prompt_state = PromptState::ConfirmDelete { entry_idx: self.file_picker_selection };
                                self.set_temp_status(format!("Delete '{}'? (y/n)", name), MESSAGE_STATUS_SECONDS);
                                self.request_redraw();
                            }
                        }
                        KeyCode::Char('R') => {
                            if self.file_picker_entries.is_empty() {
                                self.set_temp_status("Nothing selected".to_string(), MESSAGE_STATUS_SECONDS);
                                self.request_redraw();
                            } else {
                                let name = self.file_picker_entries[self.file_picker_selection].name.clone();
                                self.prompt_state = PromptState::InputRename { entry_idx: self.file_picker_selection };
                                self.prompt_input = name.clone();
                                self.set_temp_status(format!("Rename '{}' to:", name), MESSAGE_STATUS_SECONDS);
                                self.request_redraw();
                            }
                        }
                        KeyCode::Char('c') if !is_ctrl => {
                            if self.file_picker_entries.is_empty() {
                                self.set_temp_status("Nothing selected".to_string(), MESSAGE_STATUS_SECONDS);
                                self.request_redraw();
                            } else {
                                let path = self.file_picker_current_dir.join(&self.file_picker_entries[self.file_picker_selection].name);
                                if path.is_dir() {
                                    self.set_temp_status("Cannot copy a directory (file only)".to_string(), MESSAGE_STATUS_SECONDS);
                                    self.request_redraw();
                                } else {
                                    let name = self.file_picker_entries[self.file_picker_selection].name.clone();
                                    self.pending_file_op = Some(PendingFileOp::Copy { source: path });
                                    self.set_temp_status(format!("Copy: '{}' — navigate to target, press x", name), MESSAGE_STATUS_SECONDS);
                                    self.request_redraw();
                                }
                            }
                        }
                        KeyCode::Char('m') | KeyCode::Char('M') => {
                            if self.file_picker_entries.is_empty() {
                                self.set_temp_status("Nothing selected".to_string(), MESSAGE_STATUS_SECONDS);
                                self.request_redraw();
                            } else {
                                let path = self.file_picker_current_dir.join(&self.file_picker_entries[self.file_picker_selection].name);
                                if path.is_dir() {
                                    self.set_temp_status("Cannot move a directory (file only)".to_string(), MESSAGE_STATUS_SECONDS);
                                    self.request_redraw();
                                } else {
                                    let name = self.file_picker_entries[self.file_picker_selection].name.clone();
                                    self.pending_file_op = Some(PendingFileOp::Move { source: path });
                                    self.set_temp_status(format!("Move: '{}' — navigate to target, press x", name), MESSAGE_STATUS_SECONDS);
                                    self.request_redraw();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        false
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

    fn refresh_git_branch(&mut self) {
        if self.git_refreshed.elapsed() < Duration::from_secs(10) {
            return;
        }
        self.git_refreshed = Instant::now();
        let dir = if self.filename.is_empty() {
            match std::env::current_dir() {
                Ok(d) => d,
                Err(_) => return,
            }
        } else {
            let p = std::path::Path::new(&self.filename);
            if p.is_absolute() {
                match p.parent() {
                    Some(d) => d.to_path_buf(),
                    None => return,
                }
            } else {
                match std::env::current_dir() {
                    Ok(mut d) => {
                        d.push(&self.filename);
                        d.pop();
                        d
                    }
                    Err(_) => return,
                }
            }
        };
        let out = match std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["branch", "--show-current"])
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return,
        };
        let branch = match String::from_utf8(out.stdout) {
            Ok(s) => s.trim().to_string(),
            Err(_) => return,
        };
        self.git_branch = if branch.is_empty() { None } else { Some(branch) };
    }

    fn tick(&mut self) -> bool {
        self.refresh_git_branch();
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

        if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
            if let Some(lookup) = key_event_to_string(&key) {
                if let Some(action) = self.config.keybindings.lookup(&lookup) {
                    match action {
                        config::EditorAction::Exit => {
                            if !self.buffer.dirty {
                                return true;
                            }
                            self.confirm_exit = true;
                            self.request_redraw();
                            return false;
                        }
                        config::EditorAction::Save => {
                            if self.save().is_ok() {
                                self.set_temp_status(format!("SAVED: {}", self.filename), MESSAGE_STATUS_SECONDS);
                            } else {
                                self.set_temp_status(format!("Save failed: {}", self.filename), MESSAGE_STATUS_SECONDS);
                            }
                            self.request_redraw();
                            return false;
                        }
                        config::EditorAction::Find => {
                            self.in_search = true;
                            if self.search_highlight.is_empty() {
                                self.search_input.clear();
                            } else {
                                self.search_input = self.search_highlight.clone();
                            }
                            self.request_redraw();
                            return false;
                        }
                        config::EditorAction::Undo => {
                            if self.undo() {
                                self.set_temp_status("Undid last edit".to_string(), MESSAGE_STATUS_SECONDS);
                            } else {
                                self.set_temp_status("Nothing to undo".to_string(), MESSAGE_STATUS_SECONDS);
                            }
                            self.request_full_redraw();
                            return false;
                        }
                        config::EditorAction::ToggleLineNumbers => {
                            self.show_line_numbers = !self.show_line_numbers;
                            let status = if self.show_line_numbers { "on" } else { "off" };
                            self.set_temp_status(format!("Line numbers {}", status), MESSAGE_STATUS_SECONDS);
                            self.request_full_redraw();
                            return false;
                        }
                    }
                }
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
            InputMode::FilePicker => {}
            InputMode::ConfigTui => {}
        }

        if self.mode == InputMode::FilePicker {
            return self.handle_file_picker_key(key);
        }

        if self.mode == InputMode::ConfigTui {
            return self.handle_config_tui_key(key);
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
            "lines" => {
                self.show_line_numbers = !self.show_line_numbers;
                let status = if self.show_line_numbers { "on" } else { "off" };
                self.set_temp_status(format!("Line numbers {}", status), MESSAGE_STATUS_SECONDS);
                self.request_full_redraw();
                return false;
            }
            "lines on" | "lines enable" => {
                self.show_line_numbers = true;
                self.set_temp_status("Line numbers enabled".to_string(), MESSAGE_STATUS_SECONDS);
                self.request_full_redraw();
                return false;
            }
            "lines off" | "lines disable" => {
                self.show_line_numbers = false;
                self.set_temp_status("Line numbers disabled".to_string(), MESSAGE_STATUS_SECONDS);
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

        let parts: Vec<String> = self.config.status_bar_content.iter()
            .filter_map(|token| self.status_token_value(token))
            .collect();

        if parts.is_empty() {
            self.filename.clone()
        } else {
            parts.join(" | ")
        }
    }

    fn status_token_value(&self, token: &str) -> Option<String> {
        match token {
            "filename" => {
                let prefix = if self.buffer.dirty { "*" } else { "" };
                Some(format!("{}{}", prefix, self.filename))
            }
            "binds" => {
                Some(self.config.keybindings.display_binds())
            }
            "git" => {
                self.git_branch.as_ref().map(|b| format!("git:({})", b))
            }
            "time" => {
                Some(chrono::Local::now().format("%H:%M").to_string())
            }
            _ => None,
        }
    }

    fn update_scroll(&mut self, width: usize, height: usize) {
        let text_rows = height.saturating_sub(1);
        let text_width = width.saturating_sub(self.gutter_width());

        if self.cursor_y < self.offset_y {
            self.offset_y = self.cursor_y;
        } else if self.cursor_y >= self.offset_y + text_rows {
            self.offset_y = self.cursor_y.saturating_sub(text_rows.saturating_sub(1));
        }

        if self.cursor_x < self.offset_x {
            self.offset_x = self.cursor_x;
        } else if self.cursor_x >= self.offset_x + text_width {
            self.offset_x = self.cursor_x.saturating_sub(text_width.saturating_sub(1));
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
                queue!(out, cursor::MoveTo(0, (height - 1) as u16))?;
                self.render_styled_status_bar(out, &padded, width)?;
            }

            out.flush()?;
            self.needs_redraw = false;
            self.force_full_redraw = false;
            return Ok(());
        }

        if self.mode == InputMode::FilePicker {
            queue!(out, Clear(ClearType::All), cursor::Hide)?;

            let dir_str = self.file_picker_current_dir.to_string_lossy().to_string();

            match self.file_picker_view {
                FilePickerView::Simple => {
                    self.render_simple_picker(out, width, height, &dir_str)?;
                }
                FilePickerView::Manager => {
                    self.render_manager_picker(out, width, height, &dir_str)?;
                }
            }

            out.flush()?;
            self.needs_redraw = false;
            self.force_full_redraw = false;
            return Ok(());
        }

        if self.mode == InputMode::ConfigTui {
            queue!(out, Clear(ClearType::All), cursor::Hide)?;
            self.render_config_tui(out, width, height)?;
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
                queue!(out, cursor::MoveTo(0, (height - 1) as u16))?;
                self.render_styled_status_bar(out, &padded, width)?;
            }

            out.flush()?;
            self.needs_redraw = false;
            self.force_full_redraw = false;
            return Ok(());
        }

        self.update_scroll(width, height);

        use config::StatusBarPosition as Sbp;
        let status_on_top = self.config.status_bar_position == Sbp::Top;
        let text_rows = height.saturating_sub(1);
        let text_offset: usize = if status_on_top { 1 } else { 0 };
        let status_row = if status_on_top { 0 } else { height.saturating_sub(1) };

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
                    cursor::MoveTo(0, (row + text_offset) as u16),
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
            queue!(out, cursor::MoveTo(0, status_row as u16), Clear(ClearType::CurrentLine))?;
            self.render_styled_status_bar(out, &padded_status, width)?;
        }

        if height > 0 {
            let gutter = self.gutter_width() as u16;
            let effective_width = width.saturating_sub(gutter as usize);
            let cx = gutter + self
                .cursor_x
                .saturating_sub(self.offset_x)
                .min(effective_width.saturating_sub(1)) as u16;
            let cy = self
                .cursor_y
                .saturating_sub(self.offset_y)
                .min(text_rows.saturating_sub(1)) as u16;
            queue!(out, cursor::Show, cursor::MoveTo(cx, cy + text_offset as u16))?;
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
        let gutter = self.gutter_width();
        let text_width = width.saturating_sub(gutter);

        for i in 0..text_rows {
            let line_idx = self.offset_y + i;
            if line_idx < self.buffer.len() {
                let mut line = self.visible_plain_text(self.buffer.get_line(line_idx), text_width);
                if gutter > 0 {
                    let lineno = line_idx + 1;
                    let gutter_str = format!("{:>width$} ", lineno, width = gutter - 1);
                    line.insert_str(0, &gutter_str);
                }
                rows.push(line);
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

        let gutter = self.gutter_width();
        let text_width = width.saturating_sub(gutter);

        if gutter > 0 {
            let lineno = line_idx + 1;
            let gutter_str = format!("{:>width$} ", lineno, width = gutter - 1);
            queue!(out, Print(&gutter_str))?;
        }

        let line = self.buffer.get_line(line_idx);

        if self.search_highlight.is_empty() && self.syntax_highlight {
            return self.write_colored(out, line, self.offset_x, text_width);
        }

        let start_byte = char_to_byte_idx(line, self.offset_x);
        let end_byte = char_to_byte_idx(line, self.offset_x + text_width);
        let visible = &line[start_byte..end_byte];

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
                let sbg = self.config.style.bg_at(0, 1);
                let sfg = sbg.text_color();
                queue!(
                    out,
                    SetBackgroundColor(sbg.to_crossterm()),
                    SetForegroundColor(sfg.to_crossterm()),
                    Print(sanitize_str(&visible[abs..end])),
                    ResetColor
                )?;

                idx = end;
            } else {
                queue!(out, Print(sanitize_str(&visible[idx..])))?;
                break;
            }
        }

        Ok(())
    }

    fn render_styled_status_bar(&self, out: &mut Stdout, text: &str, width: usize) -> io::Result<()> {
        let style = &self.config.style;
        match style {
            config::UiStyle::White | config::UiStyle::Dark | config::UiStyle::StaticColor(_) => {
                let bg = style.bg_at(0, width);
                let fg = bg.text_color();
                queue!(out,
                    SetBackgroundColor(bg.to_crossterm()),
                    SetForegroundColor(fg.to_crossterm()),
                    Print(text),
                    ResetColor
                )?;
            }
            _ => {
                for (i, ch) in text.chars().enumerate() {
                    let bg = style.bg_at(i, width);
                    let fg = bg.text_color();
                    queue!(out,
                        SetBackgroundColor(bg.to_crossterm()),
                        SetForegroundColor(fg.to_crossterm()),
                        Print(ch.to_string()),
                    )?;
                }
                queue!(out, ResetColor)?;
            }
        }
        Ok(())
    }

    fn render_styled_box_line(&self, out: &mut Stdout, text: &str, x: usize, y: usize, box_width: usize) -> io::Result<()> {
        let style = &self.config.style;
        let display = if self.config.shelf_3d {
            let inner = &text[..text.len().min(box_width.saturating_sub(2))];
            format!("░{}█", pad_to_width(inner, box_width.saturating_sub(2)))
        } else {
            text.to_string()
        };
        queue!(out, cursor::MoveTo(x as u16, y as u16))?;
        match style {
            config::UiStyle::White | config::UiStyle::Dark | config::UiStyle::StaticColor(_) => {
                let bg = style.bg_at(0, box_width);
                let fg = bg.text_color();
                queue!(out,
                    SetBackgroundColor(bg.to_crossterm()),
                    SetForegroundColor(fg.to_crossterm()),
                    Print(&display),
                )?;
            }
            _ => {
                for (i, ch) in display.chars().enumerate() {
                    let bg = style.bg_at(i, box_width);
                    let fg = if ch == '█' { bg } else { bg.text_color() };
                    queue!(out,
                        SetBackgroundColor(bg.to_crossterm()),
                        SetForegroundColor(fg.to_crossterm()),
                        Print(ch.to_string()),
                    )?;
                }
            }
        }
        queue!(out, ResetColor)?;
        Ok(())
    }

    fn write_colored(&self, out: &mut Stdout, line: &str, offset_chars: usize, width: usize) -> io::Result<()> {
        let segments = tokenize(line, self.language, &self.config.syntax_colors);
        let mut char_pos = 0;
        let visible_end = offset_chars + width;

        for (text, color) in &segments {
            let seg_len = text.chars().count();
            let seg_end = char_pos + seg_len;

            if seg_end <= offset_chars || char_pos >= visible_end {
                char_pos = seg_end;
                continue;
            }

            let start_skip = if char_pos < offset_chars {
                offset_chars - char_pos
            } else {
                0
            };
            let end_trim = if seg_end > visible_end {
                seg_end - visible_end
            } else {
                0
            };
            let trimmed: String = text.chars()
                .skip(start_skip)
                .take(seg_len - start_skip - end_trim)
                .collect();

            if !trimmed.is_empty() {
                if let Some(c) = color {
                    queue!(out, SetForegroundColor(*c), Print(trimmed.as_str()), ResetColor)?;
                } else {
                    queue!(out, Print(trimmed.as_str()))?;
                }
            }

            char_pos = seg_end;
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

    fn gutter_width(&self) -> usize {
        if !self.show_line_numbers {
            return 0;
        }
        let total = self.buffer.len();
        if total <= 1 {
            2 // "1 " minimum space even for single-line files
        } else {
            total.to_string().len() + 1
        }
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

            Language::PlainText | Language::Shell | Language::Markdown => base,
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
            InputMode::FilePicker => {
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

    fn render_simple_picker(&self, out: &mut Stdout, width: usize, height: usize, dir_str: &str) -> io::Result<()> {
        let header = format!(" {}", dir_str);
        let status = "Tab:Manager | ↑/↓ Enter Backspace Esc | S:Config";

        let entries_to_show = cmp::min(self.file_picker_entries.len(), height.saturating_sub(9));
        let empty_msg = if self.file_picker_entries.is_empty() { " (empty directory)" } else { "" };
        let max_entry_width = self.file_picker_entries.iter()
            .map(|e| e.display.len() + 2).max().unwrap_or(0);

        let logo_width = VAN_LOGO.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let content_width = cmp::min(
            cmp::max(
                cmp::max(logo_width, header.chars().count()),
                cmp::max(max_entry_width + empty_msg.len(), status.chars().count()),
            ),
            width,
        );

        let logo_height = VAN_LOGO.len();
        let box_height = logo_height + 1 + 1 + entries_to_show + 1;
        let top_margin = height.saturating_sub(box_height) / 2;
        let left_margin = width.saturating_sub(content_width) / 2;

        let logo_color = self.config.style.logo_color();

        let left_pad = content_width.saturating_sub(logo_width) / 2;
        let logo_indent = " ".repeat(left_pad);
        for (i, logo_line) in VAN_LOGO.iter().enumerate() {
            let line = if self.config.ascii_shadow { *logo_line } else { &logo_line.replace('░', " ") };
            let display = format!("{}{}", logo_indent, line);
            let line_padded = pad_to_width(&display, content_width);
            queue!(out, cursor::MoveTo(left_margin as u16, (top_margin + i) as u16),
                SetForegroundColor(logo_color.to_crossterm()), Print(&line_padded), ResetColor)?;
        }

        let header_padded = pad_to_width(&truncate_to_width(&header, content_width), content_width);
        let header_y = top_margin + logo_height + 1;
        self.render_styled_box_line(out, &header_padded, left_margin, header_y, content_width)?;

        if self.file_picker_entries.is_empty() {
            let line_padded = pad_to_width(&truncate_to_width(empty_msg, content_width), content_width);
            queue!(out, cursor::MoveTo(left_margin as u16, (header_y + 1) as u16), Print(&line_padded))?;
        } else {
            for i in 0..entries_to_show {
                let entry = &self.file_picker_entries[i];
                let prefix = if i == self.file_picker_selection { " >" } else { "  " };
                let line = format!("{}{}", prefix, entry.display);
                let line_padded = pad_to_width(&truncate_to_width(&line, content_width), content_width);
                let y = (header_y + 1 + i) as u16;
                if i == self.file_picker_selection {
                    self.render_styled_box_line(out, &line_padded, left_margin, y as usize, content_width)?;
                } else {
                    queue!(out, cursor::MoveTo(left_margin as u16, y), Print(&line_padded))?;
                }
            }
        }

        let status_padded = pad_to_width(&truncate_to_width(status, content_width), content_width);
        let status_y = (header_y + 1 + entries_to_show) as u16;
        self.render_styled_box_line(out, &status_padded, left_margin, status_y as usize, content_width)?;

        Ok(())
    }

    fn render_manager_picker(&self, out: &mut Stdout, width: usize, height: usize, dir_str: &str) -> io::Result<()> {
        let header = format!(" {}", dir_str);

        let prompt_active = self.prompt_state != PromptState::None;
        let pending_active = self.pending_file_op.is_some();

        let status = if prompt_active {
            match &self.prompt_state {
                PromptState::ConfirmDelete { .. } => "Delete? (y/n)".to_string(),
                PromptState::ConfirmOverwrite { .. } => "Overwrite? (y/n)".to_string(),
                PromptState::InputRename { .. } => format!("Rename to: {}", self.prompt_input),
                PromptState::InputCreateFile => format!("New file: {}", self.prompt_input),
                PromptState::InputCreateDir => format!("New dir: {}", self.prompt_input),
                PromptState::None => unreachable!(),
            }
        } else if pending_active {
            match &self.pending_file_op {
                Some(PendingFileOp::Copy { source }) =>
                    format!("Copy: {} — navigate, press x", source.file_name().unwrap_or_default().to_string_lossy()),
                Some(PendingFileOp::Move { source }) =>
                    format!("Move: {} — navigate, press x", source.file_name().unwrap_or_default().to_string_lossy()),
                None => String::new(),
            }
        } else {
            "h:hidden r:refresh n:file N:dir d:delete R:rename c:copy m:move  Tab:Simple  S:Config".to_string()
        };

        let entries_to_show = cmp::min(self.file_picker_entries.len(), height.saturating_sub(9));
        let empty_msg = if self.file_picker_entries.is_empty() && !prompt_active && !pending_active { " (empty directory)" } else { "" };
        let max_entry_width = self.file_picker_entries.iter()
            .map(|e| e.display.len() + 12 + 2).max().unwrap_or(0);

        let logo_width = VAN_LOGO.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let content_width = cmp::min(
            cmp::max(
                cmp::max(logo_width, header.chars().count()),
                cmp::max(max_entry_width + empty_msg.len(), status.chars().count()),
            ),
            width,
        );

        let logo_height = VAN_LOGO.len();
        let box_height = logo_height + 1 + 1 + entries_to_show + 1;
        let top_margin = height.saturating_sub(box_height) / 2;
        let left_margin = width.saturating_sub(content_width) / 2;

        let logo_color = self.config.style.logo_color();

        let left_pad = content_width.saturating_sub(logo_width) / 2;
        let logo_indent = " ".repeat(left_pad);
        for (i, logo_line) in VAN_LOGO.iter().enumerate() {
            let line = if self.config.ascii_shadow { *logo_line } else { &logo_line.replace('░', " ") };
            let display = format!("{}{}", logo_indent, line);
            let line_padded = pad_to_width(&display, content_width);
            queue!(out, cursor::MoveTo(left_margin as u16, (top_margin + i) as u16),
                SetForegroundColor(logo_color.to_crossterm()), Print(&line_padded), ResetColor)?;
        }

        let header_padded = pad_to_width(&truncate_to_width(&header, content_width), content_width);
        let header_y = top_margin + logo_height + 1;
        self.render_styled_box_line(out, &header_padded, left_margin, header_y, content_width)?;

        if self.file_picker_entries.is_empty() && !prompt_active && !pending_active {
            let line_padded = pad_to_width(&truncate_to_width(empty_msg, content_width), content_width);
            queue!(out, cursor::MoveTo(left_margin as u16, (header_y + 1) as u16), Print(&line_padded))?;
        } else {
            for i in 0..entries_to_show {
                let entry = &self.file_picker_entries[i];
                let prefix = if i == self.file_picker_selection { " >" } else { "  " };
                let size_str = if entry.is_dir { String::new() } else { format!(" {:>10}", format_size(entry.size)) };
                let line = format!("{}{}{}", prefix, entry.display, size_str);
                let line_padded = pad_to_width(&truncate_to_width(&line, content_width), content_width);
                let y = (header_y + 1 + i) as u16;
                if i == self.file_picker_selection {
                    self.render_styled_box_line(out, &line_padded, left_margin, y as usize, content_width)?;
                } else {
                    queue!(out, cursor::MoveTo(left_margin as u16, y), Print(&line_padded))?;
                }
            }
        }

        let status_padded = pad_to_width(&truncate_to_width(&status, content_width), content_width);
        let status_y = (header_y + 1 + entries_to_show) as u16;
        self.render_styled_box_line(out, &status_padded, left_margin, status_y as usize, content_width)?;

        if prompt_active && matches!(self.prompt_state, PromptState::InputRename { .. } | PromptState::InputCreateFile | PromptState::InputCreateDir) {
            queue!(out, cursor::Show)?;
        } else {
            queue!(out, cursor::Hide)?;
        }

        Ok(())
    }

    // ── Config TUI ──────────────────────────────────────────────

    fn render_config_tui(&self, out: &mut Stdout, width: usize, height: usize) -> io::Result<()> {
        let style = &self.config.style;
        let header = " CONFIG ";
        let status_hint = "Esc:Cancel  ↑↓  Enter:Toggle  ←→:Cycle  S:Save";

        let item_count = self.config_tui_item_count();
        let scroll = self.config_tui.scroll;

        let max_item_width = self.max_config_item_width();
        let logo_width = CONFIG_LOGO.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let content_width = cmp::min(
            cmp::max(
                cmp::max(logo_width, header.len()),
                cmp::max(max_item_width, status_hint.len()),
            ),
            width.saturating_sub(4),
        );
        let logo_height = CONFIG_LOGO.len();
        let box_height = logo_height + 2 + 1 + item_count + 1;
        let top_margin = if box_height >= height { 0 } else { (height - box_height) / 2 };
        let left_margin = (width - content_width) / 2;

        // logo centered
        let logo_color = style.logo_color();
        let left_pad = content_width.saturating_sub(logo_width) / 2;
        let logo_indent = " ".repeat(left_pad);
        for (i, logo_line) in CONFIG_LOGO.iter().enumerate() {
            let line = if self.config.ascii_shadow { *logo_line } else { &logo_line.replace('░', " ") };
            let display = format!("{}{}", logo_indent, line);
            let line_padded = pad_to_width(&display, content_width);
            queue!(out, cursor::MoveTo(left_margin as u16, (top_margin + i) as u16),
                SetForegroundColor(logo_color.to_crossterm()), Print(&line_padded), ResetColor)?;
        }

        // header line
        let header_padded = pad_to_width(&truncate_to_width(header, content_width), content_width);
        let header_y = top_margin + logo_height + 1;
        self.render_styled_box_line(out, &header_padded, left_margin, header_y, content_width)?;

        // items
        let mut rendered = 0usize;
        for item_idx in scroll..item_count {
            let screen_y = header_y + 1 + rendered;
            if screen_y >= height.saturating_sub(1) { break; }
            rendered += 1;
            let is_focused = item_idx == self.config_tui.cursor;
            let prefix = if is_focused { " >" } else { "  " };
            let line = match self.config_tui_item(item_idx) {
                Some(item) => self.config_tui_item_render(item, content_width.saturating_sub(4)),
                None => String::new(),
            };
            let display = format!("{}{}", prefix, line);
            let line_padded = pad_to_width(&truncate_to_width(&display, content_width), content_width);
            if is_focused {
                self.render_styled_box_line(out, &line_padded, left_margin, screen_y, content_width)?;
            } else {
                queue!(out, cursor::MoveTo(left_margin as u16, screen_y as u16), Print(&line_padded))?;
            }
        }

        // status line right after items
        let status_y = header_y + 1 + rendered;
        let status_padded = pad_to_width(&truncate_to_width(status_hint, content_width), content_width);
        self.render_styled_box_line(out, &status_padded, left_margin, status_y, content_width)?;

        Ok(())
    }

    fn max_config_item_width(&self) -> usize {
        let mut max_w = 0usize;
        for i in 0..self.config_tui_item_count() {
            if let Some(item) = self.config_tui_item(i) {
                let w = self.config_tui_item_render(item, 1000).len() + 4;
                if w > max_w { max_w = w; }
            }
        }
        max_w
    }

    fn config_tui_item_count(&self) -> usize {
        let mut n = 5;
        if self.config_tui.expanded[0] { n += 1; }
        if self.config_tui.expanded[1] { n += 1 + 4; }
        if self.config_tui.expanded[2] { n += 14; }
        if self.config_tui.expanded[3] { n += 5; }
        if self.config_tui.expanded[4] { n += 2; }
        n += 3;
        n
    }

    fn config_tui_item(&self, idx: usize) -> Option<ConfigTuiItem> {
        let mut i = 0;
        macro_rules! check { ($val:expr) => { if i == idx { return Some($val); } i += 1; }; }
        check!(ConfigTuiItem::Section(0));
        if self.config_tui.expanded[0] { check!(ConfigTuiItem::Style); }
        check!(ConfigTuiItem::Section(1));
        if self.config_tui.expanded[1] {
            check!(ConfigTuiItem::Position);
            for t in 0..4 { check!(ConfigTuiItem::ContentToggle(t)); }
        }
        check!(ConfigTuiItem::Section(2));
        if self.config_tui.expanded[2] {
            for c in 0..14 { check!(ConfigTuiItem::ColorField(c)); }
        }
        check!(ConfigTuiItem::Section(3));
        if self.config_tui.expanded[3] {
            for k in 0..5 { check!(ConfigTuiItem::KeybindField(k)); }
        }
        check!(ConfigTuiItem::Section(4));
        if self.config_tui.expanded[4] {
            for t in 0..2 { check!(ConfigTuiItem::MenuToggle(t)); }
        }
        check!(ConfigTuiItem::Button("Load Defaults"));
        check!(ConfigTuiItem::Button("Save & Exit"));
        check!(ConfigTuiItem::Button("Cancel"));
        None
    }

    fn config_tui_item_render(&self, item: ConfigTuiItem, max_w: usize) -> String {
        let trunc = |s: &str| truncate_to_width(s, max_w);
        match item {
            ConfigTuiItem::Section(idx) => {
                let name = ["Theme", "Status Bar", "Syntax Colors", "Keybindings", "Menu Config"][idx];
                let arrow = if self.config_tui.expanded[idx] { "▼" } else { "▶" };
                trunc(&format!(" {} {}", arrow, name))
            }
            ConfigTuiItem::Style => {
                let display = if matches!(self.config_tui.edit_mode, Some(ConfigEditMode::StyleField)) {
                    let buf = &self.config_tui.edit_buffer;
                    let pos = self.config_tui.edit_cursor.min(buf.len());
                    let (left, right) = buf.split_at(pos);
                    format!("{}|{}", left, right)
                } else {
                    let cur = &self.config.style;
                    let base = cur.style_string();
                    let detail = match cur {
                        config::UiStyle::StaticColor(c) => format!(":{}", c.hex_str()),
                        config::UiStyle::SmoothGradient { from, to } | config::UiStyle::RoughGradient { from, to } => {
                            format!(":{}:{}", from.hex_str(), to.hex_str())
                        }
                        _ => String::new(),
                    };
                    format!("{}{}", base, detail)
                };
                trunc(&format!(" Style       {}", display))
            }
            ConfigTuiItem::Position => {
                let s = match self.config.status_bar_position {
                    config::StatusBarPosition::Bottom => "bottom",
                    config::StatusBarPosition::Top => "top",
                };
                trunc(&format!(" Position    {}", s))
            }
            ConfigTuiItem::ContentToggle(idx) => {
                let labels = ["filename", "git", "time", "binds"];
                let checked = self.config.status_bar_content.contains(&labels[idx].to_string());
                trunc(&format!("   [{}] {}", if checked { "x" } else { " " }, labels[idx]))
            }
            ConfigTuiItem::ColorField(idx) => {
                let labels = ["comment", "string_double", "string_single", "number",
                    "keyword", "type_name", "builtin", "decorator", "variable",
                    "lifetime", "markdown_heading", "markdown_bold", "markdown_code", "markdown_link"];
                let val = if matches!(self.config_tui.edit_mode, Some(ConfigEditMode::ColorField(i)) if i == idx) {
                    let buf = &self.config_tui.edit_buffer;
                    let pos = self.config_tui.edit_cursor.min(buf.len());
                    let (left, right) = buf.split_at(pos);
                    format!("{}|{}", left, right)
                } else {
                    color_display_name(self.color_field_value(idx))
                };
                trunc(&format!(" {:<16} {}", labels[idx], val))
            }
            ConfigTuiItem::KeybindField(idx) => {
                let actions = [config::EditorAction::Save, config::EditorAction::Find,
                    config::EditorAction::Undo, config::EditorAction::ToggleLineNumbers,
                    config::EditorAction::Exit];
                let names = ["save", "find", "undo", "lines", "exit"];
                let key = self.config.keybindings.key_for_action(actions[idx]);
                trunc(&format!(" {:<16} {}", names[idx], key))
            }
            ConfigTuiItem::MenuToggle(idx) => {
                let labels = ["ASCII shadow effect", "Shelf-/3d files effect"];
                let val = match idx {
                    0 => self.config.ascii_shadow,
                    _ => self.config.shelf_3d,
                };
                let check = if val { "x" } else { " " };
                trunc(&format!("   [{}] {}", check, labels[idx]))
            }
            ConfigTuiItem::Button(label) => trunc(&format!(" [{}]", label)),
        }
    }

    fn color_field_value(&self, idx: usize) -> Option<crossterm::style::Color> {
        let sc = &self.config.syntax_colors;
        match idx {
            0 => sc.comment, 1 => sc.string_double, 2 => sc.string_single,
            3 => sc.number, 4 => sc.keyword, 5 => sc.type_name,
            6 => sc.builtin, 7 => sc.decorator, 8 => sc.variable,
            9 => sc.lifetime, 10 => sc.markdown_heading, 11 => sc.markdown_bold,
            12 => sc.markdown_code, 13 => sc.markdown_link,
            _ => None,
        }
    }

    fn set_color_field(&mut self, idx: usize, color: Option<crossterm::style::Color>) {
        let sc = &mut self.config.syntax_colors;
        match idx {
            0 => sc.comment = color, 1 => sc.string_double = color,
            2 => sc.string_single = color, 3 => sc.number = color,
            4 => sc.keyword = color, 5 => sc.type_name = color,
            6 => sc.builtin = color, 7 => sc.decorator = color,
            8 => sc.variable = color, 9 => sc.lifetime = color,
            10 => sc.markdown_heading = color, 11 => sc.markdown_bold = color,
            12 => sc.markdown_code = color, 13 => sc.markdown_link = color,
            _ => {}
        }
    }

    fn handle_config_tui_key(&mut self, key: KeyEvent) -> bool {
        if let Some(ref mode) = self.config_tui.edit_mode.clone() {
            match mode {
                ConfigEditMode::ColorField(idx) => {
                    let idx = *idx;
                    match key.code {
                        KeyCode::Esc => {
                            self.config_tui.edit_mode = None;
                            self.config_tui.edit_buffer.clear();
                            self.request_full_redraw();
                            return false;
                        }
                        KeyCode::Enter => {
                            let val = self.config_tui.edit_buffer.trim().to_string();
                            if val.is_empty() || val.eq_ignore_ascii_case("no") || val.eq_ignore_ascii_case("none") || val == "null" {
                                self.set_color_field(idx, None);
                            } else if let Some(c) = parse_color_name(&val) {
                                self.set_color_field(idx, Some(c));
                            }
                            self.config_tui.edit_mode = None;
                            self.config_tui.edit_buffer.clear();
                            self.request_full_redraw();
                            return false;
                        }
                        KeyCode::Left => {
                            if self.config_tui.edit_cursor > 0 {
                                self.config_tui.edit_cursor -= 1;
                                self.request_redraw();
                            }
                        }
                        KeyCode::Right => {
                            if self.config_tui.edit_cursor < self.config_tui.edit_buffer.len() {
                                self.config_tui.edit_cursor += 1;
                                self.request_redraw();
                            }
                        }
                        KeyCode::Backspace => {
                            if self.config_tui.edit_cursor > 0 {
                                let pos = self.config_tui.edit_cursor;
                                self.config_tui.edit_buffer.remove(pos - 1);
                                self.config_tui.edit_cursor -= 1;
                                self.request_redraw();
                            }
                        }
                        KeyCode::Char(c) => {
                            let pos = self.config_tui.edit_cursor;
                            self.config_tui.edit_buffer.insert(pos, c);
                            self.config_tui.edit_cursor += 1;
                            self.request_redraw();
                        }
                        _ => {}
                    }
                    return false;
                }
                ConfigEditMode::StyleField => {
                    match key.code {
                        KeyCode::Esc => {
                            self.config_tui.edit_mode = None;
                            self.config_tui.edit_buffer.clear();
                            self.request_full_redraw();
                            return false;
                        }
                        KeyCode::Enter => {
                            let val = self.config_tui.edit_buffer.trim().to_string();
                            if !val.is_empty() {
                                self.config.style = config::UiStyle::parse(&val);
                            }
                            self.config_tui.edit_mode = None;
                            self.config_tui.edit_buffer.clear();
                            self.request_full_redraw();
                            return false;
                        }
                        KeyCode::Left => {
                            if self.config_tui.edit_cursor > 0 {
                                self.config_tui.edit_cursor -= 1;
                                self.request_redraw();
                            }
                        }
                        KeyCode::Right => {
                            if self.config_tui.edit_cursor < self.config_tui.edit_buffer.len() {
                                self.config_tui.edit_cursor += 1;
                                self.request_redraw();
                            }
                        }
                        KeyCode::Backspace => {
                            if self.config_tui.edit_cursor > 0 {
                                let pos = self.config_tui.edit_cursor;
                                self.config_tui.edit_buffer.remove(pos - 1);
                                self.config_tui.edit_cursor -= 1;
                                self.request_redraw();
                            }
                        }
                        KeyCode::Char(c) => {
                            let pos = self.config_tui.edit_cursor;
                            self.config_tui.edit_buffer.insert(pos, c);
                            self.config_tui.edit_cursor += 1;
                            self.request_redraw();
                        }
                        _ => {}
                    }
                    return false;
                }
                ConfigEditMode::KeybindField(action_name) => {
                    let action = config::EditorAction::parse(action_name);
                    match key.code {
                        KeyCode::Esc => {
                            self.config_tui.edit_mode = None;
                            self.request_full_redraw();
                            return false;
                        }
                        _ => {
                            if let (Some(action), Some(lookup)) = (action, key_event_to_string(&key)) {
                                self.config.keybindings.set_binding(&lookup, action);
                            }
                            self.config_tui.edit_mode = None;
                            self.request_full_redraw();
                            return false;
                        }
                    }
                }
            }
        }

        match key.code {
            KeyCode::Up => {
                if self.config_tui.cursor > 0 {
                    self.config_tui.cursor -= 1;
                    self.request_full_redraw();
                }
            }
            KeyCode::Down => {
                let max = self.config_tui_item_count().saturating_sub(1);
                if self.config_tui.cursor < max {
                    self.config_tui.cursor += 1;
                    self.request_full_redraw();
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.activate_config_tui_item(self.config_tui.cursor);
                self.request_full_redraw();
            }
            KeyCode::Left => {
                self.cycle_config_tui_item(self.config_tui.cursor, -1);
                self.request_full_redraw();
            }
            KeyCode::Right => {
                self.cycle_config_tui_item(self.config_tui.cursor, 1);
                self.request_full_redraw();
            }
            KeyCode::Esc => {
                self.config = config::load_config();
                self.mode = InputMode::FilePicker;
                let _ = execute!(stdout(), event::DisableMouseCapture);
                self.request_full_redraw();
            }
            _ => {}
        }
        false
    }

    fn activate_config_tui_item(&mut self, idx: usize) {
        match self.config_tui_item(idx) {
            Some(ConfigTuiItem::Section(s)) => {
                self.config_tui.expanded[s] = !self.config_tui.expanded[s];
            }
            Some(ConfigTuiItem::ContentToggle(t)) => {
                let labels = ["filename", "git", "time", "binds"];
                let label = labels[t].to_string();
                if self.config.status_bar_content.contains(&label) {
                    self.config.status_bar_content.retain(|x| *x != label);
                    if self.config.status_bar_content.is_empty() {
                        self.config.status_bar_content.push("filename".to_string());
                    }
                } else {
                    self.config.status_bar_content.push(label);
                }
            }
            Some(ConfigTuiItem::Style) => {
                let cur = &self.config.style;
                let base = cur.style_string();
                let detail = match cur {
                    config::UiStyle::StaticColor(c) => format!(":{}", c.hex_str()),
                    config::UiStyle::SmoothGradient { from, to } | config::UiStyle::RoughGradient { from, to } => {
                        format!(":{}:{}", from.hex_str(), to.hex_str())
                    }
                    _ => String::new(),
                };
                self.config_tui.edit_mode = Some(ConfigEditMode::StyleField);
                self.config_tui.edit_buffer = format!("{}{}", base, detail);
            }
            Some(ConfigTuiItem::ColorField(idx)) => {
                self.config_tui.edit_mode = Some(ConfigEditMode::ColorField(idx));
                self.config_tui.edit_buffer = color_display_name(self.color_field_value(idx));
            }
            Some(ConfigTuiItem::KeybindField(idx)) => {
                let names = ["save", "find", "undo", "lines", "exit"];
                self.config_tui.edit_mode = Some(ConfigEditMode::KeybindField(names[idx].to_string()));
                self.config_tui.edit_buffer.clear();
            }
            Some(ConfigTuiItem::MenuToggle(idx)) => {
                match idx {
                    0 => self.config.ascii_shadow = !self.config.ascii_shadow,
                    _ => self.config.shelf_3d = !self.config.shelf_3d,
                }
            }
            Some(ConfigTuiItem::Button(label)) => {
                match label {
                    "Load Defaults" => self.config = config::VanConfig::default(),
                    "Save & Exit" => { self.save_config_to_disk(); self.mode = InputMode::FilePicker; let _ = execute!(stdout(), event::DisableMouseCapture); }
                    "Cancel" => { self.config = config::load_config(); self.mode = InputMode::FilePicker; let _ = execute!(stdout(), event::DisableMouseCapture); }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn cycle_config_tui_item(&mut self, idx: usize, dir: isize) {
        match self.config_tui_item(idx) {
            Some(ConfigTuiItem::Style) => {
                let styles = ["white", "dark", "miami", "smooth_gradient:ff0000:00ff00", "rough_gradient:0000ff:00ff00", "static_color:ff6600"];
                let cur = self.config.style.style_string();
                let pos = styles.iter().position(|s| {
                    s.starts_with(cur) || (cur == "smooth_gradient" && s.starts_with("smooth_gradient"))
                        || (cur == "rough_gradient" && s.starts_with("rough_gradient"))
                        || (cur == "static_color" && s.starts_with("static_color"))
                }).unwrap_or(0);
                let next = (pos as isize + dir).rem_euclid(styles.len() as isize) as usize;
                self.config.style = config::UiStyle::parse(styles[next]);
            }
            Some(ConfigTuiItem::Position) => {
                self.config.status_bar_position = if self.config.status_bar_position == config::StatusBarPosition::Bottom {
                    config::StatusBarPosition::Top
                } else {
                    config::StatusBarPosition::Bottom
                };
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                match self.mode {
                    InputMode::FilePicker => {
                        self.file_picker_selection = self.file_picker_selection.saturating_sub(1);
                        self.request_redraw();
                    }
                    InputMode::ConfigTui => {
                        if self.config_tui.cursor > 0 {
                            self.config_tui.cursor -= 1;
                            self.request_full_redraw();
                        }
                    }
                    _ => {
                        if self.cursor_y > 0 {
                            self.cursor_y -= 1;
                            self.request_redraw();
                        }
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                match self.mode {
                    InputMode::FilePicker => {
                        let max = self.file_picker_entries.len().saturating_sub(1);
                        self.file_picker_selection = cmp::min(self.file_picker_selection + 1, max);
                        self.request_redraw();
                    }
                    InputMode::ConfigTui => {
                        let max_cursor = self.config_tui_item_count().saturating_sub(1);
                        if self.config_tui.cursor < max_cursor {
                            self.config_tui.cursor += 1;
                            self.request_full_redraw();
                        }
                    }
                    _ => {
                        if self.cursor_y + 1 < self.buffer.len() {
                            self.cursor_y += 1;
                            self.request_redraw();
                        }
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) if self.mode == InputMode::ConfigTui => {
                let screen_y = mouse.row as usize;
                let height = self.last_size.1 as usize;
                if screen_y >= height { return false; }
                let logo_height = CONFIG_LOGO.len();
                let item_count = self.config_tui_item_count();
                let box_height = logo_height + 2 + 1 + item_count + 1;
                let top_margin = if box_height >= height { 0 } else { (height - box_height) / 2 };
                let item_start_y = top_margin + logo_height + 2;
                if screen_y >= item_start_y {
                    let item_idx = self.config_tui.scroll + screen_y - item_start_y;
                    if item_idx < self.config_tui_item_count() {
                        self.config_tui.cursor = item_idx;
                        self.activate_config_tui_item(item_idx);
                        self.request_full_redraw();
                    }
                }
            }
            _ => {}
        }
        false
    }

    fn save_config_to_disk(&self) {
        let sc = &self.config.syntax_colors;
        let color_val = |c: Option<crossterm::style::Color>| -> serde_json::Value {
            match c {
                None => serde_json::Value::Null,
                Some(c) => serde_json::Value::String(color_display_name(Some(c))),
            }
        };
        let pos_str = match self.config.status_bar_position {
            config::StatusBarPosition::Bottom => "bottom",
            config::StatusBarPosition::Top => "top",
        };
        let content: Vec<serde_json::Value> = self.config.status_bar_content.iter()
            .map(|s| serde_json::Value::String(s.clone())).collect();
        let mut kb_map = serde_json::Map::new();
        let actions = [
            (config::EditorAction::Save, "save"),
            (config::EditorAction::Find, "find"),
            (config::EditorAction::Undo, "undo"),
            (config::EditorAction::ToggleLineNumbers, "lines"),
            (config::EditorAction::Exit, "exit"),
        ];
        for (action, name) in &actions {
            kb_map.insert(name.to_string(), serde_json::Value::String(self.config.keybindings.key_for_action(*action)));
        }
        let style_str = {
            let cur = &self.config.style;
            let base = cur.style_string();
            match cur {
                config::UiStyle::StaticColor(c) => format!("{}:{}", base, c.hex_str()),
                config::UiStyle::SmoothGradient { from, to } | config::UiStyle::RoughGradient { from, to } => {
                    format!("{}:{}:{}", base, from.hex_str(), to.hex_str())
                }
                _ => base.to_string(),
            }
        };
        let json = serde_json::json!({
            "theme": {
                "style": style_str,
                "status_bar": {
                    "position": pos_str,
                    "content": content,
                },
                "ascii_shadow": self.config.ascii_shadow,
                "shelf_3d": self.config.shelf_3d,
            },
            "syntax": {
                "comment": color_val(sc.comment), "string_double": color_val(sc.string_double),
                "string_single": color_val(sc.string_single), "number": color_val(sc.number),
                "keyword": color_val(sc.keyword), "type_name": color_val(sc.type_name),
                "builtin": color_val(sc.builtin), "decorator": color_val(sc.decorator),
                "variable": color_val(sc.variable), "lifetime": color_val(sc.lifetime),
                "markdown_heading": color_val(sc.markdown_heading), "markdown_bold": color_val(sc.markdown_bold),
                "markdown_code": color_val(sc.markdown_code), "markdown_link": color_val(sc.markdown_link),
            },
            "keybindings": kb_map,
        });
        if let Some(path) = config::config_path() {
            if let Some(parent) = path.parent() { let _ = fs::create_dir_all(parent); }
            if let Ok(content) = serde_json::to_string_pretty(&json) { let _ = fs::write(&path, content); }
        }
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

fn color_display_name(c: Option<crossterm::style::Color>) -> String {
    use crossterm::style::Color;
    match c {
        None => "no".to_string(),
        Some(Color::Black) => "black".to_string(),
        Some(Color::DarkGrey) => "dark_grey".to_string(),
        Some(Color::Red) => "red".to_string(),
        Some(Color::DarkRed) => "dark_red".to_string(),
        Some(Color::Green) => "green".to_string(),
        Some(Color::DarkGreen) => "dark_green".to_string(),
        Some(Color::Yellow) => "yellow".to_string(),
        Some(Color::DarkYellow) => "dark_yellow".to_string(),
        Some(Color::Blue) => "blue".to_string(),
        Some(Color::DarkBlue) => "dark_blue".to_string(),
        Some(Color::Magenta) => "magenta".to_string(),
        Some(Color::DarkMagenta) => "dark_magenta".to_string(),
        Some(Color::Cyan) => "cyan".to_string(),
        Some(Color::DarkCyan) => "dark_cyan".to_string(),
        Some(Color::White) => "white".to_string(),
        Some(Color::Grey) => "grey".to_string(),
        Some(Color::Rgb { r, g, b }) => format!("#{:02x}{:02x}{:02x}", r, g, b),
        Some(Color::AnsiValue(v)) => format!("ansi({})", v),
        Some(Color::Reset) => "reset".to_string(),
    }
}

fn parse_color_name(s: &str) -> Option<crossterm::style::Color> {
    use crossterm::style::Color;
    match s.trim().to_ascii_lowercase().replace(['-', '_'], "").as_str() {
        "reset" => Some(Color::Reset),
        "black" => Some(Color::Black),
        "darkgrey" | "darkgray" => Some(Color::DarkGrey),
        "red" => Some(Color::Red),
        "darkred" => Some(Color::DarkRed),
        "green" => Some(Color::Green),
        "darkgreen" => Some(Color::DarkGreen),
        "yellow" => Some(Color::Yellow),
        "darkyellow" => Some(Color::DarkYellow),
        "blue" => Some(Color::Blue),
        "darkblue" => Some(Color::DarkBlue),
        "magenta" => Some(Color::Magenta),
        "darkmagenta" => Some(Color::DarkMagenta),
        "cyan" => Some(Color::Cyan),
        "darkcyan" => Some(Color::DarkCyan),
        "white" => Some(Color::White),
        "grey" | "gray" => Some(Color::Grey),
        _ => {
            let hex = s.trim_start_matches('#');
            if hex.len() == 6 {
                if let (Ok(r), Ok(g), Ok(b)) = (
                    u8::from_str_radix(&hex[0..2], 16),
                    u8::from_str_radix(&hex[2..4], 16),
                    u8::from_str_radix(&hex[4..6], 16),
                ) {
                    return Some(Color::Rgb { r, g, b });
                }
            }
            None
        }
    }
}

fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[unit_idx])
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}
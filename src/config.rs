use std::collections::HashMap;
use std::path::PathBuf;
use std::{env, fs};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.trim_start_matches('#');
        if hex.len() != 6 { return None; }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(Self { r, g, b })
    }

    pub fn lerp(&self, other: &Self, t: f64) -> Self {
        Self {
            r: (self.r as f64 + (other.r as f64 - self.r as f64) * t).round() as u8,
            g: (self.g as f64 + (other.g as f64 - self.g as f64) * t).round() as u8,
            b: (self.b as f64 + (other.b as f64 - self.b as f64) * t).round() as u8,
        }
    }

    pub fn luminance(&self) -> f64 {
        0.299 * self.r as f64 + 0.587 * self.g as f64 + 0.114 * self.b as f64
    }

    pub fn text_color(&self) -> Self {
        if self.luminance() > 128.0 {
            Self { r: 0, g: 0, b: 0 }
        } else {
            Self { r: 255, g: 255, b: 255 }
        }
    }

    pub fn to_crossterm(&self) -> crossterm::style::Color {
        crossterm::style::Color::Rgb { r: self.r, g: self.g, b: self.b }
    }

    pub fn hex_str(&self) -> String {
        format!("{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

#[derive(Debug, Clone)]
pub enum UiStyle {
    White,
    Dark,
    Miami,
    SmoothGradient { from: Rgb, to: Rgb },
    RoughGradient { from: Rgb, to: Rgb },
    StaticColor(Rgb),
}

impl UiStyle {
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        match s {
            "white" => return Self::White,
            "dark" => return Self::Dark,
            "miami" => return Self::Miami,
            _ => {}
        }
        if let Some(hex) = s.strip_prefix("static_color:") {
            if let Some(rgb) = Rgb::from_hex(hex) {
                return Self::StaticColor(rgb);
            }
        }
        if let Some(rest) = s.strip_prefix("smooth_gradient:") {
            if let Some((a, b)) = rest.split_once(':') {
                if let (Some(from), Some(to)) = (Rgb::from_hex(a), Rgb::from_hex(b)) {
                    return Self::SmoothGradient { from, to };
                }
            }
        }
        if let Some(rest) = s.strip_prefix("rough_gradient:") {
            if let Some((a, b)) = rest.split_once(':') {
                if let (Some(from), Some(to)) = (Rgb::from_hex(a), Rgb::from_hex(b)) {
                    return Self::RoughGradient { from, to };
                }
            }
        }
        Self::Dark
    }

    pub fn bg_at(&self, index: usize, total: usize) -> Rgb {
        match self {
            Self::White => Rgb { r: 255, g: 255, b: 255 },
            Self::Dark => Rgb { r: 26, g: 26, b: 26 },
            Self::Miami => {
                let t = if total <= 1 { 0.0 } else { index as f64 / (total - 1) as f64 };
                let orange = Rgb { r: 255, g: 107, b: 53 };
                let pink = Rgb { r: 255, g: 20, b: 147 };
                orange.lerp(&pink, t)
            }
            Self::SmoothGradient { from, to } => {
                let t = if total <= 1 { 0.0 } else { index as f64 / (total - 1) as f64 };
                from.lerp(to, t)
            }
            Self::RoughGradient { from, to } => {
                let bands = 8.min(total);
                let step = total.saturating_sub(1) / bands.max(1);
                let band = index / step.max(1);
                let t = band as f64 / bands.max(1) as f64;
                from.lerp(to, t)
            }
            Self::StaticColor(c) => *c,
        }
    }

    pub fn text_at(&self, index: usize, total: usize) -> Rgb {
        self.bg_at(index, total).text_color()
    }

    pub fn logo_color(&self) -> Rgb {
        match self {
            Self::White => Rgb { r: 0, g: 120, b: 255 },
            Self::Dark => Rgb { r: 100, g: 200, b: 255 },
            Self::Miami => {
                let orange = Rgb { r: 255, g: 107, b: 53 };
                let pink = Rgb { r: 255, g: 20, b: 147 };
                orange.lerp(&pink, 0.5)
            }
            Self::SmoothGradient { from, to } => from.lerp(to, 0.5),
            Self::RoughGradient { from, to } => from.lerp(to, 0.5),
            Self::StaticColor(c) => c.text_color(),
        }
    }

    pub fn style_string(&self) -> &'static str {
        match self {
            Self::White => "white",
            Self::Dark => "dark",
            Self::Miami => "miami",
            Self::SmoothGradient { .. } => "smooth_gradient",
            Self::RoughGradient { .. } => "rough_gradient",
            Self::StaticColor(_) => "static_color",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StatusBarPosition {
    Bottom,
    Top,
}

impl StatusBarPosition {
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "top" => Self::Top,
            _ => Self::Bottom,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EditorAction {
    Save,
    Find,
    Undo,
    ToggleLineNumbers,
    Exit,
}

impl EditorAction {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "save" => Self::Save,
            "find" => Self::Find,
            "undo" => Self::Undo,
            "lines" | "toggle_line_numbers" | "linenumbers" => Self::ToggleLineNumbers,
            "exit" | "quit" => Self::Exit,
            _ => return None,
        })
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Save => "Save",
            Self::Find => "Find",
            Self::Undo => "Undo",
            Self::ToggleLineNumbers => "Lines",
            Self::Exit => "Exit",
        }
    }
}

#[derive(Debug, Clone)]
pub struct KeyBindings {
    map: HashMap<String, EditorAction>,
}

impl KeyBindings {
    pub fn lookup(&self, key: &str) -> Option<EditorAction> {
        self.map.get(key).copied()
    }

    pub fn key_for_action(&self, action: EditorAction) -> String {
        for (key, act) in &self.map {
            if *act == action {
                return key.clone();
            }
        }
        match action {
            EditorAction::Save => "ctrl+s",
            EditorAction::Find => "ctrl+f",
            EditorAction::Undo => "ctrl+z",
            EditorAction::ToggleLineNumbers => "ctrl+l",
            EditorAction::Exit => "ctrl+x",
        }.to_string()
    }

    pub fn set_binding(&mut self, new_key: &str, action: EditorAction) {
        // Remove old key for this action
        let old_keys: Vec<String> = self.map.iter()
            .filter(|(_, v)| **v == action)
            .map(|(k, _)| k.clone())
            .collect();
        for k in old_keys {
            self.map.remove(&k);
        }
        // Remove any action bound to this key (collision)
        self.map.retain(|_, v| *v != action);
        self.map.insert(new_key.to_string(), action);
    }

    pub fn display_binds(&self) -> String {
        let mut pairs: Vec<(&str, &EditorAction)> = self.map.iter().map(|(k, v)| (k.as_str(), v)).collect();
        pairs.sort_by(|a, b| a.1.display_name().cmp(b.1.display_name()));
        pairs.iter()
            .map(|(key, action)| format!("{} {}", key, action.display_name()))
            .collect::<Vec<_>>()
            .join(" | ")
    }

    fn insert_default(&mut self, key: &str, action: EditorAction) {
        self.map.entry(key.to_string()).or_insert(action);
    }

    fn insert_override(&mut self, key: &str, action_str: &str) {
        if let Some(action) = EditorAction::parse(action_str) {
            self.map.insert(key.to_string(), action);
        }
    }
}

impl Default for KeyBindings {
    fn default() -> Self {
        let map = HashMap::new();
        let mut kb = Self { map };
        kb.insert_default("ctrl+s", EditorAction::Save);
        kb.insert_default("ctrl+f", EditorAction::Find);
        kb.insert_default("ctrl+z", EditorAction::Undo);
        kb.insert_default("ctrl+l", EditorAction::ToggleLineNumbers);
        kb.insert_default("ctrl+x", EditorAction::Exit);
        kb
    }
}

pub fn parse_keybindings(obj: &serde_json::Map<String, serde_json::Value>) -> KeyBindings {
    let mut kb = KeyBindings::default();
    for (key, val) in obj {
        if let Some(action_str) = val.as_str() {
            kb.insert_override(key, action_str);
        }
    }
    kb
}

fn parse_color(s: &serde_json::Value) -> Option<Option<crossterm::style::Color>> {
    if s.is_null() { return Some(None); }
    let s = s.as_str()?;
    if s.eq_ignore_ascii_case("no") || s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("null") {
        return Some(None);
    }
    use crossterm::style::Color;
    let named = match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
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
        _ => None,
    };
    if let Some(c) = named {
        return Some(Some(c));
    }
    let hex = s.trim_start_matches('#');
    if hex.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&hex[0..2], 16),
            u8::from_str_radix(&hex[2..4], 16),
            u8::from_str_radix(&hex[4..6], 16),
        ) {
            return Some(Some(Color::Rgb { r, g, b }));
        }
    }
    None
}

fn parse_syntax_colors(obj: &serde_json::Map<String, serde_json::Value>) -> SyntaxColors {
    let mut c = SyntaxColors::default();
    for (key, val) in obj {
        if let Some(parsed) = parse_color(val) {
            match key.as_str() {
                "comment" => c.comment = parsed,
                "string_double" | "string" => c.string_double = parsed,
                "string_single" => c.string_single = parsed,
                "number" => c.number = parsed,
                "keyword" => c.keyword = parsed,
                "type_name" | "type" => c.type_name = parsed,
                "builtin" => c.builtin = parsed,
                "decorator" => c.decorator = parsed,
                "variable" => c.variable = parsed,
                "lifetime" => c.lifetime = parsed,
                "markdown_heading" => c.markdown_heading = parsed,
                "markdown_bold" => c.markdown_bold = parsed,
                "markdown_code" => c.markdown_code = parsed,
                "markdown_link" => c.markdown_link = parsed,
                _ => {}
            }
        }
    }
    c
}

#[derive(Debug, Clone)]
pub struct SyntaxColors {
    pub comment: Option<crossterm::style::Color>,
    pub string_double: Option<crossterm::style::Color>,
    pub string_single: Option<crossterm::style::Color>,
    pub number: Option<crossterm::style::Color>,
    pub keyword: Option<crossterm::style::Color>,
    pub type_name: Option<crossterm::style::Color>,
    pub builtin: Option<crossterm::style::Color>,
    pub decorator: Option<crossterm::style::Color>,
    pub variable: Option<crossterm::style::Color>,
    pub lifetime: Option<crossterm::style::Color>,
    pub markdown_heading: Option<crossterm::style::Color>,
    pub markdown_bold: Option<crossterm::style::Color>,
    pub markdown_code: Option<crossterm::style::Color>,
    pub markdown_link: Option<crossterm::style::Color>,
}

impl Default for SyntaxColors {
    fn default() -> Self {
        use crossterm::style::Color;
        Self {
            comment: Some(Color::DarkGreen),
            string_double: Some(Color::Yellow),
            string_single: Some(Color::Green),
            number: Some(Color::Magenta),
            keyword: Some(Color::Blue),
            type_name: Some(Color::Cyan),
            builtin: Some(Color::DarkYellow),
            decorator: Some(Color::Cyan),
            variable: Some(Color::DarkYellow),
            lifetime: Some(Color::Cyan),
            markdown_heading: Some(Color::Blue),
            markdown_bold: Some(Color::DarkYellow),
            markdown_code: Some(Color::Green),
            markdown_link: Some(Color::Cyan),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VanConfig {
    pub style: UiStyle,
    pub status_bar_position: StatusBarPosition,
    pub status_bar_content: Vec<String>,
    pub syntax_colors: SyntaxColors,
    pub keybindings: KeyBindings,
}

impl Default for VanConfig {
    fn default() -> Self {
        Self {
            style: UiStyle::White,
            status_bar_position: StatusBarPosition::Bottom,
            status_bar_content: vec!["filename".to_string(), "binds".to_string()],
            syntax_colors: SyntaxColors::default(),
            keybindings: KeyBindings::default(),
        }
    }
}

pub fn config_dir() -> Option<PathBuf> {
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(xdg))
    } else if let Some(home) = env::var_os("HOME") {
        Some(PathBuf::from(home).join(".config"))
    } else {
        env::var_os("USERPROFILE").map(PathBuf::from)
    }
}

pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("van").join("config.json"))
}

fn strip_json_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut i = 0;
    let chars: Vec<char> = input.chars().collect();
    while i < chars.len() {
        if chars[i] == '"' {
            in_string = !in_string;
            out.push('"');
            i += 1;
        } else if !in_string && i + 1 < chars.len() && chars[i] == '/' && chars[i + 1] == '/' {
            i += 2;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn remove_trailing_commas(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' {
            in_string = !in_string;
            out.push('"');
            i += 1;
        } else if !in_string && chars[i] == ',' {
            let mut j = i + 1;
            while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t' || chars[j] == '\n' || chars[j] == '\r') {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                i += 1;
            } else {
                out.push(',');
                i += 1;
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn load_raw_json() -> Option<serde_json::Value> {
    let path = config_path()?;
    let raw = fs::read_to_string(path).ok()?;
    let cleaned = strip_json_comments(&raw);
    let cleaned = remove_trailing_commas(&cleaned);
    serde_json::from_str(&cleaned).ok()
}

fn write_default_config() {
    if let Some(path) = config_path() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let content = r#"{
  // van editor config

  "theme": {
    "style": "white",
    // options: white, dark, miami,
    //   smooth_gradient:ff0000:00ff00,
    //   rough_gradient:ff0000:00ff00,
    //   static_color:ff6600

    "status_bar": {
      "position": "bottom",
      // options: bottom, top

      "content": ["filename", "binds"]
      // tokens: filename (includes * when dirty), dirty, git, time, binds
    }
  },

  "syntax": {
    // all optional — set to "no" or null to disable coloring for that type
    // colors: named (red, dark_green, blue...) or hex (#ff6600)

    "comment": "dark_green",
    "string_double": "yellow",
    "string_single": "green",
    "number": "magenta",
    "keyword": "blue",
    "type_name": "cyan",
    "builtin": "dark_yellow",
    "decorator": "cyan",
    "variable": "dark_yellow",
    "lifetime": "cyan",
    "markdown_heading": "blue",
    "markdown_bold": "dark_yellow",
    "markdown_code": "green",
    "markdown_link": "cyan"
  },

  "keybindings": {
    // format: "modifier+key": "action"
    // modifier: ctrl, alt
    // key: a-z, 0-9
    // actions: save, find, undo, lines, exit

    "ctrl+s": "save",
    "ctrl+f": "find",
    "ctrl+z": "undo",
    "ctrl+l": "lines",
    "ctrl+x": "exit"
  }
}
"#;
        let _ = fs::write(&path, content);
    }
}

pub fn load_config() -> VanConfig {
    let path_exists = config_path().map_or(false, |p| p.exists());
    if !path_exists {
        write_default_config();
        return VanConfig::default();
    }

    let json = match load_raw_json() {
        Some(j) => j,
        None => return VanConfig::default(),
    };

    let mut config = VanConfig::default();

    if let Some(theme) = json.get("theme") {
        if let Some(style) = theme.get("style").and_then(|s| s.as_str()) {
            config.style = UiStyle::parse(style);
        }
        if let Some(sb) = theme.get("status_bar") {
            if let Some(pos) = sb.get("position").and_then(|s| s.as_str()) {
                config.status_bar_position = StatusBarPosition::parse(pos);
            }
            if let Some(content) = sb.get("content").and_then(|c| c.as_array()) {
                let items: Vec<String> = content.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                if !items.is_empty() {
                    config.status_bar_content = items;
                }
            }
        }
    }

    if let Some(syntax) = json.get("syntax").and_then(|s| s.as_object()) {
        config.syntax_colors = parse_syntax_colors(syntax);
    }

    if let Some(kb) = json.get("keybindings").and_then(|s| s.as_object()) {
        config.keybindings = parse_keybindings(kb);
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_parsing() {
        let raw = r#"{
  // config.ve
  "theme": {
    "style": "miami",
    "status_bar": {
      "position": "top",
      "content": ["filename", "dirty", "binds"]
    },
    // comment
  }
}"#;
        let cleaned = remove_trailing_commas(&strip_json_comments(raw));
        println!("CLEANED:\n{}", cleaned);
        let v: Result<serde_json::Value, _> = serde_json::from_str(&cleaned);
        assert!(v.is_ok(), "Parse failed: {:?}", v.err());
        let json = v.unwrap();
        assert_eq!(json["theme"]["style"], "miami");
        assert_eq!(json["theme"]["status_bar"]["position"], "top");
        assert_eq!(json["theme"]["status_bar"]["content"][0], "filename");
    }
}

    #[test]
    fn test_real_config_content() {
        let raw = r#"{
  // config.ve

  "theme": {
    "style": "dark",
      // applies to status bar bg, file picker boxes, selection highlights
      // options:
      //   "white"                          — plain white
      //   "miami"                  — smooth orange-pink gradient
      //   "rough_gradient:ff0000:00ff00"  — goes from color 1 to color 2 normal
      //   "smooth_gradient:ff0000:00ff00" — goes from color 1 to 2 smoothly/fading
      //   "static_color:ff6600"          — solid hex color
      //   "dark"                         — plain dark

    "status_bar": {
      "position": "bottom",
      "content": ["filename", "dirty", "git", "time", "binds"]
    },

    // OTHER is added as soon as i tell ya
  }
}
"#;
        let cleaned = remove_trailing_commas(&strip_json_comments(raw));
        println!("CLEANED:\n{}", cleaned);
        let v: Result<serde_json::Value, _> = serde_json::from_str(&cleaned);
        assert!(v.is_ok(), "Parse failed: {:?}", v.err());
        let json = v.unwrap();
        assert_eq!(json["theme"]["style"], "dark");
        assert_eq!(json["theme"]["status_bar"]["position"], "bottom");
        assert_eq!(json["theme"]["status_bar"]["content"][4], "binds");
    }

    #[test]
    fn test_debug_parsing() {
        let raw = "{\n  // config.ve\n\n  \"theme\": {\n    \"style\": \"dark\",\n    // comment\n  }\n}\n";
        println!("INPUT: {:?}", raw);
        let step1 = strip_json_comments(raw);
        println!("AFTER COMMENTS: {:?}", step1);
        let step2 = remove_trailing_commas(&step1);
        println!("AFTER COMMAS: {:?}", step2);
        println!("OPEN: {} CLOSE: {}",
            step2.matches('{').count(),
            step2.matches('}').count());
    }

    #[test]
    fn test_debug_real() {
        let raw = "{\n  // config.ve\n\n  \"theme\": {\n    \"style\": \"dark\",\n      // comment1\n      // comment2\n\n    \"status_bar\": {\n      \"position\": \"bottom\",\n      \"content\": [\"filename\", \"dirty\", \"git\", \"time\", \"binds\"]\n    },\n\n    // OTHER\n  }\n}\n";
        println!("INPUT braces: open={} close={}", raw.matches('{').count(), raw.matches('}').count());
        let step1 = strip_json_comments(raw);
        println!("AFTER COMMENTS braces: open={} close={}", step1.matches('{').count(), step1.matches('}').count());
        let step2 = remove_trailing_commas(&step1);
        println!("AFTER COMMAS braces: open={} close={}", step2.matches('{').count(), step2.matches('}').count());
        println!("OUTPUT:\n{}", step2);
        let v: Result<serde_json::Value, _> = serde_json::from_str(&step2);
        match v {
            Ok(_) => println!("PARSE OK"),
            Err(e) => println!("PARSE ERR: {}", e),
        }
    }

    #[test]
    fn test_write_default_config() {
        let path = config_path().unwrap();
        // Remove if exists
        let _ = fs::remove_file(&path);
        // This should trigger write
        let config = load_config();
        assert!(path.exists(), "Config file should have been created");
        println!("Created: {:?}", path);
        println!("Style: {:?}", config.style);
        // Clean up
        let _ = fs::remove_file(&path);
    }

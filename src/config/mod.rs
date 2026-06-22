//! 灵犀配置系统

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct LingxiConfig {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub animations: AnimationConfig,
    #[serde(default)]
    pub layout: LayoutConfig,
    #[serde(default)]
    pub decoration: DecorationConfig,
    #[serde(default)]
    pub binds: Vec<KeyBind>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct KeyBind {
    pub keys: String,       // e.g. "Super+Shift+1"
    pub action: String,     // "exec", "close", "quit", "workspace", "movetoworkspace", "focus", "swap"
    pub arg: Option<String>, // command to exec, workspace number, etc.
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub border_size: u32,
    pub gaps_inner: u32,
    pub gaps_outer: u32,
    pub cursor_size: u32,
    pub active_border_color: [f32; 4],
    pub inactive_border_color: [f32; 4],
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            border_size: 2,
            gaps_inner: 5,
            gaps_outer: 10,
            cursor_size: 24,
            active_border_color: [0.0, 0.9, 0.8, 1.0],    // 青色
            inactive_border_color: [0.3, 0.3, 0.3, 1.0],   // 灰色
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct AnimationConfig {
    pub enabled: bool,
    pub window_open_ms: u64,
    pub window_close_ms: u64,
    pub workspace_switch_ms: u64,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window_open_ms: 200,
            window_close_ms: 150,
            workspace_switch_ms: 300,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LayoutConfig {
    pub default_layout: String,
    pub master_ratio: f64,
    pub split_ratio: f64,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            default_layout: "dwindle".into(),
            master_ratio: 0.55,
            split_ratio: 0.5,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DecorationConfig {
    pub rounding: u32,
    pub blur_enabled: bool,
    pub blur_size: u32,
    pub blur_passes: u32,
    pub shadow_enabled: bool,
    pub shadow_range: u32,
    pub shadow_color: [f32; 4],
    pub shadow_offset_x: f32,
    pub shadow_offset_y: f32,
}

impl Default for DecorationConfig {
    fn default() -> Self {
        Self {
            rounding: 10,
            blur_enabled: true,
            blur_size: 8,
            blur_passes: 2,
            shadow_enabled: true,
            shadow_range: 15,
            shadow_color: [0.0, 0.0, 0.0, 0.5],
            shadow_offset_x: 0.0,
            shadow_offset_y: 4.0,
        }
    }
}

impl Default for LingxiConfig {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            animations: AnimationConfig::default(),
            layout: LayoutConfig::default(),
            decoration: DecorationConfig::default(),
            binds: default_binds(),
        }
    }
}

/// Default keybinds when no config file exists
fn default_binds() -> Vec<KeyBind> {
    vec![
        KeyBind { keys: "Super+Return".into(), action: "exec".into(), arg: Some("alacritty".into()) },
        KeyBind { keys: "Super+Q".into(), action: "close".into(), arg: None },
        KeyBind { keys: "Super+Shift+Q".into(), action: "quit".into(), arg: None },
        KeyBind { keys: "Super+D".into(), action: "exec".into(), arg: Some("fuzzel".into()) },
        KeyBind { keys: "Super+J".into(), action: "focus".into(), arg: Some("next".into()) },
        KeyBind { keys: "Super+K".into(), action: "focus".into(), arg: Some("prev".into()) },
        KeyBind { keys: "Super+H".into(), action: "focus".into(), arg: Some("prev".into()) },
        KeyBind { keys: "Super+Shift+J".into(), action: "swap".into(), arg: Some("next".into()) },
        KeyBind { keys: "Super+Shift+K".into(), action: "swap".into(), arg: Some("prev".into()) },
        KeyBind { keys: "Super+Shift+H".into(), action: "swap".into(), arg: Some("prev".into()) },
        KeyBind { keys: "Super+Shift+L".into(), action: "swap".into(), arg: Some("next".into()) },
        KeyBind { keys: "Super+F".into(), action: "fullscreen".into(), arg: None },
        KeyBind { keys: "Super+V".into(), action: "floating".into(), arg: None },
        KeyBind { keys: "Super+Ctrl+L".into(), action: "resize".into(), arg: Some("0.05".into()) },
        KeyBind { keys: "Super+Ctrl+H".into(), action: "resize".into(), arg: Some("-0.05".into()) },
        KeyBind { keys: "Super+1".into(), action: "workspace".into(), arg: Some("1".into()) },
        KeyBind { keys: "Super+2".into(), action: "workspace".into(), arg: Some("2".into()) },
        KeyBind { keys: "Super+3".into(), action: "workspace".into(), arg: Some("3".into()) },
        KeyBind { keys: "Super+4".into(), action: "workspace".into(), arg: Some("4".into()) },
        KeyBind { keys: "Super+5".into(), action: "workspace".into(), arg: Some("5".into()) },
        KeyBind { keys: "Super+Shift+1".into(), action: "movetoworkspace".into(), arg: Some("1".into()) },
        KeyBind { keys: "Super+Shift+2".into(), action: "movetoworkspace".into(), arg: Some("2".into()) },
        KeyBind { keys: "Super+Shift+3".into(), action: "movetoworkspace".into(), arg: Some("3".into()) },
        KeyBind { keys: "Super+Shift+4".into(), action: "movetoworkspace".into(), arg: Some("4".into()) },
        KeyBind { keys: "Super+Shift+5".into(), action: "movetoworkspace".into(), arg: Some("5".into()) },
        KeyBind { keys: "Alt+Tab".into(), action: "focus".into(), arg: Some("next".into()) },
        KeyBind { keys: "Alt+Shift+Tab".into(), action: "focus".into(), arg: Some("prev".into()) },
        KeyBind { keys: "Super+Escape".into(), action: "lock".into(), arg: None },
    ]
}

/// Parse a keybind string like "Super+Shift+1" into (logo, shift, ctrl, alt, keysym)
#[derive(Debug, Clone)]
pub struct ParsedKeyBind {
    pub logo: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub keysym: u32,
}

impl ParsedKeyBind {
    pub fn parse(keys: &str) -> Option<Self> {
        let parts: Vec<&str> = keys.split('+').collect();
        let mut logo = false;
        let mut shift = false;
        let mut ctrl = false;
        let mut alt = false;
        let mut key_part = "";

        for part in &parts {
            match part.to_lowercase().as_str() {
                "super" | "mod4" | "logo" => logo = true,
                "shift" => shift = true,
                "ctrl" | "control" => ctrl = true,
                "alt" | "mod1" => alt = true,
                _ => key_part = part,
            }
        }

        let keysym = key_name_to_keysym(key_part)?;
        Some(Self { logo, shift, ctrl, alt, keysym })
    }
}

/// Convert a key name to xkb keysym
fn key_name_to_keysym(name: &str) -> Option<u32> {
    match name.to_lowercase().as_str() {
        "return" | "enter" => Some(0xff0d),
        "escape" | "esc" => Some(0xff1b),
        "tab" => Some(0xff09),
        "backspace" => Some(0xff08),
        "space" => Some(0x0020),
        "left" => Some(0xff51),
        "up" => Some(0xff52),
        "right" => Some(0xff53),
        "down" => Some(0xff54),
        "delete" => Some(0xffff),
        "home" => Some(0xff50),
        "end" => Some(0xff57),
        "f1" => Some(0xffbe),
        "f2" => Some(0xffbf),
        "f3" => Some(0xffc0),
        "f4" => Some(0xffc1),
        "f5" => Some(0xffc2),
        "f6" => Some(0xffc3),
        "f7" => Some(0xffc4),
        "f8" => Some(0xffc5),
        "f9" => Some(0xffc6),
        "f10" => Some(0xffc7),
        "f11" => Some(0xffc8),
        "f12" => Some(0xffc9),
        // Numbers
        "1" => Some(0x0031),
        "2" => Some(0x0032),
        "3" => Some(0x0033),
        "4" => Some(0x0034),
        "5" => Some(0x0035),
        "6" => Some(0x0036),
        "7" => Some(0x0037),
        "8" => Some(0x0038),
        "9" => Some(0x0039),
        "0" => Some(0x0030),
        // Letters (lowercase keysyms)
        s if s.len() == 1 && s.chars().next().unwrap().is_ascii_alphabetic() => {
            Some(s.chars().next().unwrap().to_ascii_lowercase() as u32)
        }
        _ => None,
    }
}

impl LingxiConfig {
    pub fn load() -> Self {
        let config_path = dirs_config_path();
        match std::fs::read_to_string(&config_path) {
            Ok(content) => {
                let mut config: Self = toml::from_str(&content).unwrap_or_default();
                // If no binds in config file, use defaults
                if config.binds.is_empty() {
                    config.binds = default_binds();
                }
                config
            }
            Err(_) => Self::default(),
        }
    }

    /// Get parsed keybinds
    pub fn parsed_binds(&self) -> Vec<(ParsedKeyBind, String, Option<String>)> {
        self.binds
            .iter()
            .filter_map(|bind| {
                ParsedKeyBind::parse(&bind.keys)
                    .map(|parsed| (parsed, bind.action.clone(), bind.arg.clone()))
            })
            .collect()
    }
}

fn dirs_config_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    format!("{home}/.config/lingxi/lingxi.toml")
}

use serde::Deserialize;
use std::path::PathBuf;

/// RGBA color represented as four f64 values in [0.0, 1.0].
#[derive(Clone, Copy, Deserialize)]
#[serde(untagged)]
pub enum Color {
    /// Hex string: `"#rrggbb"` (alpha=1.0) or `"#rrggbbaa"`
    Hex(HexColor),
    /// Inline array: `[r, g, b, a]`
    Array([f64; 4]),
}

impl Color {
    pub fn rgba(self) -> (f64, f64, f64, f64) {
        match self {
            Color::Hex(h) => h.0,
            Color::Array([r, g, b, a]) => (r, g, b, a),
        }
    }
}


/// Newtype so serde can deserialize a hex string into an RGBA tuple.
#[derive(Clone, Copy)]
pub struct HexColor((f64, f64, f64, f64));

impl<'de> Deserialize<'de> for HexColor {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        parse_hex(&s)
            .map(HexColor)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid hex color: {s}")))
    }
}

fn parse_hex(s: &str) -> Option<(f64, f64, f64, f64)> {
    let s = s.trim_start_matches('#');
    if s.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()? as f64 / 255.0;
    let g = u8::from_str_radix(&s[2..4], 16).ok()? as f64 / 255.0;
    let b = u8::from_str_radix(&s[4..6], 16).ok()? as f64 / 255.0;
    let a = if s.len() >= 8 {
        u8::from_str_radix(&s[6..8], 16).ok()? as f64 / 255.0
    } else {
        1.0
    };
    Some((r, g, b, a))
}

// ── sub-structs ───────────────────────────────────────────────────────────────

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct AppearanceConfig {
    /// Font for window class/title labels inside cards.
    pub font: String,
    /// Font for workspace number/name labels.
    pub label_font: String,
    /// Gap between workspace cards (px).
    pub card_padding: f64,
    /// Corner radius of cards (px).
    pub card_radius: f64,
    /// Maximum card width (px).
    pub max_card_width: f64,
    /// Maximum card height (px).
    pub max_card_height: f64,
    /// Height reserved for the workspace label at the top of each card (px).
    pub label_height: f64,
    /// Padding between card edge and window area (px).
    pub thumb_padding: f64,
    /// Width of the selection highlight border (px).
    pub select_border: f64,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            font: "Sans 11".into(),
            label_font: "Sans Bold 13".into(),
            card_padding: 24.0,
            card_radius: 12.0,
            max_card_width: 480.0,
            max_card_height: 320.0,
            label_height: 32.0,
            thumb_padding: 8.0,
            select_border: 3.0,
        }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct ColorsConfig {
    /// Full-screen dim overlay. Only the alpha matters for the default black bg.
    pub background: Color,
    /// Card background fill.
    pub card: Color,
    /// Selection highlight border.
    pub selection: Color,
    /// Workspace number/name label text.
    pub label: Color,
    /// "(empty)" text in empty workspace cards.
    pub empty_label: Color,
    /// Window class/title text drawn over thumbnails.
    pub window_label: Color,
    /// Border drawn around the active window thumbnail (the one 'm' will move).
    pub active_window: Color,
}

impl Default for ColorsConfig {
    fn default() -> Self {
        Self {
            background:    Color::Array([0.0,  0.0,  0.0,  0.75]),
            card:          Color::Array([0.12, 0.12, 0.15, 0.95]),
            selection:     Color::Array([0.4,  0.6,  1.0,  0.9 ]),
            label:         Color::Array([0.85, 0.85, 0.9,  1.0 ]),
            empty_label:   Color::Array([0.5,  0.5,  0.55, 0.8 ]),
            window_label:  Color::Array([1.0,  1.0,  1.0,  0.9 ]),
            active_window: Color::Array([1.0,  0.75, 0.2,  0.95]),
        }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct BehaviorConfig {
    /// Skip window thumbnail capture (faster, fallback colored rects only).
    pub no_preview: bool,
    /// After moving a window with 'm', also switch to the destination workspace.
    pub switch_on_move: bool,
    /// Enable mouse navigation (hover to select, left-click to switch,
    /// right-click to move the active window). Overridden by the
    /// `--allow-mouse` CLI flag when set.
    pub allow_mouse: bool,
    /// Show a "+ Add Desktop" button as an extra grid slot, and enable the
    /// `n` keybind. Clicking/pressing it switches to the first unused
    /// workspace id >= `add_desktop_start_id`, which the compositor creates
    /// empty as a side effect.
    pub show_add_desktop_button: bool,
    /// First workspace id considered for a new "Add Desktop" workspace. Keep
    /// this above your highest pinned SUPER+<n> workspace number.
    pub add_desktop_start_id: i32,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            no_preview: false,
            switch_on_move: true,
            allow_mouse: false,
            show_add_desktop_button: true,
            add_desktop_start_id: 11,
        }
    }
}

// ── top-level Config ──────────────────────────────────────────────────────────

#[derive(Deserialize, Clone, Default)]
#[serde(default)]
pub struct Config {
    pub appearance: AppearanceConfig,
    pub colors: ColorsConfig,
    pub behavior: BehaviorConfig,
}

/// Bundled at build time so a first run can seed the user's config dir.
const EXAMPLE_CONFIG: &str = include_str!("../config.example.toml");

impl Config {
    /// Load from `$XDG_CONFIG_HOME/hyprexpose/config.toml` (falls back to
    /// `~/.config/hyprexpose/config.toml`). On first run the file is created
    /// from the bundled `config.example.toml` so users have a template to
    /// edit. Errors creating the file are ignored — defaults are used.
    pub fn load() -> Self {
        let path = config_path();

        if !path.exists() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if std::fs::write(&path, EXAMPLE_CONFIG).is_ok() {
                eprintln!("hyprexpose: created default config at {}", path.display());
            }
        }

        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str::<Config>(&text) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("hyprexpose: config parse error in {}: {e}", path.display());
                Self::default()
            }
        }
    }
}

fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
            PathBuf::from(home).join(".config")
        });
    base.join("hyprexpose").join("config.toml")
}

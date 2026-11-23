// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Configuración de la terminal: `~/.config/forge/forge.toml`.
//!
//! ```toml
//! [font]
//! family = "Menlo"
//! size = 14
//! line_height = 1.3
//!
//! [window]
//! padding = 10
//!
//! [terminal]
//! scrollback = 10000
//! option_as_alt = false      # true: ⌥ envía Meta (ESC+tecla) en vez de @, #, ñ...
//! shell_integration = true   # bloques de comandos (zsh)
//! notify_after_secs = 10     # aviso al terminar comandos largos (0 = nunca)
//!
//! [theme]
//! name = "forja"             # forja | one-dark | dracula | solarized-dark | gruvbox | light
//! background = "#171412"     # cualquier color suelto sobrescribe el tema
//!
//! [verify]                 # al terminar un agente en su worktree
//! auto = true                # ejecutar los tests solos
//! timeout_secs = 600
//! [verify.commands]          # por proyecto; si no, se detecta (cargo, npm, go...)
//! forge = "cargo test --workspace"
//!
//! [keys]
//! new_tab = "cmd+t"
//! split_right = "cmd+d"
//! ```
//!
//! Se vigila el archivo: los cambios se aplican al guardar, sin reiniciar.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use forge_term::{Rgb8, Theme};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
    pub line_height: f32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self { family: "Menlo".into(), size: 14.0, line_height: 1.3 }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct WindowConfig {
    pub padding: f32,
    /// Qué se ve al abrir forge: `"mesa"` (los agentes) o `"terminal"`.
    pub start: String,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self { padding: 10.0, start: "mesa".into() }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct TerminalConfig {
    pub scrollback: usize,
    pub option_as_alt: bool,
    /// Integración automática con zsh (bloques de comandos).
    pub shell_integration: bool,
    /// Avisar cuando termine un comando que tardó al menos estos segundos y
    /// forge no está a la vista (0 = nunca).
    pub notify_after_secs: u64,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self { scrollback: 10_000, option_as_alt: false, shell_integration: true, notify_after_secs: 10 }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct ThemeConfig {
    pub name: Option<String>,
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub cursor: Option<String>,
    pub selection: Option<String>,
    /// 16 colores ANSI en hex.
    pub ansi: Option<Vec<String>>,
}

/// Verificación automática de lo que hacen los agentes.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct VerifyConfig {
    /// Ejecutarla sola cuando un agente termina en su worktree.
    pub auto: bool,
    pub timeout_secs: u64,
    /// Nombre del proyecto → comando (`""` o `"off"`: no verificar).
    pub commands: BTreeMap<String, String>,
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self { auto: true, timeout_secs: 600, commands: BTreeMap::new() }
    }
}

impl VerifyConfig {
    /// `Some(None)`: detectar; `Some(Some(c))`: el configurado; `None`: desactivada.
    pub fn command(&self, project: &str) -> Option<Option<String>> {
        match self.commands.get(project).map(|c| c.trim()) {
            None => Some(None),
            Some("" | "off" | "false") => None,
            Some(c) => Some(Some(c.to_string())),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub font: FontConfig,
    pub window: WindowConfig,
    pub terminal: TerminalConfig,
    pub theme: ThemeConfig,
    pub verify: VerifyConfig,
    /// Acción → atajo (`"cmd+shift+d"`).
    pub keys: BTreeMap<String, String>,
}

/// Ruta de la config: `FORGE_CONFIG` si está definida, o `~/.config/forge/forge.toml`.
pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FORGE_CONFIG") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".config").join("forge").join("forge.toml"))
}

/// `#rrggbb` o `rrggbb`.
pub fn parse_hex(s: &str) -> Option<Rgb8> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn hex_list(list: [&str; 16]) -> [Rgb8; 16] {
    list.map(|h| parse_hex(h).unwrap_or([0, 0, 0]))
}

/// Temas incluidos.
pub fn builtin_theme(name: &str) -> Option<Theme> {
    let base = Theme::default();
    let t = match name.to_ascii_lowercase().as_str() {
        "forja" | "forge" | "default" => base,
        "one-dark" | "onedark" => Theme::one_dark(),
        "dracula" => Theme {
            ansi: hex_list([
                "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2",
                "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff",
            ]),
            foreground: [0xf8, 0xf8, 0xf2],
            background: [0x28, 0x2a, 0x36],
            cursor: [0xff, 0x79, 0xc6],
            selection: [0x44, 0x47, 0x5a],
            ..base
        },
        "solarized-dark" | "solarized" => Theme {
            ansi: hex_list([
                "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#eee8d5",
                "#586e75", "#cb4b16", "#93a1a1", "#839496", "#657b83", "#6c71c4", "#93a1a1", "#fdf6e3",
            ]),
            foreground: [0x83, 0x94, 0x96],
            background: [0x00, 0x2b, 0x36],
            cursor: [0x93, 0xa1, 0xa1],
            selection: [0x07, 0x36, 0x42],
            ..base
        },
        "gruvbox" | "gruvbox-dark" => Theme {
            ansi: hex_list([
                "#282828", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a", "#a89984",
                "#928374", "#fb4934", "#b8bb26", "#fabd2f", "#83a598", "#d3869b", "#8ec07c", "#ebdbb2",
            ]),
            foreground: [0xeb, 0xdb, 0xb2],
            background: [0x28, 0x28, 0x28],
            cursor: [0xfe, 0x80, 0x19],
            selection: [0x50, 0x49, 0x45],
            ..base
        },
        "light" => Theme {
            ansi: hex_list([
                "#383a42", "#e45649", "#50a14f", "#c18401", "#4078f2", "#a626a4", "#0184bc", "#a0a1a7",
                "#696c77", "#e45649", "#50a14f", "#c18401", "#4078f2", "#a626a4", "#0184bc", "#ffffff",
            ]),
            foreground: [0x38, 0x3a, 0x42],
            background: [0xfa, 0xfa, 0xfa],
            cursor: [0x52, 0x6f, 0xff],
            selection: [0xd8, 0xdc, 0xe6],
            search_match: [0xf5, 0xe0, 0x9b],
            search_current: [0xff, 0xb8, 0x3d],
        },
        _ => return None,
    };
    Some(t)
}

impl Config {
    pub fn load() -> Self {
        let Some(p) = path() else { return Self::default() };
        match std::fs::read_to_string(&p) {
            Ok(s) => Self::parse(&s).unwrap_or_else(|e| {
                log::warn!("forge.toml inválido, se usan valores por defecto: {e}");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Tema resultante: el elegido más los colores sueltos.
    pub fn theme(&self) -> Theme {
        let mut t = self.theme.name.as_deref().and_then(builtin_theme).unwrap_or_default();
        let c = &self.theme;
        let set = |slot: &mut Rgb8, v: &Option<String>| {
            if let Some(rgb) = v.as_deref().and_then(parse_hex) {
                *slot = rgb;
            }
        };
        set(&mut t.foreground, &c.foreground);
        set(&mut t.background, &c.background);
        set(&mut t.cursor, &c.cursor);
        set(&mut t.selection, &c.selection);
        if let Some(list) = &c.ansi {
            for (slot, h) in t.ansi.iter_mut().zip(list) {
                if let Some(rgb) = parse_hex(h) {
                    *slot = rgb;
                }
            }
        }
        t
    }

    pub fn font_size(&self) -> f32 {
        self.font.size.clamp(6.0, 72.0)
    }

    pub fn line_height(&self) -> f32 {
        self.font.line_height.clamp(0.8, 3.0)
    }
}

/// Vigila `forge.toml` y llama a `on_change` con la config nueva al guardarlo.
pub fn watch(on_change: impl Fn(Config) + Send + 'static) {
    std::thread::Builder::new()
        .name("config-watch".into())
        .spawn(move || {
            let Some(p) = path() else { return };
            let stamp = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            let mut last: Option<SystemTime> = stamp(&p);
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let now = stamp(&p);
                if now != last {
                    last = now;
                    on_change(Config::load());
                }
            }
        })
        .expect("hilo de config");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing() {
        assert_eq!(parse_hex("#1a1d23"), Some([0x1a, 0x1d, 0x23]));
        assert_eq!(parse_hex("ffffff"), Some([255, 255, 255]));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("zzzzzz"), None);
    }

    #[test]
    fn full_example() {
        let c = Config::parse(
            r##"
            [font]
            family = "JetBrains Mono"
            size = 15
            [terminal]
            option_as_alt = true
            [theme]
            name = "dracula"
            background = "#000000"
            [keys]
            split_right = "cmd+e"
            [verify.commands]
            forge = "cargo test --workspace"
            web = "off"
            "##,
        )
        .unwrap();
        assert_eq!(c.font.family, "JetBrains Mono");
        assert_eq!(c.font_size(), 15.0);
        assert_eq!(c.font.line_height, 1.3); // por defecto
        assert!(c.terminal.option_as_alt);
        let t = c.theme();
        assert_eq!(t.background, [0, 0, 0]);
        assert_eq!(t.foreground, [0xf8, 0xf8, 0xf2]); // de dracula
        assert_eq!(c.keys["split_right"], "cmd+e");
        assert!(c.verify.auto && c.verify.timeout_secs == 600);
        assert_eq!(c.verify.command("forge"), Some(Some("cargo test --workspace".into())));
        assert_eq!(c.verify.command("web"), None);
        assert_eq!(c.verify.command("otro"), Some(None));
    }

    #[test]
    fn empty_and_unknown_theme() {
        let c = Config::parse("").unwrap();
        assert_eq!(c, Config::default());
        let c = Config::parse("[theme]\nname = \"no-existe\"").unwrap();
        assert_eq!(c.theme().background, Theme::default().background);
        for n in ["forja", "one-dark", "dracula", "solarized-dark", "gruvbox", "light"] {
            assert!(builtin_theme(n).is_some(), "{n}");
        }
    }
}

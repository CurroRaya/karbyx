// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Configuración del consumo de IA: `~/.config/forge/usage.toml`.
//!
//! ```toml
//! # Qué asistentes leer: "auto" detecta los instalados en esta máquina.
//! providers = ["auto"]            # o ["claude", "codex", "gemini"]
//!
//! # Límites propios por proveedor (Anthropic no publica los de sus planes).
//! # En tokens "frescos": input + output + escritura de caché.
//! [limits.claude]
//! block_tokens = 20_000_000       # ventana de 5 h
//! weekly_tokens = 300_000_000     # semana
//! weekly_reset = "monday"         # día en que reinicia tu semana
//!
//! [[pricing]]                     # precios propios (USD por millón de tokens)
//! prefix = "claude-opus-5-5"
//! input = 4.0
//! output = 20.0
//! ```
//!
//! Si no existe `usage.toml` se lee el antiguo `claude.toml` (con `[limits]`
//! a secas), que se aplica a Claude.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::Weekday;
use serde::{Deserialize, Serialize};

use crate::pricing::{ModelPrice, PriceTable};
use crate::provider::Provider;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub block_tokens: Option<u64>,
    pub weekly_tokens: Option<u64>,
    pub weekly_reset: Option<String>,
}

impl Limits {
    pub fn weekly_reset_day(&self) -> Weekday {
        self.weekly_reset.as_deref().and_then(|s| s.parse().ok()).unwrap_or(Weekday::Mon)
    }

    pub fn is_set(&self) -> bool {
        self.block_tokens.is_some() || self.weekly_tokens.is_some()
    }
}

fn auto() -> Vec<String> {
    vec!["auto".into()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageConfig {
    #[serde(default = "auto")]
    pub providers: Vec<String>,
    /// Límites por id de proveedor (`claude`, `codex`, `gemini`).
    pub limits: BTreeMap<String, Limits>,
    pub pricing: Vec<ModelPrice>,
}

impl Default for UsageConfig {
    fn default() -> Self {
        Self { providers: auto(), limits: BTreeMap::new(), pricing: Vec::new() }
    }
}

/// Formato antiguo (`claude.toml`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Legacy {
    limits: Limits,
    pricing: Vec<ModelPrice>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no se pudo leer {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("TOML inválido en {0}: {1}")]
    Toml(PathBuf, toml::de::Error),
}

fn config_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("forge"))
}

fn read(path: &PathBuf) -> Result<Option<String>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError::Io(path.clone(), e)),
    }
}

impl UsageConfig {
    pub fn default_path() -> Option<PathBuf> {
        config_dir().map(|d| d.join("usage.toml"))
    }

    /// Carga la config (o la antigua `claude.toml`). Sin archivo: valores por defecto.
    pub fn load(path: Option<PathBuf>) -> Result<Self, ConfigError> {
        if let Some(p) = path {
            return match read(&p)? {
                Some(s) => Self::parse(&s).map_err(|e| ConfigError::Toml(p, e)),
                None => Ok(Self::default()),
            };
        }
        let Some(dir) = config_dir() else { return Ok(Self::default()) };
        let main = dir.join("usage.toml");
        if let Some(s) = read(&main)? {
            return Self::parse(&s).map_err(|e| ConfigError::Toml(main, e));
        }
        let legacy = dir.join("claude.toml");
        if let Some(s) = read(&legacy)? {
            let l: Legacy = toml::from_str(&s).map_err(|e| ConfigError::Toml(legacy, e))?;
            let mut c = Self { pricing: l.pricing, ..Self::default() };
            if l.limits.is_set() {
                c.limits.insert("claude".into(), l.limits);
            }
            return Ok(c);
        }
        Ok(Self::default())
    }

    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Proveedores a mostrar: los elegidos, o los detectados si es "auto".
    pub fn selected(&self) -> Vec<Provider> {
        self.selected_with(&Provider::detect_all())
    }

    /// Igual que [`selected`](Self::selected) con una lista de detectados dada.
    pub fn selected_with(&self, detected: &[Provider]) -> Vec<Provider> {
        let mut out: Vec<Provider> = Vec::new();
        for id in &self.providers {
            if id.eq_ignore_ascii_case("auto") {
                out.extend(detected.iter().copied());
            } else if let Some(p) = Provider::from_id(id) {
                out.push(p);
            }
        }
        if self.providers.is_empty() {
            out.extend(detected.iter().copied());
        }
        let mut seen = Vec::new();
        out.retain(|p| {
            let fresh = !seen.contains(p);
            seen.push(*p);
            fresh
        });
        out
    }

    pub fn limits_for(&self, p: Provider) -> Limits {
        self.limits.get(p.id()).cloned().unwrap_or_default()
    }

    pub fn price_table(&self) -> PriceTable {
        let mut t = PriceTable::default();
        t.merge(self.pricing.clone());
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example() {
        let c = UsageConfig::parse(
            r#"
            providers = ["codex", "claude"]
            [limits.claude]
            block_tokens = 1_000
            weekly_reset = "thursday"
            [[pricing]]
            prefix = "claude-opus-5-5"
            input = 1.0
            output = 2.0
            "#,
        )
        .unwrap();
        let l = c.limits_for(Provider::Claude);
        assert_eq!(l.block_tokens, Some(1000));
        assert_eq!(l.weekly_reset_day(), Weekday::Thu);
        assert!(!c.limits_for(Provider::Codex).is_set());
        assert_eq!(c.price_table().lookup("claude-opus-5-5").unwrap().input, 1.0);
        assert_eq!(c.selected_with(&[]), vec![Provider::Codex, Provider::Claude]);
    }

    #[test]
    fn auto_selection() {
        let c = UsageConfig::default();
        assert_eq!(
            c.selected_with(&[Provider::Claude, Provider::Gemini]),
            vec![Provider::Claude, Provider::Gemini]
        );
        let c = UsageConfig::parse(r#"providers = ["claude", "auto"]"#).unwrap();
        assert_eq!(
            c.selected_with(&[Provider::Codex, Provider::Claude]),
            vec![Provider::Claude, Provider::Codex]
        );
        let c = UsageConfig::parse("").unwrap();
        assert_eq!(c.providers, vec!["auto".to_string()]);
    }
}

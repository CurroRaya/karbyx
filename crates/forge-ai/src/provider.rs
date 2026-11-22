// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 CurroRaya y los colaboradores de forge

//! Asistentes de IA soportados y dónde guarda cada uno su historial.
//!
//! Cada proveedor traduce su formato de archivo a [`UsageRecord`]s comunes, de
//! modo que ventanas, proyecciones y gráficos funcionan igual para todos.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Claude Code (Anthropic): `~/.claude/projects/**/*.jsonl`.
    Claude,
    /// Codex CLI (OpenAI): `~/.codex/sessions/**/rollout-*.jsonl`.
    Codex,
    /// Gemini CLI (Google): `~/.gemini/tmp/*/chats/session-*.json`.
    Gemini,
}

/// Cómo se escribe el historial en disco.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileMode {
    /// Líneas JSON que solo se añaden al final: se lee de forma incremental.
    Append,
    /// Un JSON completo que se reescribe: se relee entero cuando cambia.
    Whole,
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

fn env_dirs(var: &str, sub: &str) -> Vec<PathBuf> {
    std::env::var(var)
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| PathBuf::from(p).join(sub))
                .collect()
        })
        .unwrap_or_default()
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::Claude, Provider::Codex, Provider::Gemini];

    pub fn id(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Gemini => "gemini",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Codex => "Codex",
            Provider::Gemini => "Gemini",
        }
    }

    /// Comando de terminal que arranca el asistente.
    pub fn command(self) -> &'static str {
        self.id()
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id().eq_ignore_ascii_case(s.trim()))
    }

    /// Directorios candidatos (existan o no).
    fn candidate_roots(self) -> Vec<PathBuf> {
        let mut v = Vec::new();
        match self {
            Provider::Claude => {
                v.extend(env_dirs("CLAUDE_CONFIG_DIR", "projects"));
                if let Some(h) = home() {
                    v.push(h.join(".claude/projects"));
                    v.push(h.join(".config/claude/projects"));
                }
            }
            Provider::Codex => {
                v.extend(env_dirs("CODEX_HOME", "sessions"));
                if let Some(h) = home() {
                    v.push(h.join(".codex/sessions"));
                }
            }
            Provider::Gemini => {
                v.extend(env_dirs("GEMINI_HOME", "tmp"));
                if let Some(h) = home() {
                    v.push(h.join(".gemini/tmp"));
                }
            }
        }
        v
    }

    /// Directorios con historial que existen en esta máquina.
    pub fn roots(self) -> Vec<PathBuf> {
        let mut v = self.candidate_roots();
        v.retain(|p| p.is_dir());
        v.dedup();
        v
    }

    /// ¿Está en uso en esta máquina? Hace falta historial y, además, que el
    /// comando esté instalado o que haya actividad en los últimos
    /// [`RECENT_DAYS`] días. Así una carpeta olvidada de una prueba antigua no
    /// cuenta como asistente en uso.
    pub fn detected(self) -> bool {
        let roots = self.roots();
        !roots.is_empty()
            && (self.command_available()
                || newest_activity(self, &roots).is_some_and(|t| is_recent(t, RECENT_DAYS)))
    }

    /// ¿Está el comando del asistente instalado?
    pub fn command_available(self) -> bool {
        search_dirs().iter().any(|d| d.join(self.command()).is_file())
    }

    /// Proveedores con historial en esta máquina.
    pub fn detect_all() -> Vec<Provider> {
        Self::ALL.into_iter().filter(|p| p.detected()).collect()
    }

    pub(crate) fn mode(self) -> FileMode {
        match self {
            Provider::Claude | Provider::Codex => FileMode::Append,
            Provider::Gemini => FileMode::Whole,
        }
    }

    /// ¿Es este archivo un historial de este proveedor?
    pub(crate) fn wants(self, path: &Path) -> bool {
        let ext = path.extension().and_then(|e| e.to_str());
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        match self {
            Provider::Claude | Provider::Codex => ext == Some("jsonl"),
            Provider::Gemini => {
                ext == Some("json")
                    && name.starts_with("session-")
                    && path.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "chats")
            }
        }
    }
}

/// Días sin actividad tras los que un historial se considera abandonado.
pub const RECENT_DAYS: u64 = 30;

/// Directorios donde buscar comandos: el PATH y los habituales que una app
/// lanzada desde el Finder no hereda (Homebrew, ~/.local/bin, nvm, bun...).
fn search_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    v.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    if let Some(h) = home() {
        v.extend([".local/bin", ".bun/bin", ".npm-global/bin", ".claude/local"].map(|d| h.join(d)));
        if let Ok(rd) = std::fs::read_dir(h.join(".nvm/versions/node")) {
            v.extend(rd.flatten().map(|e| e.path().join("bin")));
        }
    }
    v
}

/// Fecha del archivo de historial más reciente del proveedor.
fn newest_activity(p: Provider, roots: &[PathBuf]) -> Option<SystemTime> {
    roots
        .iter()
        .flat_map(|r| walkdir::WalkDir::new(r).follow_links(false).into_iter().flatten())
        .filter(|e| e.file_type().is_file() && p.wants(e.path()))
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
}

fn is_recent(t: SystemTime, days: u64) -> bool {
    SystemTime::now().duration_since(t).map(|d| d.as_secs() <= days * 86_400).unwrap_or(true)
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn ids_roundtrip() {
        for p in Provider::ALL {
            assert_eq!(Provider::from_id(p.id()), Some(p));
        }
        assert_eq!(Provider::from_id(" Codex "), Some(Provider::Codex));
        assert_eq!(Provider::from_id("gpt"), None);
    }

    #[test]
    fn recency() {
        let now = SystemTime::now();
        assert!(is_recent(now, RECENT_DAYS));
        assert!(is_recent(now - Duration::from_secs(29 * 86_400), RECENT_DAYS));
        assert!(!is_recent(now - Duration::from_secs(31 * 86_400), RECENT_DAYS));
        // Fechas en el futuro (relojes desajustados) cuentan como recientes.
        assert!(is_recent(now + Duration::from_secs(3600), RECENT_DAYS));
    }

    #[test]
    fn stale_history_is_found_by_newest_activity() {
        let dir = std::env::temp_dir().join(format!("forge-prov-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("p")).unwrap();
        std::fs::write(dir.join("p/a.jsonl"), "{}\n").unwrap();
        let t = newest_activity(Provider::Claude, std::slice::from_ref(&dir)).expect("mtime");
        assert!(is_recent(t, 1));
        assert!(newest_activity(Provider::Gemini, std::slice::from_ref(&dir)).is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn file_filters() {
        assert!(Provider::Claude.wants(Path::new("/x/proj/abc.jsonl")));
        assert!(Provider::Codex.wants(Path::new("/x/2026/11/22/rollout-1.jsonl")));
        assert!(Provider::Gemini.wants(Path::new("/x/tmp/hash/chats/session-2026.json")));
        assert!(!Provider::Gemini.wants(Path::new("/x/tmp/hash/logs.json")));
        assert!(!Provider::Gemini.wants(Path::new("/x/tmp/hash/session-2026.json")));
    }
}

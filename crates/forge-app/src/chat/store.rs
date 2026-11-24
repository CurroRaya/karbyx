// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Lo que el chat recuerda entre arranques: servidores, modelo elegido y
//! carpetas recientes. Solo lo puede leer el usuario (hay claves).

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use forge_agent::api::Server;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    pub servers: Vec<Server>,
    /// Modelo elegido: (dirección del servidor, id).
    pub model: Option<(String, String)>,
    pub project: Option<PathBuf>,
    pub recents: Vec<PathBuf>,
}

fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FORGE_CHAT_STATE") {
        return Some(p.into());
    }
    Some(dirs::config_dir()?.join("forge").join("chat.json"))
}

impl Saved {
    pub fn load() -> Self {
        let text = path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Err(e) = write_private(&p, &serde_json::to_string_pretty(self).unwrap_or_default()) {
            log::warn!("no se pudo guardar {}: {e}", p.display());
        }
    }

    pub fn used_project(&mut self, p: PathBuf) {
        self.recents.retain(|r| *r != p);
        self.recents.insert(0, p.clone());
        self.recents.truncate(8);
        self.project = Some(p);
    }
}

/// Escribe de una vez (archivo nuevo y cambio de nombre), solo para el usuario.
fn write_private(p: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(text.as_bytes())?;
    std::fs::rename(&tmp, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_atomic() {
        let p = std::env::temp_dir().join(format!("forge-store-{}.json", std::process::id()));
        write_private(&p, "{}").unwrap();
        write_private(&p, "{\"servers\":[]}").unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"servers\":[]}");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn recents() {
        let mut s = Saved::default();
        for p in ["/a", "/b", "/a"] {
            s.used_project(p.into());
        }
        assert_eq!(s.recents, [PathBuf::from("/a"), PathBuf::from("/b")]);
    }
}

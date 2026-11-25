// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Lo que el chat recuerda entre arranques: servidores, modelo elegido,
//! carpetas recientes y conversaciones. Solo lo puede leer el usuario (hay
//! claves y trozos de sus archivos).
//!
//! Cada conversación es un archivo de dos líneas: la ficha (para la lista, sin
//! leer el resto) y el contenido.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use forge_agent::api::{Msg, Server, Stats};
use forge_agent::files;
use forge_agent::tools::Detail;
use serde::{Deserialize, Serialize};

use super::extend::McpState;
use crate::worktree::Worktree;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    pub servers: Vec<Server>,
    /// Modelo elegido: (dirección del servidor, id).
    pub model: Option<(String, String)>,
    pub project: Option<PathBuf>,
    pub recents: Vec<PathBuf>,
    /// Quién hace las tareas de la mesa.
    pub worker: Option<String>,
    /// Lo decidido de cada servidor MCP.
    pub mcp: BTreeMap<String, McpState>,
}

fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FORGE_CHAT_STATE") {
        return Some(p.into());
    }
    Some(dirs::config_dir()?.join("forge").join("chat.json"))
}

impl Saved {
    /// Lo guardado y, si estaba dañado, dónde se ha apartado.
    pub fn load() -> (Self, Option<PathBuf>) {
        path().map(|p| files::load_json(&p)).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Err(e) = files::write_private(&p, &serde_json::to_string_pretty(self).unwrap_or_default()) {
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

/// Versión del formato de las conversaciones.
pub const VERSION: u32 = 1;

/// Ficha de una conversación.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub v: u32,
    pub title: String,
    pub root: Option<PathBuf>,
    /// Copia del proyecto hecha para ella.
    #[serde(default)]
    pub worktree: Option<Worktree>,
    /// Su trabajo ya se fusionó o se descartó.
    #[serde(default)]
    pub settled: bool,
    /// Último cambio, en segundos desde 1970.
    pub updated: i64,
}

/// Contenido de una conversación.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored {
    /// Se guardó mientras trabajaba.
    pub busy: bool,
    pub stats: Option<Stats>,
    /// Lo que ve el modelo.
    pub history: Vec<Msg>,
    /// Lo que se ve en pantalla.
    pub items: Vec<StoredItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StoredItem {
    User(String),
    Assistant(String),
    Think { text: String, ms: u64 },
    Tool { verb: String, target: String, detail: Detail, ok: bool, summary: String, ms: u64, output: String },
    Summary { text: String, before: u32, after: u32 },
    Notice(String),
    Error(String),
}

/// Sesiones iniciadas en servidores MCP remotos.
pub fn sessions_path() -> PathBuf {
    path().map(|p| p.with_extension("sesiones")).unwrap_or_default()
}

/// Carpeta de la memoria.
pub fn memory_dir() -> PathBuf {
    path().map(|p| p.with_extension("memoria")).unwrap_or_default()
}

/// Carpeta de las habilidades del usuario.
pub fn skills_dir() -> PathBuf {
    match std::env::var_os("FORGE_SKILLS") {
        Some(p) => p.into(),
        None => dirs::home_dir().unwrap_or_default().join(".config").join("forge").join("skills"),
    }
}

/// Carpeta de las conversaciones.
fn chats_dir() -> Option<PathBuf> {
    Some(path()?.with_extension("d"))
}

/// Archivo para una conversación nueva.
pub fn new_file() -> Option<PathBuf> {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    Some(chats_dir()?.join(format!("{ms}-{}.json", std::process::id())))
}

/// Fichas de las conversaciones guardadas. Lo que no se entiende se salta.
pub fn list() -> Vec<(PathBuf, Meta)> {
    chats_dir().map(|d| list_in(&d)).unwrap_or_default()
}

fn list_in(dir: &Path) -> Vec<(PathBuf, Meta)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| {
            let mut line = String::new();
            std::io::BufReader::new(std::fs::File::open(&p).ok()?).read_line(&mut line).ok()?;
            let meta: Meta = serde_json::from_str(&line).ok().filter(|m: &Meta| m.v == VERSION)?;
            Some((p, meta))
        })
        .collect()
}

pub fn load(p: &Path) -> Result<Stored, String> {
    let text = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
    let body = text.split_once('\n').map(|(_, b)| b).ok_or("el archivo está incompleto")?;
    serde_json::from_str(body).map_err(|e| e.to_string())
}

pub fn store(p: &Path, meta: &Meta, body: &Stored) -> std::io::Result<()> {
    files::write_private(p, &format!("{}\n{}\n", serde_json::to_string(meta)?, serde_json::to_string(body)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn conversations() {
        use forge_agent::tools::{DiffLine, LineKind};
        let dir = std::env::temp_dir().join(format!("forge-conv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let body = Stored {
            busy: true,
            stats: Some(Stats { prompt: 1, output: 2, speed: Some(3.0) }),
            history: vec![Msg::User("hola\nñ".into())],
            items: vec![
                StoredItem::User("hola\nñ".into()),
                StoredItem::Tool {
                    verb: "Editar".into(),
                    target: "a.rs".into(),
                    detail: Detail::Diff {
                        lines: vec![DiffLine { kind: LineKind::Add, text: "x".into() }],
                        added: 1,
                        removed: 0,
                        new: false,
                    },
                    ok: true,
                    summary: String::new(),
                    ms: 5,
                    output: "Done.".into(),
                },
            ],
        };
        let p = dir.join("1.json");
        let wt = Worktree {
            path: "/x.worktrees/agente-1".into(),
            repo: "/x".into(),
            branch: "forge/agente-1".into(),
            base: "abc".into(),
        };
        let meta = Meta {
            v: VERSION,
            title: "Un título\ncon salto".into(),
            root: Some(wt.path.clone()),
            worktree: Some(wt.clone()),
            settled: false,
            updated: 7,
        };
        store(&p, &meta, &body).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        // Basura, versiones futuras y restos no estorban.
        std::fs::write(dir.join("2.json"), "{mal").unwrap();
        std::fs::write(dir.join("3.json"), r#"{"v":99,"title":"","root":null,"updated":1}"#).unwrap();
        std::fs::write(dir.join("4.tmp"), "").unwrap();
        let l = list_in(&dir);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].1.worktree, Some(wt));
        // Las de antes de que hubiera copias se siguen leyendo.
        std::fs::write(dir.join("5.json"), r#"{"v":1,"title":"vieja","root":null,"updated":1}"#).unwrap();
        assert_eq!(list_in(&dir).len(), 2);
        assert_eq!((l[0].1.title.as_str(), l[0].1.updated), ("Un título\ncon salto", 7));
        assert_eq!(load(&p).unwrap(), body);
        // Un archivo a medias es un error, no una conversación vacía.
        let t = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, &t[..t.len() - 20]).unwrap();
        assert!(load(&p).is_err());
        let _ = std::fs::remove_dir_all(dir);
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

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 CurroRaya y los colaboradores de forge

//! Escaneo incremental del historial de los asistentes.
//!
//! Recuerda cuánto ha leído de cada archivo, así que llamar a [`Scanner::scan`]
//! periódicamente solo procesa lo nuevo. Los formatos de líneas (Claude, Codex)
//! se leen desde el último offset; los JSON completos (Gemini) se releen solo
//! cuando cambian su tamaño o fecha.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use walkdir::WalkDir;

use crate::provider::{FileMode, Provider};
use crate::record::{RateLimits, UsageRecord};
use crate::{claude, codex, gemini};

#[derive(Debug, Default)]
struct FileState {
    offset: u64,
    len: u64,
    mtime: Option<SystemTime>,
    codex: codex::FileCtx,
}

#[derive(Debug, Default)]
pub struct Scanner {
    sources: Vec<(Provider, Vec<PathBuf>)>,
    files: HashMap<PathBuf, FileState>,
    seen: HashSet<String>,
    records: BTreeMap<Provider, Vec<UsageRecord>>,
    limits: BTreeMap<Provider, RateLimits>,
}

impl Scanner {
    /// Escanea los proveedores indicados en sus rutas por defecto.
    pub fn new(providers: &[Provider]) -> Self {
        Self::with_roots(providers.iter().map(|p| (*p, p.roots())).collect())
    }

    /// Escanea todos los proveedores detectados en esta máquina.
    pub fn auto() -> Self {
        Self::new(&Provider::detect_all())
    }

    /// Rutas explícitas por proveedor (tests, configuraciones a medida).
    pub fn with_roots(sources: Vec<(Provider, Vec<PathBuf>)>) -> Self {
        let mut s = Self { sources, ..Default::default() };
        for (p, _) in &s.sources {
            s.records.entry(*p).or_default();
        }
        s
    }

    /// Proveedores que se escanean.
    pub fn providers(&self) -> Vec<Provider> {
        self.sources.iter().map(|(p, _)| *p).collect()
    }

    /// ¿Tiene algún directorio con historial?
    pub fn has_sources(&self) -> bool {
        self.sources.iter().any(|(_, r)| !r.is_empty())
    }

    /// Registros de un proveedor, ordenados por fecha.
    pub fn records(&self, p: Provider) -> &[UsageRecord] {
        self.records.get(&p).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Últimos límites oficiales publicados por el proveedor, si los hay.
    pub fn limits(&self, p: Provider) -> Option<&RateLimits> {
        self.limits.get(&p)
    }

    /// Lee las novedades. Devuelve cuántos registros nuevos se añadieron.
    pub fn scan(&mut self) -> usize {
        let mut added = 0;
        let sources = self.sources.clone();
        for (provider, roots) in sources {
            let files: Vec<PathBuf> = roots
                .iter()
                .flat_map(|r| WalkDir::new(r).follow_links(false).into_iter().flatten())
                .filter(|e| e.file_type().is_file())
                .map(|e| e.into_path())
                .filter(|p| provider.wants(p))
                .collect();
            let before = self.records(provider).len();
            for f in files {
                // Un archivo ilegible no debe impedir leer el resto.
                let _ = match provider.mode() {
                    FileMode::Append => self.scan_append(provider, &f),
                    FileMode::Whole => self.scan_whole(provider, &f),
                };
            }
            let list = self.records.entry(provider).or_default();
            if list.len() > before {
                added += list.len() - before;
                list.sort_by_key(|r| r.timestamp);
            }
        }
        added
    }

    fn push(&mut self, rec: UsageRecord) {
        if self.seen.insert(rec.dedupe_key.clone()) {
            self.records.entry(rec.provider).or_default().push(rec);
        }
    }

    fn scan_append(&mut self, provider: Provider, path: &Path) -> std::io::Result<()> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let mut state = self.files.remove(path).unwrap_or_default();
        if len < state.offset {
            state = FileState::default(); // archivo truncado o reemplazado
        }
        if len == state.offset {
            self.files.insert(path.to_path_buf(), state);
            return Ok(());
        }
        file.seek(SeekFrom::Start(state.offset))?;
        let mut reader = BufReader::new(file);
        let key = path.to_string_lossy().into_owned();
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf)?;
            if n == 0 || !buf.ends_with('\n') {
                break; // fin, o línea a medio escribir: se leerá la próxima vez
            }
            state.offset += n as u64;
            match provider {
                Provider::Claude => {
                    if let Some(r) = claude::parse_line(&buf) {
                        self.push(r);
                    }
                }
                Provider::Codex => {
                    let out = codex::parse_line(&mut state.codex, &key, &buf);
                    if let Some(r) = out.record {
                        self.push(r);
                    }
                    if let Some(l) = out.limits {
                        let newer =
                            self.limits.get(&provider).is_none_or(|old| l.observed_at >= old.observed_at);
                        if newer {
                            self.limits.insert(provider, l);
                        }
                    }
                }
                Provider::Gemini => {}
            }
        }
        state.len = len;
        self.files.insert(path.to_path_buf(), state);
        Ok(())
    }

    fn scan_whole(&mut self, provider: Provider, path: &Path) -> std::io::Result<()> {
        let meta = std::fs::metadata(path)?;
        let (len, mtime) = (meta.len(), meta.modified().ok());
        if let Some(st) = self.files.get(path)
            && st.len == len
            && st.mtime == mtime
        {
            return Ok(());
        }
        let content = std::fs::read_to_string(path)?;
        let key = path.to_string_lossy().into_owned();
        if provider == Provider::Gemini {
            for r in gemini::parse_file(&key, &content) {
                self.push(r);
            }
        }
        self.files.insert(path.to_path_buf(), FileState { len, mtime, ..Default::default() });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("forge-scan-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn claude_line(id: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","requestId":"r{id}","message":{{"model":"claude-opus-5-5","id":"m{id}","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
        )
    }

    #[test]
    fn claude_incremental_and_dedup() {
        let dir = tmpdir("claude");
        std::fs::create_dir_all(dir.join("proj")).unwrap();
        let f = dir.join("proj/a.jsonl");
        let mut fh = File::create(&f).unwrap();
        writeln!(fh, "{}", claude_line("1", "2026-11-22T10:00:00Z")).unwrap();
        writeln!(fh, "{}", claude_line("1", "2026-11-22T10:00:00Z")).unwrap(); // duplicado
        write!(fh, "{}", claude_line("2", "2026-11-22T10:01:00Z")).unwrap(); // sin \n aún
        fh.flush().unwrap();

        let mut s = Scanner::with_roots(vec![(Provider::Claude, vec![dir.clone()])]);
        assert_eq!(s.scan(), 1);
        writeln!(fh).unwrap();
        writeln!(fh, "{}", claude_line("3", "2026-11-22T09:00:00Z")).unwrap();
        fh.flush().unwrap();
        assert_eq!(s.scan(), 2);
        assert_eq!(s.scan(), 0);
        assert_eq!(s.records(Provider::Claude)[0].dedupe_key, "m3:r3"); // ordenado por fecha
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn codex_and_gemini_side_by_side() {
        let codex_dir = tmpdir("codex");
        std::fs::create_dir_all(codex_dir.join("2026/11/22")).unwrap();
        std::fs::write(
            codex_dir.join("2026/11/22/rollout-x.jsonl"),
            concat!(
                r#"{"timestamp":"2026-11-22T10:00:01Z","type":"turn_context","payload":{"model":"gpt-5-codex","cwd":"/p"}}"#,
                "\n",
                r#"{"timestamp":"2026-11-22T10:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":10,"total_tokens":110}},"rate_limits":{"primary":{"used_percent":12.0,"window_minutes":300}}}}"#,
                "\n"
            ),
        )
        .unwrap();
        let gem_dir = tmpdir("gemini");
        std::fs::create_dir_all(gem_dir.join("h/chats")).unwrap();
        let gf = gem_dir.join("h/chats/session-1.json");
        let gem = |n: usize| {
            let msgs: Vec<String> = (0..n)
                .map(|i| format!(r#"{{"id":"g{i}","timestamp":"2026-11-22T10:0{i}:00Z","model":"gemini-2.5-pro","tokens":{{"input":10,"output":5}}}}"#))
                .collect();
            format!(r#"{{"sessionId":"s","messages":[{}]}}"#, msgs.join(","))
        };
        std::fs::write(&gf, gem(1)).unwrap();

        let mut s = Scanner::with_roots(vec![
            (Provider::Codex, vec![codex_dir.clone()]),
            (Provider::Gemini, vec![gem_dir.clone()]),
        ]);
        assert_eq!(s.scan(), 2);
        assert_eq!(s.records(Provider::Codex).len(), 1);
        assert_eq!(s.records(Provider::Gemini).len(), 1);
        assert!((s.limits(Provider::Codex).unwrap().windows[0].used_percent - 12.0).abs() < 1e-9);

        // Gemini reescribe el archivo con un mensaje más: solo cuenta el nuevo.
        std::fs::write(&gf, gem(2)).unwrap();
        assert_eq!(s.scan(), 1);
        assert_eq!(s.records(Provider::Gemini).len(), 2);
        std::fs::remove_dir_all(codex_dir).ok();
        std::fs::remove_dir_all(gem_dir).ok();
    }
}

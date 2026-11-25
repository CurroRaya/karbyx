// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! Bloques estilo Warp: cada comando con su salida es una unidad.
//!
//! La shell marca las fases con secuencias OSC 133 (estándar FinalTerm):
//! `A` empieza el prompt, `B` empieza el comando, `C` empieza la salida y
//! `D;código` termina. Además `OSC 633;E;texto` trae el comando tal cual.
//!
//! [`OscScanner`] detecta esas marcas en el flujo de bytes del PTY (aunque
//! lleguen partidas entre lecturas). Al aplicarlas, la terminal marca las
//! celdas con un hipervínculo interno `karbyx://block/N/in|out`: así la marca
//! viaja con el texto al hacer scroll o redimensionar.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

/// Marca recibida de la shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellMark {
    PromptStart,
    CommandStart,
    OutputStart,
    Finished(Option<i32>),
    CommandLine(String),
}

const MAX_PAYLOAD: usize = 4096;

#[derive(Debug, Clone, Default)]
enum State {
    #[default]
    Ground,
    Esc,
    /// Dentro de `ESC ]`: acumulando el contenido hasta BEL o ST.
    Osc(Vec<u8>),
    /// `ESC` dentro de una OSC (posible ST = `ESC \`).
    OscEsc(Vec<u8>),
}

/// Detector incremental de OSC 133 / 633 en bytes de la PTY.
#[derive(Debug, Default)]
pub struct OscScanner {
    state: State,
}

impl OscScanner {
    /// Procesa un trozo y devuelve las marcas encontradas con la posición
    /// (exclusiva) del byte donde termina cada una.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<(usize, ShellMark)> {
        let mut out = Vec::new();
        for (i, &b) in bytes.iter().enumerate() {
            let state = std::mem::take(&mut self.state);
            self.state = match state {
                State::Ground => {
                    if b == 0x1b {
                        State::Esc
                    } else {
                        State::Ground
                    }
                }
                State::Esc => match b {
                    b']' => State::Osc(Vec::new()),
                    0x1b => State::Esc,
                    _ => State::Ground,
                },
                State::Osc(mut buf) => match b {
                    0x07 => {
                        if let Some(m) = parse(&buf) {
                            out.push((i + 1, m));
                        }
                        State::Ground
                    }
                    0x1b => State::OscEsc(buf),
                    _ => {
                        if buf.len() < MAX_PAYLOAD {
                            buf.push(b);
                            State::Osc(buf)
                        } else {
                            State::Ground // demasiado larga: no es nuestra
                        }
                    }
                },
                State::OscEsc(buf) => {
                    if b == b'\\' {
                        if let Some(m) = parse(&buf) {
                            out.push((i + 1, m));
                        }
                        State::Ground
                    } else if b == b']' {
                        State::Osc(Vec::new())
                    } else {
                        State::Ground
                    }
                }
            };
        }
        out
    }
}

fn parse(payload: &[u8]) -> Option<ShellMark> {
    let s = std::str::from_utf8(payload).ok()?;
    if let Some(rest) = s.strip_prefix("133;") {
        let mut parts = rest.split(';');
        return match parts.next()? {
            "A" => Some(ShellMark::PromptStart),
            "B" => Some(ShellMark::CommandStart),
            "C" => Some(ShellMark::OutputStart),
            "D" => Some(ShellMark::Finished(parts.next().and_then(|c| c.trim().parse().ok()))),
            _ => None,
        };
    }
    s.strip_prefix("633;E;").map(|cmd| ShellMark::CommandLine(cmd.to_string()))
}

/// Datos de un bloque que no están en el grid.
#[derive(Debug, Clone, Default)]
pub struct BlockInfo {
    pub command: Option<String>,
    pub started: Option<Instant>,
    pub finished: Option<Instant>,
    pub exit: Option<i32>,
}

impl BlockInfo {
    pub fn running(&self) -> bool {
        self.started.is_some() && self.finished.is_none()
    }

    pub fn duration(&self) -> Option<Duration> {
        Some(self.finished?.saturating_duration_since(self.started?))
    }

    pub fn failed(&self) -> bool {
        self.exit.is_some_and(|c| c != 0)
    }
}

const MAX_BLOCKS: usize = 1000;

/// Tabla de bloques de una sesión.
#[derive(Debug, Default)]
pub struct BlockTable {
    next: u64,
    current: Option<u64>,
    blocks: BTreeMap<u64, BlockInfo>,
}

impl BlockTable {
    /// Abre un bloque nuevo (marca A) y devuelve su id.
    pub fn start(&mut self) -> u64 {
        self.next += 1;
        let id = self.next;
        self.current = Some(id);
        self.blocks.insert(id, BlockInfo::default());
        while self.blocks.len() > MAX_BLOCKS {
            self.blocks.pop_first();
        }
        id
    }

    pub fn current(&self) -> Option<u64> {
        self.current
    }

    pub fn current_mut(&mut self) -> Option<&mut BlockInfo> {
        self.blocks.get_mut(&self.current?)
    }

    pub fn get(&self, id: u64) -> Option<&BlockInfo> {
        self.blocks.get(&id)
    }

    pub fn snapshot(&self, ids: impl IntoIterator<Item = u64>) -> HashMap<u64, BlockInfo> {
        ids.into_iter().filter_map(|id| Some((id, self.blocks.get(&id)?.clone()))).collect()
    }
}

pub const URI_PREFIX: &str = "karbyx://block/";

/// URI interna de las celdas de un bloque.
pub fn block_uri(id: u64, output: bool) -> String {
    format!("{URI_PREFIX}{id}/{}", if output { "out" } else { "in" })
}

/// `karbyx://block/12/out` → `(12, true)`.
pub fn parse_block_uri(uri: &str) -> Option<(u64, bool)> {
    let rest = uri.strip_prefix(URI_PREFIX)?;
    let (id, part) = rest.split_once('/')?;
    Some((id.parse().ok()?, part == "out"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_marks_with_both_terminators() {
        let mut s = OscScanner::default();
        let data =
            b"hola\x1b]133;A\x07$ \x1b]133;B\x1b\\ls\x1b]633;E;ls -la\x07\x1b]133;C\x07x\x1b]133;D;2\x07";
        let marks = s.feed(data);
        let kinds: Vec<_> = marks.iter().map(|(_, m)| m.clone()).collect();
        assert_eq!(
            kinds,
            vec![
                ShellMark::PromptStart,
                ShellMark::CommandStart,
                ShellMark::CommandLine("ls -la".into()),
                ShellMark::OutputStart,
                ShellMark::Finished(Some(2)),
            ]
        );
        // El offset apunta justo detrás del terminador.
        assert_eq!(&data[..marks[0].0], b"hola\x1b]133;A\x07");
    }

    #[test]
    fn marks_split_across_reads() {
        let mut s = OscScanner::default();
        assert!(s.feed(b"abc\x1b]13").is_empty());
        assert!(s.feed(b"3;D").is_empty());
        let m = s.feed(b";0\x07rest");
        assert_eq!(m, vec![(3, ShellMark::Finished(Some(0)))]);
    }

    #[test]
    fn ignores_other_sequences() {
        let mut s = OscScanner::default();
        assert!(s.feed(b"\x1b]0;titulo\x07\x1b]8;;http://x\x07\x1b[31mrojo").is_empty());
        assert_eq!(s.feed(b"\x1b]133;D\x07"), vec![(8, ShellMark::Finished(None))]);
    }

    #[test]
    fn uris_and_table() {
        assert_eq!(parse_block_uri(&block_uri(7, true)), Some((7, true)));
        assert_eq!(parse_block_uri(&block_uri(7, false)), Some((7, false)));
        assert_eq!(parse_block_uri("https://x"), None);

        let mut t = BlockTable::default();
        let a = t.start();
        t.current_mut().unwrap().started = Some(Instant::now());
        assert!(t.get(a).unwrap().running());
        t.current_mut().unwrap().finished = Some(Instant::now());
        t.current_mut().unwrap().exit = Some(1);
        assert!(t.get(a).unwrap().failed());
        assert_eq!(t.start(), a + 1);
    }
}

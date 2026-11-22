// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 CurroRaya y los colaboradores de forge

//! E/S del PTY: hilo lector (parser VT + marcas de bloque) y hilo escritor.
//!
//! Sustituye al `EventLoop` de alacritty para poder aplicar las marcas OSC 133
//! en el punto exacto del flujo: los bytes se parsean hasta el final de cada
//! marca y la marca se aplica con el cursor justo en su sitio.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::{Handler, Hyperlink, Processor, StdSyncHandler};
use crossbeam_channel::{Receiver, Sender};

use crate::TermEvent;
use crate::blocks::{BlockTable, OscScanner, ShellMark, block_uri};

const READ_BUFFER: usize = 64 * 1024;
/// Máximo de bytes que se procesan antes de avisar a la UI.
const MAX_BATCH: usize = 1024 * 1024;

/// Espera a que el descriptor esté listo. `timeout_ms < 0`: sin límite.
/// Devuelve `None` si hubo error, `Some(false)` si venció el plazo.
fn wait_fd(fd: i32, events: i16, timeout_ms: i32) -> Option<(bool, i16)> {
    let mut pfd = libc::pollfd { fd, events, revents: 0 };
    loop {
        // SAFETY: un único pollfd válido.
        let r = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if r >= 0 {
            return Some((r > 0, pfd.revents));
        }
        if std::io::Error::last_os_error().kind() != ErrorKind::Interrupted {
            return None;
        }
    }
}

/// Aplica una marca de la shell al terminal y a la tabla de bloques.
fn apply_mark<L: EventListener>(
    term: &mut Term<L>,
    table: &Mutex<BlockTable>,
    mark: ShellMark,
    events: &Sender<TermEvent>,
) {
    let Ok(mut t) = table.lock() else { return };
    let link = |id: u64, output: bool| {
        Some(Hyperlink { id: Some(format!("forge{id}")), uri: block_uri(id, output) })
    };
    match mark {
        ShellMark::PromptStart => {
            let id = t.start();
            term.set_hyperlink(link(id, false));
        }
        ShellMark::CommandStart => {}
        ShellMark::CommandLine(cmd) => {
            if let Some(b) = t.current_mut() {
                let cmd = cmd.trim();
                b.command = (!cmd.is_empty()).then(|| cmd.to_string());
            }
        }
        ShellMark::OutputStart => {
            if let Some(id) = t.current() {
                if let Some(b) = t.current_mut() {
                    b.started = Some(Instant::now());
                }
                term.set_hyperlink(link(id, true));
            }
        }
        ShellMark::Finished(code) => {
            term.set_hyperlink(None);
            let id = t.current();
            if let (Some(id), Some(b)) = (id, t.current_mut())
                && b.started.is_some()
                && b.finished.is_none()
            {
                b.finished = Some(Instant::now());
                b.exit = code;
                let _ = events.send(TermEvent::CommandFinished {
                    id,
                    exit: code,
                    duration: b.duration().unwrap_or_default(),
                    command: b.command.clone(),
                });
            }
        }
    }
}

/// Arranca el hilo lector. Termina cuando el proceso hijo cierra el PTY.
pub(crate) fn spawn_reader<L: EventListener + Send + 'static>(
    mut file: File,
    term: Arc<FairMutex<Term<L>>>,
    listener: L,
    table: Arc<Mutex<BlockTable>>,
    events: Sender<TermEvent>,
) {
    std::thread::Builder::new()
        .name("pty-reader".into())
        .spawn(move || {
            let fd = file.as_raw_fd();
            let mut processor: Processor<StdSyncHandler> = Processor::new();
            let mut scanner = OscScanner::default();
            let mut buf = vec![0u8; READ_BUFFER];
            'outer: loop {
                // Si hay una actualización sincronizada a medias, esperar como mucho hasta su plazo.
                let timeout = processor
                    .sync_timeout()
                    .sync_timeout()
                    .map(|t| t.saturating_duration_since(Instant::now()).as_millis() as i32)
                    .unwrap_or(-1);
                let Some((ready, revents)) = wait_fd(fd, libc::POLLIN, timeout) else { break };
                if !ready {
                    processor.stop_sync(&mut *term.lock());
                    listener.send_event(Event::Wakeup);
                    continue;
                }

                // Leer todo lo disponible (hasta un tope) antes de despertar a la UI.
                let mut batch = 0;
                loop {
                    match file.read(&mut buf) {
                        Ok(0) => break 'outer,
                        Ok(n) => {
                            let chunk = &buf[..n];
                            let marks = scanner.feed(chunk);
                            let mut t = term.lock();
                            let mut prev = 0;
                            for (end, mark) in marks {
                                processor.advance(&mut *t, &chunk[prev..end]);
                                apply_mark(&mut t, &table, mark, &events);
                                prev = end;
                            }
                            processor.advance(&mut *t, &chunk[prev..]);
                            drop(t);
                            batch += n;
                            if batch >= MAX_BATCH {
                                break;
                            }
                        }
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {
                            if batch == 0 && revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                                break 'outer;
                            }
                            break;
                        }
                        // EIO: el hijo cerró el PTY.
                        Err(_) => break 'outer,
                    }
                }
                listener.send_event(Event::Wakeup);
            }
            listener.send_event(Event::Exit);
        })
        .expect("hilo lector del PTY");
}

/// Arranca el hilo escritor: lo que llega por el canal se escribe en el PTY.
pub(crate) fn spawn_writer(mut file: File, rx: Receiver<Vec<u8>>) {
    std::thread::Builder::new()
        .name("pty-writer".into())
        .spawn(move || {
            let fd = file.as_raw_fd();
            for bytes in rx {
                let mut off = 0;
                while off < bytes.len() {
                    match file.write(&bytes[off..]) {
                        Ok(n) => off += n,
                        Err(e) if e.kind() == ErrorKind::Interrupted => {}
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {
                            if wait_fd(fd, libc::POLLOUT, 1000).is_none() {
                                return;
                            }
                        }
                        Err(_) => return,
                    }
                }
            }
        })
        .expect("hilo escritor del PTY");
}

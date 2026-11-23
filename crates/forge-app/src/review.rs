// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Panel «Revisar cambios» (⌘R): el diff del trabajo de un agente (o de
//! cualquier terminal en un repositorio git), con fusionar y descartar
//! para los agentes que trabajan en su propio worktree.
//!
//! Git corre siempre en un hilo aparte: la ventana nunca espera.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use winit::event::KeyEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};

use super::{App, PaneKind, UserEvent};
use crate::panes::PaneId;
use crate::worktree::{self, FileDiff, Worktree};

/// Cada cuánto se recalcula el diff con el panel abierto.
const REFRESH: Duration = Duration::from_secs(2);

pub(super) struct Review {
    pub pane: PaneId,
    /// Carpeta de la que se saca el diff.
    pub dir: Option<PathBuf>,
    /// Commit de referencia (`HEAD` si no es un worktree de agente).
    pub base: String,
    pub worktree: Option<Worktree>,
    pub project: String,
    /// Rama donde se fusiona (o la rama actual).
    pub target: String,
    /// `None` mientras carga la primera vez.
    pub files: Option<Result<Vec<FileDiff>, String>>,
    pub sel: usize,
    pub scroll: usize,
    /// Filas de diff visibles en el último frame (para RePág/AvPág).
    pub rows: usize,
    pub confirm_discard: bool,
    pub message: Option<(String, bool)>,
    /// Hay una operación de git en marcha.
    pub busy: bool,
    pub loading: bool,
    pub refreshed: Instant,
}

#[derive(Debug)]
pub(super) enum ReviewEvent {
    Loaded { pane: PaneId, files: Result<Vec<FileDiff>, String>, target: String },
    Merged { pane: PaneId, result: Result<(String, Option<String>), String> },
    Removed { pane: PaneId, result: Result<String, String> },
}

impl Review {
    pub fn file_count(&self) -> usize {
        match &self.files {
            Some(Ok(f)) => f.len(),
            _ => 0,
        }
    }

    pub fn max_scroll(&self) -> usize {
        match &self.files {
            Some(Ok(f)) => f.get(self.sel).map_or(0, |f| f.lines.len().saturating_sub(self.rows.max(1))),
            _ => 0,
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let s = self.scroll as isize + delta;
        self.scroll = (s.max(0) as usize).min(self.max_scroll());
    }
}

impl App {
    /// Abre (o cierra) el panel para el panel con foco.
    pub(super) fn toggle_review(&mut self) {
        if self.review.is_some() {
            self.review = None;
            return;
        }
        let Some(id) = self.focused_id() else { return };
        let Some(p) = self.panes.get(&id) else { return };
        let wt = p.worktree.clone().or_else(|| p.info.cwd.as_deref().and_then(worktree::detect));
        let dir = wt.as_ref().map(|w| w.path.clone()).or_else(|| p.info.cwd.clone());
        let base = wt.as_ref().map(|w| w.base.clone()).unwrap_or_else(|| "HEAD".into());
        let project = super::project_of(dir.as_deref());
        self.inbox = None;
        self.dashboard = None;
        self.review = Some(Review {
            pane: id,
            dir,
            base,
            project,
            worktree: wt,
            target: String::new(),
            files: None,
            sel: 0,
            scroll: 0,
            rows: 20,
            confirm_discard: false,
            message: None,
            busy: false,
            loading: false,
            refreshed: Instant::now(),
        });
        self.refresh_review();
    }

    /// Recalcula el diff en segundo plano.
    pub(super) fn refresh_review(&mut self) {
        let proxy = self.proxy.clone();
        let Some(r) = &mut self.review else { return };
        if r.loading {
            return;
        }
        let Some(dir) = r.dir.clone() else {
            r.files = Some(Err("no sé en qué carpeta está esta terminal".into()));
            return;
        };
        r.loading = true;
        r.refreshed = Instant::now();
        let (pane, base) = (r.pane, r.base.clone());
        let target_dir = r.worktree.as_ref().map(|w| w.repo.clone()).unwrap_or_else(|| dir.clone());
        std::thread::spawn(move || {
            let files = worktree::diff(&dir, &base);
            let target = worktree::current_branch(&target_dir);
            let _ = proxy.send_event(UserEvent::Review(ReviewEvent::Loaded { pane, files, target }));
        });
    }

    /// Refresco periódico mientras el panel está abierto.
    pub(super) fn tick_review(&mut self) {
        if self.review.as_ref().is_some_and(|r| !r.busy && r.refreshed.elapsed() >= REFRESH) {
            self.refresh_review();
        }
    }

    pub(super) fn on_review(&mut self, ev: ReviewEvent) {
        let current = self.review.as_ref().map(|r| r.pane);
        match ev {
            ReviewEvent::Loaded { pane, files, target } if current == Some(pane) => {
                let Some(r) = &mut self.review else { return };
                r.loading = false;
                // Conserva el archivo elegido aunque cambie la lista.
                let chosen = match &r.files {
                    Some(Ok(f)) => f.get(r.sel).map(|f| f.path.clone()),
                    _ => None,
                };
                if let Ok(new) = &files {
                    let idx = chosen.and_then(|c| new.iter().position(|f| f.path == c));
                    if idx != Some(r.sel) {
                        r.scroll = 0;
                    }
                    r.sel = idx.unwrap_or(0).min(new.len().saturating_sub(1));
                }
                r.files = Some(files);
                r.target = target;
                r.scroll = r.scroll.min(r.max_scroll());
            }
            ReviewEvent::Loaded { .. } => {}
            ReviewEvent::Merged { pane, result } => {
                if let (Ok((_, Some(head))), Some(p)) = (&result, self.panes.get_mut(&pane))
                    && let Some(w) = &mut p.worktree
                {
                    w.base = head.clone();
                }
                if let Some(r) = self.review.as_mut().filter(|r| r.pane == pane) {
                    r.busy = false;
                    match result {
                        Ok((msg, head)) => {
                            if let (Some(h), Some(w)) = (head, &mut r.worktree) {
                                w.base = h.clone();
                                r.base = h;
                            }
                            r.message =
                                Some((format!("{msg}. ⌫ cierra el agente y borra su worktree."), false));
                        }
                        Err(e) => r.message = Some((e, true)),
                    }
                    self.refresh_review();
                }
            }
            ReviewEvent::Removed { pane, result } => {
                if let Some(r) = self.review.as_mut().filter(|r| r.pane == pane) {
                    r.busy = false;
                    match result {
                        Ok(_) => self.review = None,
                        Err(e) => r.message = Some((e, true)),
                    }
                }
            }
        }
        self.redraw();
    }

    fn review_merge(&mut self) {
        let task = self
            .review
            .as_ref()
            .and_then(|r| self.panes.get(&r.pane))
            .and_then(|p| p.agent.as_ref()?.task.clone());
        let proxy = self.proxy.clone();
        let Some(r) = &mut self.review else { return };
        let Some(wt) = r.worktree.clone() else { return };
        if r.busy {
            return;
        }
        r.busy = true;
        r.confirm_discard = false;
        r.message = Some((format!("Fusionando {} en {}…", wt.name(), r.target), false));
        let pane = r.pane;
        let message = task.unwrap_or_else(|| format!("trabajo de {}", wt.name()));
        std::thread::spawn(move || {
            let result = worktree::merge(&wt, &message).map(|msg| (msg, worktree::head(&wt.path)));
            let _ = proxy.send_event(UserEvent::Review(ReviewEvent::Merged { pane, result }));
        });
    }

    /// Primera pulsación: pide confirmación. Segunda: cierra el agente y
    /// borra su worktree y su rama.
    fn review_discard(&mut self, event_loop: &ActiveEventLoop) {
        let Some(r) = &mut self.review else { return };
        let Some(wt) = r.worktree.clone() else { return };
        if r.busy {
            return;
        }
        if !r.confirm_discard {
            r.confirm_discard = true;
            return;
        }
        r.confirm_discard = false;
        r.busy = true;
        r.message = Some((format!("Borrando {}…", wt.name()), false));
        let pane = r.pane;
        // Primero se cierra el agente, para que no siga escribiendo ahí. Si
        // era la última terminal, se abre otra para no cerrar forge.
        let in_worktree: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(id, p)| {
                **id == pane
                    || p.worktree.as_ref() == Some(&wt)
                    || p.info.cwd.as_deref().is_some_and(|c| c.starts_with(&wt.path))
            })
            .map(|(id, _)| *id)
            .collect();
        if in_worktree.len() == self.panes.len() {
            self.open_tab(PaneKind::Shell);
        }
        for id in in_worktree {
            self.remove_pane(id, event_loop);
        }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = worktree::remove(&wt).map(|_| format!("{} borrado", wt.name()));
            let _ = proxy.send_event(UserEvent::Review(ReviewEvent::Removed { pane, result }));
        });
    }

    /// Teclas con el panel abierto. Las consume todas.
    pub(super) fn review_key(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) -> bool {
        let Some(r) = &mut self.review else { return false };
        let n = r.file_count();
        let page = r.rows.max(2) as isize - 1;
        let confirm = std::mem::take(&mut r.confirm_discard);
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                if !confirm {
                    self.review = None;
                }
            }
            Key::Named(NamedKey::ArrowDown) if n > 0 => {
                r.sel = (r.sel + 1) % n;
                r.scroll = 0;
            }
            Key::Named(NamedKey::ArrowUp) if n > 0 => {
                r.sel = (r.sel + n - 1) % n;
                r.scroll = 0;
            }
            Key::Named(NamedKey::PageDown | NamedKey::Space) => r.scroll_by(page),
            Key::Named(NamedKey::PageUp) => r.scroll_by(-page),
            Key::Named(NamedKey::Home) => r.scroll = 0,
            Key::Named(NamedKey::End) => r.scroll = r.max_scroll(),
            Key::Character(c) if c.as_str() == "j" => r.scroll_by(1),
            Key::Character(c) if c.as_str() == "k" => r.scroll_by(-1),
            Key::Character(c) if c.as_str() == "r" => {
                r.message = None;
                self.refresh_review();
            }
            Key::Named(NamedKey::Enter) => self.review_merge(),
            Key::Named(NamedKey::Backspace | NamedKey::Delete) => {
                if let Some(r) = &mut self.review {
                    r.confirm_discard = confirm;
                }
                self.review_discard(event_loop);
            }
            _ => {}
        }
        self.redraw();
        true
    }

    /// Rueda del ratón sobre el panel.
    pub(super) fn review_wheel(&mut self, lines: f64) {
        self.scroll_accum += lines;
        let whole = self.scroll_accum.trunc() as isize;
        if whole == 0 {
            return;
        }
        self.scroll_accum -= whole as f64;
        if let Some(r) = &mut self.review {
            r.scroll_by(-whole);
        }
        self.redraw();
    }

    pub(super) fn review_click(&mut self, hit: crate::ui::Hit, event_loop: &ActiveEventLoop) {
        use crate::ui::Hit;
        match hit {
            Hit::ReviewFile(i) => {
                if let Some(r) = &mut self.review
                    && i != r.sel
                {
                    r.sel = i;
                    r.scroll = 0;
                }
            }
            Hit::ReviewMerge => self.review_merge(),
            Hit::ReviewDiscard => self.review_discard(event_loop),
            _ => {}
        }
        self.redraw();
    }
}

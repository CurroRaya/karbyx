// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! Panel «Revisar cambios» (⌘R): el diff del trabajo de un agente (o de
//! cualquier terminal en un repositorio git), con fusionar y descartar
//! para los agentes que trabajan en su propio worktree.
//!
//! Git corre siempre en un hilo aparte: la ventana nunca espera.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use winit::event::KeyEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};

use super::{App, PaneKind, UserEvent};
use crate::owner;
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
    /// Lo descartado, para apuntarlo en «Hecho hoy» cuando se borre.
    pub discarded: Option<(String, String)>,
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

    /// Diff nuevo: conserva el archivo elegido aunque cambie la lista.
    fn loaded(&mut self, files: Result<Vec<FileDiff>, String>, target: String) {
        self.loading = false;
        let chosen = match &self.files {
            Some(Ok(f)) => f.get(self.sel).map(|f| f.path.clone()),
            _ => None,
        };
        if let Ok(new) = &files {
            let idx = chosen.and_then(|c| new.iter().position(|f| f.path == c));
            if idx != Some(self.sel) {
                self.scroll = 0;
            }
            self.sel = idx.unwrap_or(0).min(new.len().saturating_sub(1));
        }
        self.files = Some(files);
        self.target = target;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// Empieza a fusionar, si se puede.
    fn start_merge(&mut self) -> Option<Worktree> {
        let wt = self.worktree.clone().filter(|_| !self.busy)?;
        self.busy = true;
        self.confirm_discard = false;
        self.message = Some((format!("Fusionando {} en {}…", wt.name(), self.target), false));
        Some(wt)
    }

    /// Empieza a borrar, si se puede. La primera vez solo pide confirmación.
    fn start_discard(&mut self) -> Option<Worktree> {
        let wt = self.worktree.clone().filter(|_| !self.busy)?;
        if !self.confirm_discard {
            self.confirm_discard = true;
            return None;
        }
        self.confirm_discard = false;
        self.busy = true;
        self.message = Some((format!("Borrando {}…", wt.name()), false));
        Some(wt)
    }

    /// Fin de la fusión.
    fn merged(&mut self, result: Result<(String, Option<String>), String>) {
        self.busy = false;
        match result {
            Ok((msg, head)) => {
                if let (Some(h), Some(w)) = (head, &mut self.worktree) {
                    w.base = h.clone();
                    self.base = h;
                }
                self.message = Some((format!("{msg}. ⌫ cierra el agente y borra su worktree."), false));
            }
            Err(e) => self.message = Some((e, true)),
        }
    }

    fn scroll_by(&mut self, delta: isize) {
        let s = self.scroll as isize + delta;
        self.scroll = (s.max(0) as usize).min(self.max_scroll());
    }
}

impl App {
    /// Abre (o cierra) el panel para lo que se está viendo.
    pub(super) fn toggle_review(&mut self) {
        if self.review.is_some() {
            self.review = None;
            return;
        }
        let owner = match self.chats.current() {
            Some(c) if self.chat_visible => Some(owner::of_chat(c.id)),
            _ => self.focused_id(),
        };
        if let Some(o) = owner {
            self.open_review(o);
        }
    }

    pub(super) fn open_review(&mut self, id: PaneId) {
        let cwd = self.dir_of(id);
        let wt = self.worktree_of(id).or_else(|| cwd.as_deref().and_then(worktree::detect));
        let dir = wt.as_ref().map(|w| w.path.clone()).or(cwd);
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
            discarded: None,
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
                if let Some(r) = &mut self.review {
                    r.loaded(files, target);
                }
            }
            ReviewEvent::Loaded { .. } => {}
            ReviewEvent::Merged { pane, result } => {
                if let Ok((_, Some(head))) = &result {
                    self.set_base_of(pane, head);
                }
                if result.is_ok()
                    && let Some(done) = self.done_entry(pane)
                {
                    self.log_done(done.0, done.1, true);
                }
                if let Some(r) = self.review.as_mut().filter(|r| r.pane == pane) {
                    r.merged(result);
                    self.refresh_review();
                }
            }
            ReviewEvent::Removed { pane, result } => {
                if let Some(r) = self.review.as_mut().filter(|r| r.pane == pane) {
                    r.busy = false;
                    match result {
                        Ok(_) => {
                            if let Some((title, project)) = r.discarded.take() {
                                self.log_done(title, project, false);
                            }
                            self.review = None;
                        }
                        Err(e) => r.message = Some((e, true)),
                    }
                }
            }
        }
        self.redraw();
    }

    /// Título y proyecto del trabajo del panel, para «Hecho hoy».
    fn done_entry(&self, pane: PaneId) -> Option<(String, String)> {
        let r = self.review.as_ref().filter(|r| r.pane == pane)?;
        let wt = r.worktree.as_ref()?;
        let title = self.title_of(pane).unwrap_or_else(|| wt.name().to_string());
        Some((title, r.project.clone()))
    }

    pub(super) fn review_merge(&mut self) {
        let task = self.review.as_ref().and_then(|r| self.title_of(r.pane));
        let proxy = self.proxy.clone();
        let Some(r) = &mut self.review else { return };
        let Some(wt) = r.start_merge() else { return };
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
        let Some(wt) = r.start_discard() else { return };
        let pane = r.pane;
        let done = self.done_entry(pane);
        if let Some(r) = &mut self.review {
            r.discarded = done;
        }
        // Primero se cierra el agente, para que no siga escribiendo ahí. Si
        // era la última terminal, se abre otra para no cerrar Karbyx.
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
        // Los modelos locales que trabajen ahí se paran antes de borrar.
        let working = self.leave_worktree(&wt);
        if in_worktree.len() == self.panes.len() {
            self.open_tab(PaneKind::Shell);
        }
        for id in in_worktree {
            self.remove_pane(id, event_loop);
        }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let start = Instant::now();
            while working.iter().any(|w| w.load(Ordering::Acquire))
                && start.elapsed() < Duration::from_secs(5)
            {
                std::thread::sleep(Duration::from_millis(30));
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn review() -> Review {
        Review {
            pane: 1,
            dir: None,
            base: "viejo".into(),
            worktree: Some(Worktree {
                path: "/w/agente-1".into(),
                repo: "/w".into(),
                branch: "karbyx/agente-1".into(),
                base: "viejo".into(),
            }),
            project: "p".into(),
            target: "main".into(),
            files: None,
            sel: 0,
            scroll: 0,
            rows: 10,
            confirm_discard: false,
            message: None,
            busy: false,
            loading: true,
            refreshed: Instant::now(),
            discarded: None,
        }
    }

    fn file(path: &str, lines: usize) -> FileDiff {
        let l =
            worktree::DiffLine { kind: worktree::LineKind::Context, old: None, new: None, text: "".into() };
        FileDiff { path: path.into(), status: 'M', added: 0, removed: 0, lines: vec![l; lines] }
    }

    #[test]
    fn discarding_needs_two_presses_and_nothing_running() {
        let mut r = review();
        assert!(r.start_discard().is_none() && r.confirm_discard && !r.busy, "la primera solo pregunta");
        assert!(r.start_discard().is_some() && r.busy && !r.confirm_discard);
        assert!(r.start_discard().is_none() && r.start_merge().is_none(), "ya hay algo en marcha");
        // Ni siquiera se pregunta mientras se fusiona.
        let mut r = review();
        assert!(r.start_merge().is_some());
        assert!(r.start_discard().is_none() && !r.confirm_discard);
        // Fusionar olvida una confirmación a medias.
        let mut r = review();
        r.start_discard();
        assert!(r.start_merge().is_some() && !r.confirm_discard);
        // Una terminal cualquiera no tiene nada que fusionar ni borrar.
        let mut r = review();
        r.worktree = None;
        assert!(r.start_merge().is_none() && r.start_discard().is_none() && !r.busy && !r.confirm_discard);
    }

    #[test]
    fn after_merging_the_diff_starts_again() {
        let mut r = review();
        r.start_merge();
        r.merged(Err("conflictos en a.rs".into()));
        assert_eq!(r.message, Some(("conflictos en a.rs".into(), true)));
        assert!(!r.busy && r.base == "viejo", "con conflictos no cambia nada");
        r.start_merge();
        r.merged(Ok(("Fusionado".into(), Some("nuevo".into()))));
        assert!(!r.busy && r.base == "nuevo" && r.worktree.as_ref().unwrap().base == "nuevo");
        assert!(r.message.as_ref().is_some_and(|(_, error)| !error));
    }

    #[test]
    fn keeps_the_chosen_file() {
        let mut r = review();
        r.loaded(Ok(vec![file("a", 5), file("b", 50)]), "main".into());
        (r.sel, r.scroll) = (1, 40);
        // Aparece otro delante: se sigue viendo el mismo.
        r.loaded(Ok(vec![file("0", 5), file("a", 5), file("b", 50)]), "main".into());
        assert_eq!(r.sel, 2);
        // En el mismo sitio y más corto: no se queda pasado del final.
        r.scroll = 40;
        r.loaded(Ok(vec![file("0", 5), file("a", 5), file("b", 12)]), "main".into());
        assert_eq!((r.sel, r.scroll), (2, 2));
        // Ya no está, o no se pudo leer.
        r.loaded(Ok(vec![file("a", 5)]), "main".into());
        assert_eq!((r.sel, r.scroll), (0, 0));
        r.loaded(Err("git".into()), "main".into());
        assert_eq!((r.file_count(), r.max_scroll(), r.loading), (0, 0, false));
        r.scroll_by(5);
        assert_eq!(r.scroll, 0);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! La mesa: la pantalla principal de forge. En vez de una shell, tus
//! agentes en columnas según lo que necesitan de ti, y arriba una barra
//! «¿Qué hay que hacer?» que lanza un agente nuevo, en su propio worktree,
//! con esa tarea ya escrita.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use forge_ai::Provider;
use winit::event::KeyEvent;
use winit::keyboard::{Key, NamedKey};

use super::{App, PaneKind, Spawn, UserEvent, ago, project_root, state_text};
use crate::agents::AgentState;
use crate::chat::ChatCard;
use crate::owner;
use crate::panes::PaneId;
use crate::ui::{self, Align, Area, Hit, Rgb, Ui};
use crate::usage::UsageState;
use crate::verify::{self, Ask, Outcome, Start, Verify};
use crate::worktree;
use forge_term::{CellView, Snapshot};

/// Cada cuánto se recalculan los +/− de los worktrees con la mesa a la vista.
const STATS_EVERY: Duration = Duration::from_secs(3);

/// Columnas de la mesa.
pub const COLUMNS: [&str; 4] = ["Trabajando", "Te necesita", "Para revisar", "Hecho hoy"];

/// Algo terminado hoy (fusionado o descartado).
#[derive(Debug, Clone)]
pub struct DoneItem {
    pub title: String,
    pub project: String,
    pub merged: bool,
    pub at: DateTime<Local>,
}

/// Cambios de un worktree: (añadidas, quitadas, archivos).
pub type Stats = (usize, usize, usize);

#[derive(Default)]
pub(super) struct Board {
    /// Texto de la barra de tarea.
    pub input: String,
    /// Proyecto donde se lanzará el agente (raíz).
    pub project: Option<PathBuf>,
    /// Tarjeta seleccionada (panel).
    pub sel: Option<PaneId>,
    pub stats: HashMap<PaneId, Stats>,
    stats_at: Option<Instant>,
    stats_loading: bool,
    pub done: Vec<DoneItem>,
    /// Último aviso (p. ej. por qué no hubo worktree).
    pub notice: Option<(String, bool)>,
    /// Verificación de cada agente (en curso o terminada).
    pub verify: HashMap<PaneId, Verify>,
    verify_gen: u64,
}

/// La verificación tal como se ve en la tarjeta.
#[derive(Debug, Clone)]
pub struct Badge {
    /// `None` mientras se ejecuta.
    pub outcome: Option<Outcome>,
    pub cmd: Option<String>,
    pub since: Instant,
    pub took: Option<Duration>,
}

impl Badge {
    fn of(v: &Verify) -> Self {
        Self {
            outcome: v.report.as_ref().map(|r| r.outcome.clone()),
            cmd: v.report.as_ref().and_then(|r| r.cmd.clone()),
            since: v.since,
            took: v.report.as_ref().map(|r| r.took),
        }
    }
}

/// Quién puede hacer una tarea.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Worker {
    Local,
    Ai(Provider),
}

impl Worker {
    fn id(self) -> &'static str {
        match self {
            Worker::Local => "local",
            Worker::Ai(p) => p.command(),
        }
    }
}

/// Una tarjeta de agente.
pub struct Card {
    pub pane: PaneId,
    pub title: String,
    pub project: String,
    pub branch: Option<String>,
    pub detail: String,
    pub preview: Vec<String>,
    pub stats: Option<Stats>,
    pub elapsed: String,
    pub tokens: Option<u64>,
    pub state: Option<AgentState>,
    pub ai: Option<Provider>,
    pub unseen: bool,
    /// Pregunta con opciones que el agente tiene en pantalla.
    pub choice: Option<Choice>,
    /// Miniatura en vivo de su terminal.
    pub thumb: Thumb,
    /// Resultado de la verificación de sus cambios.
    pub verify: Option<Badge>,
    /// Se puede verificar a mano (tiene cambios y no está trabajando).
    pub can_verify: bool,
}

/// Un tramo de una fila de la miniatura: `len` celdas desde `start`, del
/// color del texto (o del fondo, si `bg`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Run {
    pub start: u16,
    pub len: u16,
    pub color: Rgb,
    pub bg: bool,
}

/// Miniatura de una terminal: sus últimas filas con contenido, como tramos
/// de color (un minimapa), y el cursor.
#[derive(Debug, Clone, Default)]
pub struct Thumb {
    pub cols: usize,
    pub rows: Vec<Vec<Run>>,
    pub cursor: Option<(usize, usize)>,
}

/// Tramos de una fila: fondos distintos del de la terminal y texto.
pub fn row_runs(row: &[CellView], background: Rgb) -> Vec<Run> {
    let mut v = Vec::new();
    let mut push = |start: usize, end: usize, color: Rgb, bg: bool| {
        v.push(Run { start: start as u16, len: (end - start) as u16, color, bg });
    };
    let mut c = 0;
    while c < row.len() {
        let bg = row[c].bg;
        let start = c;
        while c < row.len() && row[c].bg == bg {
            c += 1;
        }
        if bg != background {
            push(start, c, bg, true);
        }
    }
    let ink = |cell: &CellView| cell.spacer || !(cell.c == ' ' || cell.c == '\0');
    let mut c = 0;
    while c < row.len() {
        if !ink(&row[c]) {
            c += 1;
            continue;
        }
        let fg = row[c].fg;
        let start = c;
        while c < row.len() && ink(&row[c]) && (row[c].spacer || row[c].fg == fg) {
            c += 1;
        }
        push(start, c, fg, false);
    }
    v
}

/// Miniatura de la pantalla: como mucho `max_rows` filas, acabando en la
/// última con contenido (o en el cursor).
pub fn thumb(snap: &Snapshot, max_rows: usize) -> Thumb {
    let rows: Vec<Vec<Run>> = (0..snap.rows).map(|r| row_runs(snap.row(r), snap.background)).collect();
    let cursor = snap.cursor.visible.then_some(snap.cursor.row);
    let last = rows.iter().rposition(|r| !r.is_empty()).max(cursor);
    let Some(last) = last else { return Thumb { cols: snap.cols, ..Thumb::default() } };
    let first = (last + 1).saturating_sub(max_rows);
    Thumb {
        cols: snap.cols,
        cursor: cursor.filter(|r| (first..=last).contains(r)).map(|r| (r - first, snap.cursor.col)),
        rows: rows[first..=last].to_vec(),
    }
}

/// Un menú de opciones del asistente («Do you want to…? ❯ 1. Yes  2. No»).
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub question: String,
    /// Texto de cada opción, en orden (la 1 es la primera).
    pub options: Vec<String>,
    /// Opción marcada ahora con ❯ (índice desde 0).
    pub current: usize,
}

/// Una línea de opción: (marcada, número, texto).
fn option_line(l: &str) -> Option<(bool, usize, String)> {
    let t = l.trim();
    let (marked, rest) = match t.strip_prefix(['❯', '›', '>']) {
        Some(r) => (true, r.trim_start()),
        None => (false, t),
    };
    let (num, label) = rest.split_once(". ")?;
    let n: usize = num.parse().ok().filter(|n| (1..=9).contains(n))?;
    let label = label.trim();
    (!label.is_empty()).then(|| (marked, n, label.to_string()))
}

/// Busca un menú de opciones al final del texto de una terminal.
pub fn parse_choice(text: &str) -> Option<Choice> {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim().trim_matches(['│', '┃', '|']).trim())
        .filter(|l| !l.is_empty() && !l.chars().all(|c| ('\u{2500}'..='\u{257f}').contains(&c)))
        .collect();
    let lines = &lines[lines.len().saturating_sub(30)..];
    // Las opciones tienen que estar al final (como mucho unas líneas de ayuda debajo).
    let last = lines.iter().rposition(|l| option_line(l).is_some())?;
    if lines.len() - 1 - last > 3 {
        return None;
    }
    let mut first = last;
    while first > 0 && option_line(lines[first - 1]).is_some() {
        first -= 1;
    }
    let opts: Vec<(bool, usize, String)> =
        lines[first..=last].iter().filter_map(|l| option_line(l)).collect();
    let numbered = opts.iter().enumerate().all(|(i, o)| o.1 == i + 1);
    let marked: Vec<usize> = opts.iter().enumerate().filter(|(_, o)| o.0).map(|(i, _)| i).collect();
    if opts.len() < 2 || !numbered || marked.len() != 1 {
        return None;
    }
    let above = &lines[first.saturating_sub(4)..first];
    let question = above
        .iter()
        .rev()
        .find(|l| l.ends_with('?'))
        .or(above.last())
        .map(|l| l.to_string())
        .unwrap_or_default();
    Some(Choice { question, options: opts.into_iter().map(|o| o.2).collect(), current: marked[0] })
}

/// Etiqueta corta en español para el botón de una opción.
pub fn short_option(label: &str) -> String {
    let mut t = label.trim().to_string();
    // Quita la pista de tecla del final: «(esc)», «(shift+tab)».
    if t.ends_with(')')
        && let Some(i) = t.rfind(" (")
    {
        t.truncate(i);
    }
    let l = t.to_lowercase();
    if l == "yes" || l == "sí" || l == "si" {
        "Sí".into()
    } else if l.starts_with("yes")
        && (l.contains("don't ask") || l.contains("allow all") || l.contains("always"))
    {
        "Sí, siempre".into()
    } else if l == "no" || l.starts_with("no,") {
        "No".into()
    } else {
        crate::agents::one_line(&t, 20)
    }
}

/// Traduce las preguntas habituales de Claude Code.
pub fn translate_question(q: &str) -> String {
    let t = q.trim();
    let rest = |p: &str| t.strip_prefix(p).map(|r| r.trim_end_matches('?').trim().to_string());
    if let Some(f) = rest("Do you want to make this edit to") {
        format!("¿Aplico este cambio en {f}?")
    } else if let Some(f) = rest("Do you want to create") {
        format!("¿Creo {f}?")
    } else if t.starts_with("Do you want to allow Claude to fetch") {
        "¿Dejo que Claude lea esta página?".into()
    } else if t == "Do you want to proceed?" {
        "¿Sigo adelante?".into()
    } else if t.is_empty() {
        "¿Qué hago?".into()
    } else {
        t.to_string()
    }
}

/// Bytes para elegir la opción `idx` (desde 0) de un menú.
fn answer_bytes(choice: &Choice, idx: usize) -> Vec<u8> {
    if idx == choice.current { b"\r".to_vec() } else { (idx + 1).to_string().into_bytes() }
}

/// Una terminal normal (sin agente), en la tira de abajo.
pub struct ShellChip {
    pub pane: PaneId,
    pub label: String,
}

/// Todo lo que la mesa necesita para pintarse.
pub struct BoardView {
    pub columns: [Vec<Card>; 3],
    /// Conversaciones del chat, en las mismas columnas.
    pub chats: [Vec<ChatCard>; 3],
    pub done: Vec<DoneItem>,
    pub shells: Vec<ShellChip>,
    pub input: String,
    pub project: String,
    /// Quién hará la tarea.
    pub worker: String,
    pub selected: Option<PaneId>,
    pub today: u64,
    pub notice: Option<(String, bool)>,
}

/// ¿Parece el prompt de una shell (`usuario@máquina carpeta %`)?
fn is_prompt(t: &str) -> bool {
    let first = t.split_whitespace().next().unwrap_or_default();
    first.contains('@') && (t.ends_with('%') || t.ends_with('$') || t.contains(" % ") || t.contains(" $ "))
}

/// Últimas líneas útiles de una terminal: sin vacías ni marcos de la
/// interfaz del asistente (cajas de texto, barras de estado).
pub fn preview_lines(text: &str, n: usize) -> Vec<String> {
    let boxy = |l: &str| {
        let t = l.trim();
        t.chars().next().is_some_and(|c| ('\u{2500}'..='\u{257f}').contains(&c) || c == '>' || c == '❯')
            || t.contains("for shortcuts")
            || is_prompt(t)
            || t.starts_with("Last login:")
            || t.contains("esc to interrupt")
    };
    let mut v: Vec<String> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty() && !boxy(l))
        .map(|l| l.trim_start_matches(['⏺', '●', '⎿', ' ']).to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let keep = v.len().saturating_sub(n);
    v.drain(..keep);
    v
}

impl App {
    /// Proyectos a los que lanzar un agente: los de las terminales abiertas.
    pub(super) fn board_projects(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = Vec::new();
        let mut add = |p: PathBuf| {
            if !v.contains(&p) {
                v.push(p);
            }
        };
        if let Some(cwd) = self.focused_pane().and_then(|p| p.info.cwd.as_deref()) {
            add(project_root(cwd));
        }
        let mut others: Vec<PathBuf> =
            self.panes.values().filter_map(|p| p.info.cwd.as_deref()).map(project_root).collect();
        others.sort();
        for p in others {
            add(p);
        }
        if v.is_empty()
            && let Some(cwd) = self.new_session_cwd()
        {
            v.push(cwd);
        }
        v
    }

    fn board_project(&self) -> Option<PathBuf> {
        let projects = self.board_projects();
        self.board.project.clone().filter(|p| projects.contains(p)).or_else(|| projects.into_iter().next())
    }

    pub(super) fn board_view(&self, usage: &UsageState) -> BoardView {
        let mut columns: [Vec<Card>; 3] = Default::default();
        let mut shells = Vec::new();
        for ti in self.display_order() {
            for id in self.tabs[ti].root.leaves() {
                let Some(p) = self.panes.get(&id) else { continue };
                let (name, ai) = p.title();
                let Some(provider) = ai else {
                    let folder = p
                        .info
                        .cwd
                        .as_deref()
                        .and_then(|c| c.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "~".into());
                    let label = if folder == p.project {
                        format!("{name} · {folder}")
                    } else {
                        format!("{name} · {} / {folder}", p.project)
                    };
                    shells.push(ShellChip { pane: id, label });
                    continue;
                };
                let a = p.agent.as_ref();
                let state = a.map(|a| a.state);
                let tail = p.session.tail_text(30);
                let choice = parse_choice(&tail);
                // Un menú en pantalla es que te necesita, diga lo que diga el estado.
                let col = match state {
                    _ if choice.is_some() => 1,
                    Some(AgentState::Waiting) => 1,
                    Some(AgentState::Done) => 2,
                    _ => 0,
                };
                let tokens = a.and_then(|a| a.session.as_ref()).and_then(|sid| {
                    usage.views.iter().find(|u| u.provider == provider)?.sessions.get(sid).copied()
                });
                let detail = match a {
                    Some(a) => a.detail.clone().unwrap_or_else(|| state_text(a.state).into()),
                    None => "Arrancando…".into(),
                };
                columns[col].push(Card {
                    pane: id,
                    title: a.and_then(|a| a.task.clone()).or_else(|| p.label.clone()).unwrap_or(name),
                    project: p.project.clone(),
                    branch: p.worktree.as_ref().map(|w| w.name().to_string()),
                    detail,
                    preview: preview_lines(&tail, 1),
                    stats: self.board.stats.get(&id).copied(),
                    elapsed: a.map(|a| ago(a.since)).unwrap_or_default(),
                    tokens,
                    state,
                    ai,
                    unseen: a.is_some_and(|a| a.unseen),
                    choice,
                    thumb: thumb(&p.session.snapshot(), 48),
                    verify: self.board.verify.get(&id).map(Badge::of),
                    can_verify: p.worktree.is_some()
                        && state.is_some_and(|s| s != AgentState::Working)
                        && self.board.stats.get(&id).is_some_and(|s| s.2 > 0),
                });
            }
        }
        let mut chats: [Vec<ChatCard>; 3] = Default::default();
        for mut c in self.chat_cards() {
            let id = owner::of_chat(c.id);
            c.stats = self.board.stats.get(&id).copied();
            c.verify = self.board.verify.get(&id).map(Badge::of);
            c.can_verify = !c.busy && c.branch.is_some() && c.stats.is_some_and(|s| s.2 > 0);
            let col = if c.ask.is_some() {
                1
            } else if c.busy {
                0
            } else {
                2
            };
            chats[col].push(c);
        }
        let project = self
            .board_project()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "~".into());
        let today_start = Local::now().date_naive();
        let worker = match self.board_worker() {
            Worker::Ai(p) => p.name().to_string(),
            Worker::Local => {
                self.chats.model().map_or_else(|| "modelo local".into(), |(_, m)| m.name.clone())
            }
        };
        BoardView {
            columns,
            chats,
            done: self
                .board
                .done
                .iter()
                .filter(|d| d.at.date_naive() == today_start)
                .rev()
                .cloned()
                .collect(),
            shells,
            input: self.board.input.clone(),
            project,
            worker,
            selected: self.board.sel,
            today: usage.views.iter().map(|v| v.today).sum(),
            notice: self.board.notice.clone(),
        }
    }

    /// Paneles de las tarjetas en orden (columna a columna), para ↑↓.
    fn board_order(&self) -> Vec<PaneId> {
        let v = self.board_view(&UsageState::default());
        (0..3)
            .flat_map(|i| {
                let chats = v.chats[i].iter().map(|c| owner::of_chat(c.id));
                chats.chain(v.columns[i].iter().map(|c| c.pane))
            })
            .collect()
    }

    pub(super) fn show_board(&mut self, on: bool) {
        self.board_visible = on;
        if on {
            self.chat_visible = false;
            self.inbox = None;
            self.dashboard = None;
            self.review = None;
            self.close_search();
            self.refresh_board_stats();
        }
        self.redraw();
    }

    /// Recalcula en segundo plano los +/− de los agentes con worktree.
    pub(super) fn refresh_board_stats(&mut self) {
        if self.board.stats_loading {
            return;
        }
        let jobs: Vec<(PaneId, PathBuf, String)> = self
            .panes
            .iter()
            .filter_map(|(id, p)| Some((*id, p.worktree.as_ref()?)))
            .chain(self.chats.list.iter().filter_map(|c| Some((owner::of_chat(c.id), c.worktree.as_ref()?))))
            .filter(|(_, w)| w.path.is_dir())
            .map(|(id, w)| (id, w.path.clone(), w.base.clone()))
            .collect();
        self.board.stats_at = Some(Instant::now());
        if jobs.is_empty() {
            self.board.stats.clear();
            return;
        }
        self.board.stats_loading = true;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let stats = jobs
                .into_iter()
                .filter_map(|(id, path, base)| {
                    let files = worktree::diff(&path, &base).ok()?;
                    let added = files.iter().map(|f| f.added).sum();
                    let removed = files.iter().map(|f| f.removed).sum();
                    Some((id, (added, removed, files.len())))
                })
                .collect();
            let _ = proxy.send_event(UserEvent::BoardStats(stats));
        });
    }

    pub(super) fn on_board_stats(&mut self, stats: Vec<(PaneId, Stats)>) {
        self.board.stats_loading = false;
        self.board.stats = stats.into_iter().collect();
        self.redraw();
    }

    pub(super) fn tick_board(&mut self) {
        // Verificaciones de agentes que ya no están.
        let gone: Vec<PaneId> =
            self.board.verify.keys().copied().filter(|id| self.worktree_of(*id).is_none()).collect();
        for id in gone {
            self.stop_verify(id);
        }
        if self.board_visible && self.board.stats_at.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
            self.refresh_board_stats();
        }
    }

    /// Tras un cambio de estado de un agente: si vuelve a trabajar, su
    /// verificación ya no vale; si termina en su worktree, se verifica.
    pub(super) fn after_agent_change(&mut self, pane: PaneId, before: Option<AgentState>) {
        let Some(p) = self.panes.get(&pane) else { return };
        let Some(a) = &p.agent else { return };
        match a.state {
            AgentState::Working => self.stop_verify(pane),
            AgentState::Done
                if before == Some(AgentState::Working)
                    && a.from_hooks
                    && p.worktree.is_some()
                    && self.config.verify.auto =>
            {
                self.start_verify(pane, true);
            }
            _ => {}
        }
    }

    /// Lo verificado ya no vale.
    pub(super) fn stop_verify(&mut self, owner: PaneId) {
        if let Some(v) = self.board.verify.remove(&owner) {
            v.stop();
        }
    }

    /// Verifica en segundo plano el worktree del agente. `auto`: nadie lo ha
    /// pedido, así que solo se ejecuta si el proyecto lo tiene activado.
    pub(super) fn start_verify(&mut self, pane: PaneId, auto: bool) {
        let Some(wt) = self.worktree_of(pane) else { return };
        if let Some(old) = self.board.verify.remove(&pane) {
            old.stop();
        }
        // El proyecto sale de cuando forge creó el worktree, no de archivos
        // que el agente pueda cambiar.
        let project = wt.repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(configured) = self.config.verify.command(&project) else {
            if !auto {
                self.board.notice = Some((format!("La verificación está desactivada para {project}"), true));
            }
            return;
        };
        let Some(dir) = dirs::cache_dir().map(|d| d.join("forge").join("verify")) else { return };
        self.board.verify_gen += 1;
        let generation = self.board.verify_gen;
        let job = verify::Job {
            start: if auto { Start::Auto { enabled: verify::is_enabled(&wt.repo) } } else { Start::Manual },
            configured,
            timeout: Duration::from_secs(self.config.verify.timeout_secs.max(10)),
            network: self.config.verify.network.contains(&project),
            log: dir.join(format!("{}-{generation}.log", wt.name().replace('/', "-"))),
            dir: wt.path,
            repo: wt.repo,
            base: wt.base,
        };
        let v = Verify::running(generation);
        let cancel = v.cancel.clone();
        self.board.verify.insert(pane, v);
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            if let Some(r) = verify::check(&job, &cancel) {
                let _ = proxy.send_event(UserEvent::Verify(pane, generation, r));
            }
        });
        self.redraw();
    }

    /// Activa la verificación automática del proyecto y verifica ya.
    fn board_verify_enable(&mut self, pane: PaneId) {
        let Some(wt) = self.worktree_of(pane) else { return };
        match verify::enable(&wt.repo) {
            Ok(()) => {
                let name = wt.repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.board.notice = Some((format!("Verificación automática activada para {name}"), false));
                self.start_verify(pane, false);
            }
            Err(e) => self.board.notice = Some((format!("No se pudo activar: {e}"), true)),
        }
    }

    pub(super) fn on_verify(&mut self, pane: PaneId, generation: u64, r: verify::Report) {
        let Some(v) = self.board.verify.get_mut(&pane).filter(|v| v.generation == generation) else { return };
        let outcome = r.outcome.clone();
        v.report = Some(r);
        if !self.focused {
            let project = self.project_of_owner(pane);
            let what = self.title_of(pane).map(|t| format!(" · {t}")).unwrap_or_default();
            match &outcome {
                Outcome::Passed(m) => super::notify("forge", &format!("✓ {m} en {project}{what}")),
                Outcome::Failed(m) => super::notify("forge", &format!("✕ {m} en {project}{what}")),
                Outcome::Skipped(_) | Outcome::Ask(_) => {}
            }
        }
        self.redraw();
    }

    /// Le devuelve al agente el fallo de la verificación para que lo arregle.
    pub(super) fn board_fix(&mut self, pane: PaneId) {
        if let Some(chat) = owner::as_chat(pane) {
            return self.board_fix_chat(chat);
        }
        let name = self.agent_name(pane);
        let Some(prompt) =
            self.board.verify.get(&pane).and_then(|v| v.report.as_ref()).and_then(verify::fix_prompt)
        else {
            return;
        };
        let Some(p) = self.panes.get_mut(&pane) else { return };
        if p.title().1.is_none() {
            self.board.notice = Some((format!("{name} ya no está abierto en esa terminal"), true));
            return;
        }
        if p.agent.as_ref().is_some_and(|a| a.state == AgentState::Working) {
            self.board.notice = Some((format!("{name} está trabajando; espera a que termine"), true));
            return;
        }
        p.session.write(prompt.into_bytes());
        if let Some(a) = p.agent.as_mut() {
            a.unseen = false;
        }
        // El ↵ aparte: si llega pegado al texto, el asistente lo toma por
        // parte de un pegado y no lo envía.
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            let _ = proxy.send_event(UserEvent::WritePane(pane, b"\r".to_vec()));
        });
        self.board.notice = Some((format!("Le he pasado el fallo a {name}"), false));
    }

    /// Lo mismo, para un modelo local.
    fn board_fix_chat(&mut self, chat: u64) {
        let report = self.board.verify.get(&owner::of_chat(chat)).and_then(|v| v.report.as_ref());
        let Some(prompt) = report.and_then(verify::fix_prompt_with_output) else { return };
        self.board.notice = Some(match self.say_to_chat(chat, prompt) {
            Ok(()) => ("Le he pasado el fallo al modelo".into(), false),
            Err(e) => (e, true),
        });
    }

    /// Apunta algo terminado en «Hecho hoy».
    pub(super) fn log_done(&mut self, title: String, project: String, merged: bool) {
        self.board.done.push(DoneItem { title, project, merged, at: Local::now() });
        if self.board.done.len() > 50 {
            self.board.done.remove(0);
        }
    }

    /// Lanza un agente con la tarea escrita, en un worktree del proyecto
    /// elegido (o en su carpeta si no es un repositorio git).
    pub(super) fn board_launch(&mut self) {
        let task = self.board.input.trim().to_string();
        if task.is_empty() {
            return;
        }
        let Some(dir) = self.board_project() else { return };
        let provider = match self.board_worker() {
            Worker::Local => return self.board_launch_local(task, dir),
            Worker::Ai(p) => p,
        };
        let Some(prompts) = dirs::cache_dir().map(|d| d.join("forge").join("prompts")) else { return };
        if std::fs::create_dir_all(&prompts).is_err() {
            return;
        }
        let file = prompts.join(format!("tarea-{}-{}.txt", std::process::id(), self.next_id));
        if std::fs::write(&file, &task).is_err() {
            return;
        }
        let mut extra = Spawn { prompt: Some(file), label: Some(agents_title(&task)), ..Spawn::default() };
        match worktree::create(&dir) {
            Ok(wt) => {
                self.board.notice = Some((format!("Agente lanzado en ⎇ {}", wt.branch), false));
                extra.worktree = Some(wt);
            }
            Err(e) => {
                self.board.notice = Some((format!("Agente lanzado sin worktree: {e}"), true));
                extra.cwd = Some(dir);
            }
        }
        let before = self.next_id;
        if self.open_tab_ext(PaneKind::Ai(provider), extra) {
            self.board.sel = Some(before);
            self.board.input.clear();
        }
    }

    /// Lanza la tarea a tu modelo local, en su propia copia del proyecto.
    fn board_launch_local(&mut self, task: String, dir: PathBuf) {
        let Some(model) = self.chats.model().map(|(_, m)| m.name.clone()) else {
            self.board.notice = Some(("Tu modelo local no responde".into(), true));
            return self.probe_servers();
        };
        let (worktree, note) = match worktree::create(&dir) {
            Ok(wt) => {
                let note = (format!("{model} trabaja en ⎇ {}", wt.branch), false);
                (Some(wt), note)
            }
            // Sin copia trabaja sobre tus archivos: pregunta antes de tocarlos.
            Err(e) => (None, (format!("{model} trabaja sin copia y preguntando: {e}"), true)),
        };
        let root = worktree.as_ref().map_or(dir, |w| w.path.clone());
        self.board.notice = Some(note);
        let id = self.launch_chat(task, root, worktree);
        self.board.sel = Some(owner::of_chat(id));
        self.board.input.clear();
    }

    /// Quiénes pueden hacer una tarea: tu modelo local y los asistentes
    /// instalados.
    pub(super) fn board_workers(&self) -> Vec<Worker> {
        let mut v = Vec::new();
        if !self.chats.servers.is_empty() {
            v.push(Worker::Local);
        }
        v.extend(Provider::detect_all().into_iter().map(Worker::Ai));
        v
    }

    /// El elegido; si no, tu modelo local.
    fn board_worker(&self) -> Worker {
        let all = self.board_workers();
        let chosen = self.chats.saved.worker.as_deref();
        all.iter()
            .copied()
            .find(|w| Some(w.id()) == chosen)
            .or(all.first().copied())
            .unwrap_or(Worker::Ai(self.selected_provider().unwrap_or(Provider::Claude)))
    }

    /// Pasa al siguiente.
    fn board_next_worker(&mut self) {
        let all = self.board_workers();
        let cur = self.board_worker();
        let i = all.iter().position(|w| *w == cur).map_or(0, |i| i + 1);
        if let Some(w) = all.get(i % all.len().max(1)) {
            self.chats.saved.worker = Some(w.id().to_string());
            self.chats.saved.save();
        }
    }

    /// Teclas con la mesa a la vista. Las consume todas.
    pub(super) fn board_key(&mut self, event: &KeyEvent) -> bool {
        let order = self.board_order();
        let pos = self.board.sel.and_then(|s| order.iter().position(|p| *p == s));
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                if !self.board.input.is_empty() {
                    self.board.input.clear();
                } else if self.board.sel.is_some() {
                    self.board.sel = None;
                } else {
                    self.show_board(false);
                }
            }
            Key::Named(NamedKey::Enter) => {
                if !self.board.input.trim().is_empty() {
                    self.board_launch();
                } else if let Some(p) = self.board.sel {
                    self.go_to_owner(p);
                }
            }
            Key::Named(NamedKey::Tab) if self.mods.shift_key() => self.board_next_worker(),
            Key::Named(NamedKey::Tab) => {
                let projects = self.board_projects();
                if !projects.is_empty() {
                    let cur = self.board_project().and_then(|c| projects.iter().position(|p| *p == c));
                    let n = projects.len();
                    let next = match cur {
                        Some(i) => (i + 1) % n,
                        None => 0,
                    };
                    self.board.project = Some(projects[next].clone());
                }
            }
            Key::Named(NamedKey::ArrowDown) if !order.is_empty() => {
                self.board.sel = Some(order[pos.map_or(0, |i| (i + 1) % order.len())]);
            }
            Key::Named(NamedKey::ArrowUp) if !order.is_empty() => {
                let n = order.len();
                self.board.sel = Some(order[pos.map_or(n - 1, |i| (i + n - 1) % n)]);
            }
            Key::Named(NamedKey::Backspace) => {
                if self.mods.alt_key() {
                    // ⌥⌫ borra la última palabra.
                    let t = self.board.input.trim_end().to_string();
                    let cut = t.rfind(' ').map_or(0, |i| i + 1);
                    self.board.input.truncate(cut);
                } else {
                    self.board.input.pop();
                }
            }
            _ => {
                if self.mods.super_key() || self.mods.control_key() {
                    return false;
                }
                // Con la barra vacía y un agente con pregunta seleccionado, 1-9 responde.
                if self.board.input.is_empty()
                    && let Some(sel) = self.board.sel
                    && let Some(d) = event.text.as_deref().and_then(|t| t.parse::<usize>().ok())
                    && (1..=9).contains(&d)
                    && let Some(p) = self.panes.get(&sel)
                    && parse_choice(&p.session.tail_text(30)).is_some_and(|c| d <= c.options.len())
                {
                    self.board_answer(sel, d - 1);
                    self.redraw();
                    return true;
                }
                if let Some(text) = event.text.as_deref().filter(|t| !t.chars().any(char::is_control)) {
                    self.board.input.push_str(text);
                    self.board.notice = None;
                }
            }
        }
        self.redraw();
        true
    }

    /// Responde al menú que el agente `pane` tiene en pantalla. Se vuelve a
    /// leer la pantalla: si el menú ya no está, no se envía nada.
    pub(super) fn board_answer(&mut self, pane: PaneId, idx: usize) {
        let Some(p) = self.panes.get_mut(&pane) else { return };
        let Some(choice) = parse_choice(&p.session.tail_text(30)) else {
            self.board.notice = Some(("Esa pregunta ya no está en pantalla".into(), true));
            return;
        };
        let Some(label) = choice.options.get(idx) else { return };
        p.session.write(answer_bytes(&choice, idx));
        if let Some(a) = p.agent.as_mut() {
            a.unseen = false;
        }
        self.board.notice = Some((format!("Respondido: {}", short_option(label)), false));
    }

    pub(super) fn board_click(&mut self, hit: Hit) {
        match hit {
            Hit::BoardAnswer(p, i) => self.board_answer(p, i as usize),
            Hit::BoardVerify(p) => self.start_verify(p, false),
            Hit::BoardVerifyEnable(p) => self.board_verify_enable(p),
            Hit::BoardFix(p) => self.board_fix(p),
            Hit::BoardCard(p) | Hit::BoardShell(p) => self.go_to_pane(p),
            Hit::BoardReview(p) if owner::as_chat(p).is_some() => self.open_review(p),
            Hit::BoardReview(p) => {
                self.go_to_pane(p);
                self.toggle_review();
            }
            Hit::BoardWorker => self.board_next_worker(),
            Hit::BoardProject => {
                let projects = self.board_projects();
                if !projects.is_empty() {
                    let cur = self.board_project().and_then(|c| projects.iter().position(|p| *p == c));
                    self.board.project = Some(projects[cur.map_or(0, |i| (i + 1) % projects.len())].clone());
                }
            }
            Hit::BoardLaunch => self.board_launch(),
            _ => {}
        }
        self.redraw();
    }

    /// Pega texto en la barra de tarea (⌘V con la mesa a la vista).
    pub(super) fn board_paste(&mut self, text: &str) {
        let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        self.board.input.push_str(&flat);
        self.redraw();
    }
}

/// Título corto para la tarjeta a partir de la tarea.
fn agents_title(task: &str) -> String {
    crate::agents::one_line(task, 80)
}

// ---------- dibujo ----------

use ui::{EMBER, HOT, STEEL};

const CARD_BG: Rgb = [0x1e, 0x1a, 0x17];
const PREVIEW_BG: Rgb = [0x12, 0x10, 0x0e];
const EMBER_DEEP: Rgb = [0xd2, 0x5f, 0x2c];
const ON_EMBER: Rgb = [0x1c, 0x12, 0x0b];
const FAINT: Rgb = [0x5e, 0x55, 0x4d];

/// Temperatura de cada columna: en la forja, al rojo, templado, frío.
fn column_color(i: usize) -> Rgb {
    match i {
        0 => EMBER,
        1 => HOT,
        2 => ui::GREEN,
        _ => STEEL,
    }
}

/// Latido de una brasa (0.4..1), al ritmo del reloj de la interfaz.
fn pulse(tick: u64) -> f32 {
    [0.45, 0.7, 1.0, 0.7][(tick % 4) as usize]
}

/// Pinta la mesa a pantalla completa.
pub fn draw_board(ui: &mut Ui, screen: Area, v: &BoardView, tick: u64, hover: Option<Hit>) {
    ui.fill(screen, ui::PANEL_BG);
    // Calor de la forja que sube desde arriba.
    ui.gradient(Area::new(screen.x, screen.y, screen.w, ui.s(260.0)), (EMBER, 0.045), (EMBER, 0.0), 0.0);
    let pad = ui.s(28.0);
    let x = screen.x + pad;
    let w = screen.w - 2.0 * pad;
    let mut y = screen.y + ui.s(22.0);

    // Marca: el lingote y el nombre.
    let logo = Area::new(x, y + ui.s(9.0), ui.s(26.0), ui.s(26.0));
    ui.glow(logo, EMBER, 0.35, 7.0, 12.0);
    ui.gradient(logo, (HOT, 1.0), (EMBER_DEEP, 1.0), 7.0);
    ui.text("❯", logo.x, logo.y + ui.s(3.0), logo.w, 14.0, ON_EMBER, true, Align::Center);
    ui.text(
        "forge",
        logo.right() + ui.s(10.0),
        y + ui.s(11.0),
        ui.s(80.0),
        17.0,
        ui::TEXT,
        true,
        Align::Left,
    );

    let right_w = ui.s(196.0);
    crate::chat::draw::draw_nav(ui, Area::new(x + w - right_w, y - ui.s(2.0), right_w, ui.s(30.0)), 1, hover);
    ui.text(
        format!("hoy {} tok", ui::tok(v.today)),
        x + w - right_w,
        y + ui.s(33.0),
        right_w,
        11.5,
        ui::DIM,
        false,
        Align::Right,
    );

    // Barra de tarea.
    let bx = x + ui.s(128.0);
    let bar = Area::new(bx, y, x + w - right_w - ui.s(16.0) - bx, ui.s(46.0));
    let focused = !v.input.is_empty();
    if focused {
        ui.glow(bar, EMBER, 0.22, 12.0, 16.0);
    }
    ui.panel(bar, CARD_BG, if focused { EMBER } else { ui::BORDER }, 12.0, 1.0);
    ui.text("✦", bar.x + ui.s(15.0), bar.y + ui.s(12.0), ui.s(18.0), 14.0, EMBER, false, Align::Left);
    // Selector de proyecto y botón de lanzar, a la derecha de la barra.
    let launch = Area::new(bar.right() - ui.s(116.0), bar.y + ui.s(8.0), ui.s(108.0), bar.h - ui.s(16.0));
    let lhover = hover == Some(Hit::BoardLaunch);
    if focused || lhover {
        let top = if lhover { HOT } else { [0xf6, 0x9a, 0x55] };
        ui.gradient(launch, (top, 1.0), (EMBER_DEEP, 1.0), 8.0);
    } else {
        ui.round(launch, ui::HOVER_BG, 1.0, 8.0);
    }
    ui.text(
        "↵ lanzar",
        launch.x,
        launch.y + ui.s(6.0),
        launch.w,
        12.0,
        if focused || lhover { ON_EMBER } else { ui::DIM },
        true,
        Align::Center,
    );
    ui.hit(launch, Hit::BoardLaunch);
    let proj = Area::new(launch.x - ui.s(160.0), launch.y, ui.s(152.0), launch.h);
    let pbg = if hover == Some(Hit::BoardProject) { ui::ACTIVE_BG } else { ui::HOVER_BG };
    ui.round(proj, pbg, 1.0, 8.0);
    ui.text(
        format!("{} ⇥", v.project),
        proj.x + ui.s(8.0),
        proj.y + ui.s(6.0),
        proj.w - ui.s(16.0),
        12.0,
        ui::TEXT,
        false,
        Align::Center,
    );
    ui.hit(proj, Hit::BoardProject);
    let who = Area::new(proj.x - ui.s(168.0), proj.y, ui.s(160.0), proj.h);
    ui.round(who, if hover == Some(Hit::BoardWorker) { ui::ACTIVE_BG } else { ui::HOVER_BG }, 1.0, 8.0);
    ui.text("◆", who.x + ui.s(10.0), who.y + ui.s(6.5), ui.s(14.0), 11.0, EMBER, true, Align::Left);
    ui.text(
        if v.worker.len() > 16 { v.worker.clone() } else { format!("{} ⇧⇥", v.worker) },
        who.x + ui.s(26.0),
        who.y + ui.s(6.0),
        who.w - ui.s(34.0),
        12.0,
        ui::TEXT,
        false,
        Align::Left,
    );
    ui.hit(who, Hit::BoardWorker);
    let tx = bar.x + ui.s(40.0);
    let tw = who.x - tx - ui.s(10.0);
    if v.input.is_empty() {
        ui.text("¿Qué hay que hacer?", tx, bar.y + ui.s(13.0), tw, 14.0, ui::DIM, false, Align::Left);
    } else {
        // Si no cabe, se ve el final (lo que estás escribiendo).
        let max_chars = (tw / ui.s(7.6)).max(10.0) as usize;
        let n = v.input.chars().count();
        let shown: String = if n > max_chars {
            format!("…{}", v.input.chars().skip(n - max_chars + 1).collect::<String>())
        } else {
            v.input.clone()
        };
        let caret = if tick.is_multiple_of(2) { "▏" } else { " " };
        ui.text(format!("{shown}{caret}"), tx, bar.y + ui.s(13.0), tw, 14.0, ui::TEXT, false, Align::Left);
    }
    y += bar.h + ui.s(10.0);
    let (hint, hcolor) = match &v.notice {
        Some((m, err)) => (m.clone(), if *err { HOT } else { ui::GREEN }),
        None => (
            "Escribe una tarea y pulsa ↵: forge lanza un agente en su propio worktree. ⇥ proyecto · ⇧⇥ quién la hace · ↑↓ elegir · 1-3 responder".into(),
            FAINT,
        ),
    };
    ui.text(hint, bar.x + ui.s(4.0), y, bar.w, 11.0, hcolor, false, Align::Left);
    y += ui.s(36.0);

    // Columnas.
    let shells_h = if v.shells.is_empty() { 0.0 } else { ui.s(58.0) };
    let bottom = screen.bottom() - pad - shells_h;
    let gap = ui.s(16.0);
    let cw = (w - 3.0 * gap) / 4.0;
    let empty = v.columns.iter().all(Vec::is_empty) && v.chats.iter().all(Vec::is_empty);
    for (i, title) in COLUMNS.iter().enumerate() {
        let cx = x + i as f32 * (cw + gap);
        let count = if i < 3 { v.columns[i].len() + v.chats[i].len() } else { v.done.len() };
        let color = column_color(i);
        let dot = ui.s(8.0);
        let dot_a = Area::new(cx, y + ui.s(5.0), dot, dot);
        if count > 0 && i < 2 {
            ui.glow(dot_a, color, 0.5 * if i == 0 { pulse(tick) } else { 1.0 }, 4.0, 6.0);
        }
        ui.round(dot_a, color, if count > 0 { 1.0 } else { 0.35 }, 4.0);
        ui.text(title.to_uppercase(), cx + ui.s(16.0), y, cw, 10.5, ui::TEXT, true, Align::Left);
        ui.text(
            count.to_string(),
            cx,
            y,
            cw,
            10.5,
            if count > 0 { color } else { FAINT },
            true,
            Align::Right,
        );
        ui.fill(Area::new(cx, y + ui.s(22.0), cw, 1.0), ui::BORDER);
        ui.fill(Area::new(cx, y + ui.s(22.0), ui.s(36.0), 1.0), color);
        let mut cy = y + ui.s(34.0);
        if count == 0 && !empty {
            let msg =
                ["Ningún agente trabajando", "Nadie te espera", "Nada que revisar", "Aún nada fusionado hoy"]
                    [i];
            ui.text(msg, cx, cy + ui.s(4.0), cw, 11.5, FAINT, false, Align::Left);
        }
        if i == 3 {
            for d in &v.done {
                if cy + ui.s(50.0) > bottom {
                    break;
                }
                let card = Area::new(cx, cy, cw, ui.s(46.0));
                ui.panel(card, [0x19, 0x17, 0x15], ui::BORDER, 9.0, 1.0);
                let (mark, mc) = if d.merged { ("✓", STEEL) } else { ("✕", FAINT) };
                ui.text(
                    mark,
                    card.x + ui.s(11.0),
                    card.y + ui.s(6.0),
                    ui.s(14.0),
                    12.0,
                    mc,
                    true,
                    Align::Left,
                );
                ui.text(
                    &d.title,
                    card.x + ui.s(29.0),
                    card.y + ui.s(6.0),
                    card.w - ui.s(38.0),
                    12.0,
                    [0xc4, 0xbc, 0xb3],
                    false,
                    Align::Left,
                );
                let what = if d.merged { "fusionado" } else { "descartado" };
                ui.text(
                    format!("{} · {what} · {}", d.project, d.at.format("%H:%M")),
                    card.x + ui.s(29.0),
                    card.y + ui.s(25.0),
                    card.w - ui.s(38.0),
                    10.5,
                    FAINT,
                    false,
                    Align::Left,
                );
                cy += card.h + ui.s(8.0);
            }
            continue;
        }
        for (k, c) in v.chats[i].iter().enumerate() {
            let h = ui.s(chat_card_h(c));
            if cy + h > bottom {
                let more = format!("+{} conversaciones", v.chats[i].len() - k);
                ui.text(more, cx, cy + ui.s(2.0), cw, 11.0, ui::DIM, false, Align::Left);
                cy += ui.s(22.0);
                break;
            }
            let sel = v.selected == Some(owner::of_chat(c.id));
            draw_chat_card(ui, Area::new(cx, cy, cw, h), c, i, sel, tick, hover);
            cy += h + ui.s(12.0);
        }
        let cards = &v.columns[i];
        let card_h = ui.s(204.0);
        let fit = (((bottom - cy) / (card_h + ui.s(12.0))).floor() as usize).max(1);
        for (k, c) in cards.iter().enumerate() {
            if k + 1 == fit && cards.len() > fit {
                ui.text(
                    format!("+{} más", cards.len() - k),
                    cx,
                    cy + ui.s(4.0),
                    cw,
                    11.0,
                    ui::DIM,
                    false,
                    Align::Left,
                );
                break;
            }
            draw_card(ui, Area::new(cx, cy, cw, card_h), c, i, v.selected == Some(c.pane), tick, hover);
            cy += card_h + ui.s(12.0);
        }
    }
    if empty {
        let my = y + ui.s(110.0);
        let anvil = Area::new(x + w / 2.0 - ui.s(22.0), my, ui.s(44.0), ui.s(44.0));
        ui.glow(anvil, EMBER, 0.18, 12.0, 22.0);
        ui.panel(anvil, CARD_BG, ui::BORDER, 12.0, 1.0);
        ui.text("✦", anvil.x, anvil.y + ui.s(10.0), anvil.w, 17.0, EMBER, false, Align::Center);
        ui.text("La forja está fría", x, my + ui.s(62.0), w, 18.0, ui::TEXT, true, Align::Center);
        ui.text(
            "Escribe arriba lo que hay que hacer y pulsa ↵. Cada agente trabaja en su propia copia del proyecto; tú revisas y fusionas.",
            x,
            my + ui.s(92.0),
            w,
            12.5,
            ui::DIM,
            false,
            Align::Center,
        );
    }

    // Terminales normales.
    if !v.shells.is_empty() {
        let sy = screen.bottom() - pad - shells_h + ui.s(14.0);
        ui.fill(Area::new(x, sy - ui.s(8.0), w, 1.0), ui::BORDER);
        ui.text("TERMINALES", x, sy + ui.s(8.0), ui.s(100.0), 10.5, FAINT, true, Align::Left);
        let mut chip_x = x + ui.s(100.0);
        let chip_w = ui.s(230.0);
        for s in &v.shells {
            if chip_x + chip_w > x + w {
                break;
            }
            let chip = Area::new(chip_x, sy + ui.s(2.0), chip_w, ui.s(28.0));
            let h = hover == Some(Hit::BoardShell(s.pane));
            ui.panel(chip, if h { ui::ACTIVE_BG } else { CARD_BG }, ui::BORDER, 8.0, 1.0);
            ui.text("❯", chip.x + ui.s(10.0), chip.y + ui.s(6.0), ui.s(14.0), 11.5, EMBER, true, Align::Left);
            ui.text(
                &s.label,
                chip.x + ui.s(26.0),
                chip.y + ui.s(6.0),
                chip.w - ui.s(36.0),
                11.5,
                ui::TEXT,
                false,
                Align::Left,
            );
            ui.hit(chip, Hit::BoardShell(s.pane));
            chip_x += chip_w + ui.s(8.0);
        }
    }
}

/// Pinta la miniatura de una terminal dentro de `a`: cada tramo de texto es
/// una barra de su color; los fondos, bloques tenues.
fn draw_thumb(ui: &mut Ui, a: Area, t: &Thumb, tick: u64) {
    ui.round(a, PREVIEW_BG, 1.0, 6.0);
    let pad = ui.s(6.0);
    let inner = Area::new(a.x + pad, a.y + pad, a.w - 2.0 * pad, a.h - 2.0 * pad);
    if t.cols == 0 || t.rows.is_empty() {
        return;
    }
    let cw = inner.w / t.cols as f32;
    let rh = ui.s(3.0).max(2.0);
    let fit = ((inner.h / rh).floor() as usize).max(1);
    let skip = t.rows.len().saturating_sub(fit);
    for (i, row) in t.rows.iter().skip(skip).enumerate() {
        let y = inner.y + i as f32 * rh;
        for r in row {
            let x = inner.x + r.start as f32 * cw;
            let w = (r.len as f32 * cw).max(1.0);
            if r.bg {
                ui.tint(Area::new(x, y, w, rh), r.color, 0.55);
            } else {
                ui.tint(Area::new(x, y + rh * 0.2, w, (rh * 0.6).max(1.0)), r.color, 0.6);
            }
        }
    }
    if let Some((row, col)) = t.cursor
        && row >= skip
        && tick.is_multiple_of(2)
    {
        let y = inner.y + (row - skip) as f32 * rh;
        ui.tint(Area::new(inner.x + col as f32 * cw, y, cw.max(ui.s(1.5)), rh), EMBER, 0.9);
    }
}

fn draw_card(ui: &mut Ui, a: Area, c: &Card, col: usize, selected: bool, tick: u64, hover: Option<Hit>) {
    let heat = column_color(col);
    let hovered = hover == Some(Hit::BoardCard(c.pane));
    // Lo que te espera brilla; lo seleccionado, también.
    if col == 1 {
        ui.glow(a, HOT, 0.16, 10.0, 14.0);
    } else if selected {
        ui.glow(a, EMBER, 0.14, 10.0, 12.0);
    }
    let border = if selected {
        EMBER
    } else if col == 1 {
        HOT
    } else if hovered {
        [0x4a, 0x3f, 0x36]
    } else {
        ui::BORDER
    };
    let bw = if selected || col == 1 { ui.s(1.5) } else { 1.0 };
    ui.panel(a, if hovered { [0x23, 0x1e, 0x1a] } else { CARD_BG }, border, 10.0, bw);
    // Calor de la tarjeta: un tinte arriba y la barra de temperatura a la izquierda.
    ui.gradient(Area::new(a.x, a.y, a.w, ui.s(46.0)), (heat, 0.05), (heat, 0.0), 10.0);
    let alpha = if col == 0 { pulse(tick) } else { 0.9 };
    let strip = Area::new(a.x + ui.s(5.0), a.y + ui.s(12.0), ui.s(3.0), a.h - ui.s(24.0));
    if col == 0 {
        ui.glow(strip, EMBER, 0.45 * alpha, 2.0, 5.0);
    }
    ui.round(strip, heat, alpha, 1.5);

    let px = ui.s(16.0);
    let iw = a.w - px - ui.s(12.0);
    let (icon, icolor) = ui::state_icon(c.state, c.ai, tick);
    ui.text(icon, a.x + px, a.y + ui.s(10.0), ui.s(16.0), 12.0, icolor, false, Align::Left);
    ui.text(
        &c.title,
        a.x + px + ui.s(18.0),
        a.y + ui.s(9.0),
        iw - ui.s(18.0),
        13.0,
        ui::TEXT,
        true,
        Align::Left,
    );
    let place = match &c.branch {
        Some(b) => format!("{} · ⎇ {b}", c.project),
        None => c.project.clone(),
    };
    ui.text(
        place,
        a.x + px + ui.s(18.0),
        a.y + ui.s(29.0),
        iw - ui.s(18.0),
        10.5,
        ui::DIM,
        false,
        Align::Left,
    );

    if let Some(ch) = &c.choice {
        // La pregunta y un botón por opción: se responde sin abrir la terminal.
        let q = translate_question(&ch.question);
        ui.text(q, a.x + px, a.y + ui.s(50.0), iw, 11.5, HOT, true, Align::Left);
        let shown = ch.options.len().min(3);
        let gap = ui.s(6.0);
        let bw = (iw - gap * (shown as f32 - 1.0)) / shown as f32;
        for (k, opt) in ch.options.iter().take(shown).enumerate() {
            let b = Area::new(a.x + px + k as f32 * (bw + gap), a.y + ui.s(74.0), bw, ui.s(34.0));
            let label = short_option(opt);
            let color = match label.as_str() {
                "Sí" => ui::GREEN,
                "No" => ui::RED,
                _ => EMBER,
            };
            let h = hover == Some(Hit::BoardAnswer(c.pane, k as u8));
            if h {
                ui.glow(b, color, 0.2, 8.0, 8.0);
            }
            ui.round(b, color, if h { 0.3 } else { 0.14 }, 8.0);
            if k == ch.current {
                // La opción que Claude tiene marcada: la que elige ↵ en su terminal.
                ui.round(
                    Area::new(b.x + ui.s(8.0), b.bottom() - ui.s(5.0), b.w - ui.s(16.0), ui.s(2.0)),
                    color,
                    0.6,
                    1.0,
                );
            }
            ui.text(
                format!("{} {label}", k + 1),
                b.x + ui.s(4.0),
                b.y + ui.s(8.0),
                b.w - ui.s(8.0),
                11.5,
                color,
                true,
                Align::Center,
            );
            ui.hit(b, Hit::BoardAnswer(c.pane, k as u8));
        }
        // Debajo, lo que hay en su pantalla, para decidir con contexto.
        let tv = Area::new(a.x + px, a.y + ui.s(116.0), iw, ui.s(56.0));
        draw_thumb(ui, tv, &c.thumb, tick);
    } else {
        // Lo que hace ahora (o el sello de la verificación), su pantalla en
        // miniatura y su última línea.
        let dcolor = if col == 1 { HOT } else { ui::TEXT };
        let tw = draw_verify(
            ui,
            Area::new(a.x + px, a.y + ui.s(44.0), iw, ui.s(24.0)),
            (c.pane, c.verify.as_ref(), c.can_verify),
            tick,
            hover,
        );
        if c.verify.is_none() {
            ui.text(&c.detail, a.x + px, a.y + ui.s(50.0), tw, 11.5, dcolor, false, Align::Left);
        }
        let tv = Area::new(a.x + px, a.y + ui.s(70.0), iw, ui.s(84.0));
        draw_thumb(ui, tv, &c.thumb, tick);
        if let Some(l) = c.preview.last() {
            ui.text(
                l,
                tv.x + ui.s(2.0),
                tv.bottom() + ui.s(5.0),
                tv.w - ui.s(4.0),
                10.0,
                ui::DIM,
                false,
                Align::Left,
            );
            if let Some(lab) = ui.labels.last_mut() {
                lab.font = ui::Font::Mono;
            }
        }
    }

    // Pie: cambios, tiempo y tokens; o el botón de revisar.
    let fy = a.bottom() - ui.s(24.0);
    let mut fx = a.x + px;
    if let Some((add, rem, files)) = c.stats
        && files > 0
    {
        let s = format!("+{add}");
        ui.text(&s, fx, fy, ui.s(60.0), 11.0, ui::GREEN, true, Align::Left);
        fx += ui.s(8.0) + s.len() as f32 * ui.s(7.0);
        let s = format!("−{rem}");
        ui.text(&s, fx, fy, ui.s(60.0), 11.0, ui::RED, true, Align::Left);
        fx += ui.s(14.0) + s.chars().count() as f32 * ui.s(7.0);
    }
    let mut meta = Vec::new();
    if !c.elapsed.is_empty() {
        meta.push(c.elapsed.clone());
    }
    if let Some(t) = c.tokens {
        meta.push(format!("{} tok", ui::tok(t)));
    }
    // Qué se verificó y cuánto tardó.
    if let Some(b) = &c.verify
        && let (Some(cmd), Some(took)) = (&b.cmd, b.took)
    {
        meta.push(format!("{cmd} {} s", took.as_secs()));
    }
    ui.text(meta.join(" · "), fx, fy, a.right() - fx - px, 11.0, ui::DIM, false, Align::Left);
    if c.branch.is_some() && c.stats.is_some_and(|s| s.2 > 0) && col != 0 {
        let b = Area::new(a.right() - ui.s(12.0) - ui.s(88.0), fy - ui.s(5.0), ui.s(88.0), ui.s(24.0));
        let h = hover == Some(Hit::BoardReview(c.pane));
        ui.round(b, ui::GREEN, if h { 0.28 } else { 0.14 }, 7.0);
        ui.text("Revisar ⌘R", b.x, b.y + ui.s(5.0), b.w, 11.0, ui::GREEN, true, Align::Center);
        ui.hit(b, Hit::BoardReview(c.pane));
    }
    if c.unseen {
        let d = Area::new(a.right() - ui.s(16.0), a.y + ui.s(14.0), ui.s(7.0), ui.s(7.0));
        ui.glow(d, HOT, 0.5, 4.0, 5.0);
        ui.round(d, HOT, 1.0, 4.0);
    }
    ui.hit(a, Hit::BoardCard(c.pane));
}

/// Alto de la tarjeta según lo que lleva.
fn chat_card_h(c: &ChatCard) -> f32 {
    let base = if c.ask.is_some() { 138.0 } else { 96.0 };
    base + if c.branch.is_some() { 28.0 } else { 0.0 } + if c.has_badge() { 32.0 } else { 0.0 }
}

/// Tarjeta de una conversación con un modelo local.
fn draw_chat_card(
    ui: &mut Ui,
    a: Area,
    c: &ChatCard,
    col: usize,
    selected: bool,
    tick: u64,
    hover: Option<Hit>,
) {
    let heat = column_color(col);
    let hovered = hover == Some(Hit::BoardChat(c.id));
    if col == 1 {
        ui.glow(a, HOT, 0.16, 10.0, 14.0);
    }
    if selected && col != 1 {
        ui.glow(a, EMBER, 0.14, 10.0, 12.0);
    }
    let border = if selected {
        EMBER
    } else if col == 1 {
        HOT
    } else if hovered {
        [0x4a, 0x3f, 0x36]
    } else {
        ui::BORDER
    };
    let bw = if selected || col == 1 { ui.s(1.5) } else { 1.0 };
    ui.panel(a, if hovered { [0x23, 0x1e, 0x1a] } else { CARD_BG }, border, 10.0, bw);
    ui.gradient(Area::new(a.x, a.y, a.w, ui.s(46.0)), (heat, 0.05), (heat, 0.0), 10.0);
    let alpha = if col == 0 { pulse(tick) } else { 0.9 };
    let strip = Area::new(a.x + ui.s(5.0), a.y + ui.s(12.0), ui.s(3.0), a.h - ui.s(24.0));
    ui.round(strip, heat, alpha, 1.5);
    let px = ui.s(16.0);
    let (x, iw) = (a.x + px, a.w - px - ui.s(12.0));
    let (icon, icolor) = match col {
        0 => ui::state_icon(Some(AgentState::Working), None, tick),
        1 => ("●", HOT),
        _ => ("❯", EMBER),
    };
    let tx = x + ui.s(18.0);
    ui.text(icon, x, a.y + ui.s(10.0), ui.s(16.0), 12.0, icolor, false, Align::Left);
    ui.text(&c.title, tx, a.y + ui.s(9.0), iw - ui.s(30.0), 13.0, ui::TEXT, true, Align::Left);
    let place = match &c.branch {
        Some(b) => format!("{} · ⎇ {b}", c.project),
        None => format!("Chat · {}", c.project),
    };
    ui.text(place, tx, a.y + ui.s(29.0), iw - ui.s(18.0), 10.5, ui::DIM, false, Align::Left);
    let dcolor = if col == 1 { HOT } else { ui::TEXT };
    ui.text(&c.detail, x, a.y + ui.s(50.0), iw, 11.5, dcolor, col == 1, Align::Left);
    if let Some(tool) = c.ask {
        let y = a.y + ui.s(72.0);
        match &c.warn {
            // Lo peligroso se decide viéndolo entero.
            Some(w) => {
                ui.text(format!("⚠ {w}"), x, y, iw, 11.0, HOT, false, Align::Left);
                let b = Area::new(x, y + ui.s(24.0), iw, ui.s(30.0));
                let h = hover == Some(Hit::BoardChat(c.id));
                ui.round(b, EMBER, if h { 0.3 } else { 0.14 }, 8.0);
                ui.text("Ver y decidir", b.x, b.y + ui.s(6.0), b.w, 11.5, EMBER, true, Align::Center);
            }
            None => {
                let w = (iw - ui.s(12.0)) / 3.0;
                let buttons = [
                    ("Permitir", ui::GREEN, Hit::BoardChatAnswer(c.id, tool, true)),
                    ("Ver", EMBER, Hit::BoardChat(c.id)),
                    ("No", ui::RED, Hit::BoardChatAnswer(c.id, tool, false)),
                ];
                for (k, (t, color, hit)) in buttons.into_iter().enumerate() {
                    let b = Area::new(x + k as f32 * (w + ui.s(6.0)), y + ui.s(8.0), w, ui.s(34.0));
                    ui.round(b, color, if hover == Some(hit) { 0.3 } else { 0.14 }, 8.0);
                    ui.text(t, b.x, b.y + ui.s(8.0), b.w, 11.5, color, true, Align::Center);
                    ui.hit(b, hit);
                }
            }
        }
    } else {
        let mut foot = c.last.clone();
        if let Some(u) = c.used {
            let t = format!("{} tok", ui::tok(u as u64));
            ui.text(t, x, a.y + ui.s(70.0), iw, 10.5, FAINT, false, Align::Right);
        }
        if foot.is_empty() {
            foot = DateTime::from_timestamp(c.since, 0)
                .map(|d| d.with_timezone(&Local).format("%H:%M").to_string())
                .unwrap_or_default();
        }
        ui.text(foot, x, a.y + ui.s(70.0), iw - ui.s(64.0), 11.0, ui::DIM, false, Align::Left);
    }
    // Trabajo en su copia: verificación, cambios y revisar.
    let id = owner::of_chat(c.id);
    if c.has_badge() {
        let y = a.bottom() - ui.s(62.0);
        draw_verify(ui, Area::new(x, y, iw, ui.s(24.0)), (id, c.verify.as_ref(), c.can_verify), tick, hover);
    }
    if c.branch.is_some() {
        let fy = a.bottom() - ui.s(26.0);
        match c.stats {
            Some((add, rem, files)) if files > 0 => {
                let s = format!("+{add}");
                let w = ui.s(8.0) + s.len() as f32 * ui.s(7.0);
                ui.text(s, x, fy, w + ui.s(10.0), 11.0, ui::GREEN, true, Align::Left);
                ui.text(format!("−{rem}"), x + w, fy, ui.s(70.0), 11.0, ui::RED, true, Align::Left);
                if col != 0 {
                    let b = Area::new(a.right() - ui.s(100.0), fy - ui.s(5.0), ui.s(88.0), ui.s(24.0));
                    let h = hover == Some(Hit::BoardReview(id));
                    ui.round(b, ui::GREEN, if h { 0.28 } else { 0.14 }, 7.0);
                    ui.text("Revisar", b.x, b.y + ui.s(5.0), b.w, 11.0, ui::GREEN, true, Align::Center);
                    ui.hit(b, Hit::BoardReview(id));
                }
            }
            _ => ui.text("Sin cambios", x, fy, iw, 11.0, FAINT, false, Align::Left),
        }
    }
    if c.unseen {
        let d = Area::new(a.right() - ui.s(16.0), a.y + ui.s(14.0), ui.s(7.0), ui.s(7.0));
        ui.glow(d, HOT, 0.5, 4.0, 5.0);
        ui.round(d, HOT, 1.0, 4.0);
    }
    ui.hit(a, Hit::BoardChat(c.id));
}

/// Botón pequeño de la tarjeta. Devuelve su ancho.
fn small_button(
    ui: &mut Ui,
    right: f32,
    y: f32,
    label: &str,
    color: Rgb,
    hit: Hit,
    hover: Option<Hit>,
) -> f32 {
    let w = ui.s(20.0) + label.chars().count() as f32 * ui.s(6.6);
    let b = Area::new(right - w, y, w, ui.s(24.0));
    let h = hover == Some(hit);
    if h {
        ui.glow(b, color, 0.18, 7.0, 7.0);
    }
    ui.round(b, color, if h { 0.3 } else { 0.14 }, 7.0);
    ui.text(label, b.x, b.y + ui.s(5.0), b.w, 11.0, color, true, Align::Center);
    ui.hit(b, hit);
    w
}

/// Sello de la verificación y sus botones, en la línea de detalle. Devuelve
/// el ancho que queda libre a la izquierda para el texto del detalle.
fn draw_verify(
    ui: &mut Ui,
    a: Area,
    (pane, badge, can_verify): (PaneId, Option<&Badge>, bool),
    tick: u64,
    hover: Option<Hit>,
) -> f32 {
    let gap = ui.s(6.0);
    let Some(b) = badge else {
        if !can_verify {
            return a.w;
        }
        let w = small_button(ui, a.right(), a.y, "Verificar", STEEL, Hit::BoardVerify(pane), hover);
        return a.w - w - gap;
    };
    let mut right = a.right();
    let ty = a.y + ui.s(6.0);
    let (icon, text, color) = match &b.outcome {
        None => {
            const SPIN: [&str; 4] = ["◐", "◓", "◑", "◒"];
            let secs = b.since.elapsed().as_secs();
            (SPIN[(tick % 4) as usize], format!("Verificando… {secs} s"), EMBER)
        }
        Some(Outcome::Passed(m)) => ("✓", m.clone(), ui::GREEN),
        Some(Outcome::Failed(m)) => ("✕", m.clone(), ui::RED),
        Some(Outcome::Skipped(m)) => ("–", m.clone(), ui::DIM),
        Some(Outcome::Ask(Ask::Enable)) => ("?", "¿Verifico?".into(), HOT),
        Some(Outcome::Ask(Ask::Touched(f))) => ("!", format!("Cambió {f}"), HOT),
    };
    if let Some(Outcome::Ask(ask)) = &b.outcome {
        // Nada se ha ejecutado aún: decide el usuario.
        if *ask == Ask::Enable {
            let hit = Hit::BoardVerifyEnable(pane);
            right -= small_button(ui, right, a.y, "Siempre", STEEL, hit, hover) + gap;
        }
        let label = if *ask == Ask::Enable { "Una vez" } else { "Verificar" };
        right -= small_button(ui, right, a.y, label, EMBER, Hit::BoardVerify(pane), hover) + gap;
    } else if b.outcome.is_some() {
        right -= small_button(ui, right, a.y, "↻", STEEL, Hit::BoardVerify(pane), hover) + gap;
    }
    if matches!(b.outcome, Some(Outcome::Failed(_))) {
        right -= small_button(ui, right, a.y, "Que lo arregle", EMBER, Hit::BoardFix(pane), hover) + gap;
    }
    // Sello: una píldora tintada con el icono y el resultado.
    let label = text;
    let room = right - a.x;
    let pill_w = (ui.s(32.0) + label.chars().count() as f32 * ui.s(7.0)).min(room);
    let pill = Area::new(a.x, a.y, pill_w, ui.s(24.0));
    ui.round(pill, color, 0.12, 7.0);
    ui.text(icon, pill.x + ui.s(8.0), ty, ui.s(14.0), 11.5, color, true, Align::Left);
    ui.text(label, pill.x + ui.s(24.0), ty, pill.w - ui.s(30.0), 11.0, color, true, Align::Left);
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(text: &str, fg: Rgb) -> Vec<CellView> {
        text.chars()
            .map(|c| CellView {
                c,
                fg,
                bg: [0, 0, 0],
                bold: false,
                italic: false,
                underline: false,
                strikeout: false,
                wide: false,
                spacer: false,
            })
            .collect()
    }

    #[test]
    fn thumbnail_runs() {
        let mut row = cells("ab  cd", [1, 1, 1]);
        row[4].fg = [2, 2, 2];
        row[5].fg = [2, 2, 2];
        row[1].bg = [9, 9, 9];
        let runs = row_runs(&row, [0, 0, 0]);
        let bg = Run { start: 1, len: 1, color: [9, 9, 9], bg: true };
        let a = Run { start: 0, len: 2, color: [1, 1, 1], bg: false };
        let b = Run { start: 4, len: 2, color: [2, 2, 2], bg: false };
        assert_eq!(runs, [bg, a, b]);
        assert!(row_runs(&cells("    ", [1, 1, 1]), [0, 0, 0]).is_empty());
    }

    #[test]
    fn parses_permission_menu() {
        let screen = "⏺ Update(src/main.rs)\n╭────────────────────────╮\n│ Edit file              │\n│ Do you want to make this edit to main.rs? │\n│ ❯ 1. Yes               │\n│   2. Yes, allow all edits during this session (shift+tab) │\n│   3. No, and tell Claude what to do differently (esc) │\n╰────────────────────────╯\n";
        let c = parse_choice(screen).unwrap();
        assert_eq!(c.question, "Do you want to make this edit to main.rs?");
        assert_eq!(translate_question(&c.question), "¿Aplico este cambio en main.rs?");
        assert_eq!(c.options.len(), 3);
        assert_eq!(c.current, 0);
        let short: Vec<String> = c.options.iter().map(|o| short_option(o)).collect();
        assert_eq!(short, ["Sí", "Sí, siempre", "No"]);
        assert_eq!(answer_bytes(&c, 0), b"\r");
        assert_eq!(answer_bytes(&c, 2), b"3");
        // Sin marco y con la ayuda debajo.
        let plain = "Do you want to proceed?\n  1. Yes\n❯ 2. No\n\nEsc to cancel";
        let c = parse_choice(plain).unwrap();
        assert_eq!((c.current, c.options.len()), (1, 2));
        // Una lista numerada normal (sin ❯) o que no está al final no es un menú.
        assert!(parse_choice("Pasos:\n1. Compilar\n2. Probar\n").is_none());
        assert!(parse_choice("❯ 1. Yes\n  2. No\nuno\ndos\ntres\ncuatro").is_none());
    }

    #[test]
    fn preview_skips_assistant_chrome() {
        assert!(is_prompt("curro@mac forge % ls"));
        assert!(!is_prompt("Editando src/ui.rs"));
        let text = "curro@mac demo % echo hola\nHola\n\n⏺ Editando ui.rs\n  ⎿  Updated 3 lines\n╭──────╮\n│ > _  │\n╰──────╯\n  ? for shortcuts\n";
        assert_eq!(preview_lines(text, 3), ["Hola", "Editando ui.rs", "Updated 3 lines"]);
        assert_eq!(preview_lines(text, 1), ["Updated 3 lines"]);
    }
}

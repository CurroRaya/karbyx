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
use crate::panes::PaneId;
use crate::ui::{self, Align, Area, Hit, Rgb, Ui};
use crate::usage::UsageState;
use crate::worktree;

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
}

/// Una terminal normal (sin agente), en la tira de abajo.
pub struct ShellChip {
    pub pane: PaneId,
    pub label: String,
}

/// Todo lo que la mesa necesita para pintarse.
pub struct BoardView {
    pub columns: [Vec<Card>; 3],
    pub done: Vec<DoneItem>,
    pub shells: Vec<ShellChip>,
    pub input: String,
    pub project: String,
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
                let col = match state {
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
                    preview: preview_lines(&p.session.tail_text(30), 3),
                    stats: self.board.stats.get(&id).copied(),
                    elapsed: a.map(|a| ago(a.since)).unwrap_or_default(),
                    tokens,
                    state,
                    ai,
                    unseen: a.is_some_and(|a| a.unseen),
                });
            }
        }
        let project = self
            .board_project()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "~".into());
        let today_start = Local::now().date_naive();
        BoardView {
            columns,
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
            selected: self.board.sel,
            today: usage.views.iter().map(|v| v.today).sum(),
            notice: self.board.notice.clone(),
        }
    }

    /// Paneles de las tarjetas en orden (columna a columna), para ↑↓.
    fn board_order(&self) -> Vec<PaneId> {
        let v = self.board_view(&UsageState::default());
        v.columns.iter().flatten().map(|c| c.pane).collect()
    }

    pub(super) fn show_board(&mut self, on: bool) {
        self.board_visible = on;
        if on {
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
            .filter_map(|(id, p)| p.worktree.as_ref().map(|w| (*id, w.path.clone(), w.base.clone())))
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
        if self.board_visible && self.board.stats_at.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
            self.refresh_board_stats();
        }
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
        let provider = self.selected_provider().unwrap_or(Provider::Claude);
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
                    self.go_to_pane(p);
                }
            }
            Key::Named(NamedKey::Tab) => {
                let projects = self.board_projects();
                if !projects.is_empty() {
                    let cur = self.board_project().and_then(|c| projects.iter().position(|p| *p == c));
                    let n = projects.len();
                    let next = match cur {
                        Some(i) if self.mods.shift_key() => (i + n - 1) % n,
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
                if let Some(text) = event.text.as_deref().filter(|t| !t.chars().any(char::is_control)) {
                    self.board.input.push_str(text);
                    self.board.notice = None;
                }
            }
        }
        self.redraw();
        true
    }

    pub(super) fn board_click(&mut self, hit: Hit) {
        match hit {
            Hit::BoardCard(p) | Hit::BoardShell(p) => self.go_to_pane(p),
            Hit::BoardReview(p) => {
                self.go_to_pane(p);
                self.toggle_review();
            }
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

const EMBER: Rgb = [0xf0, 0x8a, 0x4b];
const CARD_BG: Rgb = [0x1b, 0x1e, 0x25];
const PREVIEW_BG: Rgb = [0x11, 0x13, 0x17];

fn column_color(i: usize) -> Rgb {
    match i {
        1 => ui::ORANGE,
        2 => ui::GREEN,
        _ => ui::DIM,
    }
}

/// Pinta la mesa a pantalla completa.
pub fn draw_board(ui: &mut Ui, screen: Area, v: &BoardView, tick: u64, hover: Option<Hit>) {
    ui.fill(screen, ui::PANEL_BG);
    let pad = ui.s(28.0);
    let x = screen.x + pad;
    let w = screen.w - 2.0 * pad;
    let mut y = screen.y + ui.s(22.0);

    // Cabecera: marca, barra de tarea y consumo de hoy.
    ui.text("◆ forge", x, y + ui.s(10.0), ui.s(110.0), 16.0, EMBER, true, Align::Left);
    let right_w = ui.s(170.0);
    ui.text(
        format!("hoy {} tok", ui::tok(v.today)),
        x + w - right_w,
        y + ui.s(13.0),
        right_w,
        11.5,
        ui::DIM,
        false,
        Align::Right,
    );
    let bar = Area::new(x + ui.s(120.0), y, w - ui.s(120.0) - right_w - ui.s(16.0), ui.s(44.0));
    let focused = !v.input.is_empty();
    ui.fill(
        Area::new(bar.x - 1.0, bar.y - 1.0, bar.w + 2.0, bar.h + 2.0),
        if focused { EMBER } else { ui::BORDER },
    );
    ui.fill(bar, CARD_BG);
    ui.text("✦", bar.x + ui.s(14.0), bar.y + ui.s(12.0), ui.s(18.0), 14.0, EMBER, false, Align::Left);
    // Selector de proyecto y botón de lanzar, a la derecha de la barra.
    let launch = Area::new(bar.right() - ui.s(118.0), bar.y + ui.s(8.0), ui.s(108.0), bar.h - ui.s(16.0));
    let lbg = if hover == Some(Hit::BoardLaunch) { ui::ACTIVE_BG } else { ui::HOVER_BG };
    ui.fill(launch, lbg);
    ui.text(
        "↵ lanzar",
        launch.x,
        launch.y + ui.s(5.0),
        launch.w,
        12.0,
        if focused { EMBER } else { ui::DIM },
        true,
        Align::Center,
    );
    ui.hit(launch, Hit::BoardLaunch);
    let proj = Area::new(launch.x - ui.s(160.0), launch.y, ui.s(152.0), launch.h);
    let pbg = if hover == Some(Hit::BoardProject) { ui::ACTIVE_BG } else { ui::HOVER_BG };
    ui.fill(proj, pbg);
    ui.text(
        format!("{} ⇥", v.project),
        proj.x + ui.s(8.0),
        proj.y + ui.s(5.0),
        proj.w - ui.s(16.0),
        12.0,
        ui::TEXT,
        false,
        Align::Center,
    );
    ui.hit(proj, Hit::BoardProject);
    let tx = bar.x + ui.s(40.0);
    let tw = proj.x - tx - ui.s(10.0);
    if v.input.is_empty() {
        ui.text(
            "¿Qué hay que hacer?  p. ej. «arregla el test de login que falla»",
            tx,
            bar.y + ui.s(12.0),
            tw,
            14.0,
            ui::DIM,
            false,
            Align::Left,
        );
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
        ui.text(format!("{shown}{caret}"), tx, bar.y + ui.s(12.0), tw, 14.0, ui::TEXT, false, Align::Left);
    }
    y += bar.h + ui.s(10.0);
    let (hint, hcolor) = match &v.notice {
        Some((m, err)) => (m.clone(), if *err { ui::ORANGE } else { ui::GREEN }),
        None => (
            "Escribe una tarea y pulsa ↵: forge lanza un agente en su propio worktree. ⇥ cambia de proyecto · ↑↓ elegir agente · ⌘E terminal".into(),
            ui::DIM,
        ),
    };
    ui.text(hint, bar.x, y, bar.w, 11.0, hcolor, false, Align::Left);
    y += ui.s(34.0);

    // Columnas.
    let shells_h = if v.shells.is_empty() { 0.0 } else { ui.s(58.0) };
    let bottom = screen.bottom() - pad - shells_h;
    let gap = ui.s(14.0);
    let cw = (w - 3.0 * gap) / 4.0;
    let empty = v.columns.iter().all(Vec::is_empty);
    for (i, title) in COLUMNS.iter().enumerate() {
        let cx = x + i as f32 * (cw + gap);
        let count = if i < 3 { v.columns[i].len() } else { v.done.len() };
        let color = column_color(i);
        ui.text(format!("{}  {count}", title.to_uppercase()), cx, y, cw, 10.5, color, true, Align::Left);
        ui.fill(Area::new(cx, y + ui.s(20.0), cw, 1.0), ui::BORDER);
        let mut cy = y + ui.s(30.0);
        if count == 0 && !empty {
            let msg =
                ["Ningún agente trabajando", "Nadie te espera", "Nada que revisar", "Aún nada fusionado hoy"]
                    [i];
            ui.text(msg, cx, cy + ui.s(4.0), cw, 11.5, [0x55, 0x5a, 0x66], false, Align::Left);
        }
        if i == 3 {
            for d in &v.done {
                if cy + ui.s(50.0) > bottom {
                    break;
                }
                let card = Area::new(cx, cy, cw, ui.s(46.0));
                ui.fill(Area::new(card.x - 1.0, card.y - 1.0, card.w + 2.0, card.h + 2.0), ui::BORDER);
                ui.fill(card, ui::PANEL_BG);
                let (mark, mc) = if d.merged { ("✓", ui::GREEN) } else { ("✕", ui::DIM) };
                ui.text(
                    mark,
                    card.x + ui.s(10.0),
                    card.y + ui.s(6.0),
                    ui.s(14.0),
                    12.0,
                    mc,
                    true,
                    Align::Left,
                );
                ui.text(
                    &d.title,
                    card.x + ui.s(28.0),
                    card.y + ui.s(6.0),
                    card.w - ui.s(36.0),
                    12.0,
                    ui::TEXT,
                    false,
                    Align::Left,
                );
                let what = if d.merged { "fusionado" } else { "descartado" };
                ui.text(
                    format!("{} · {what} · {}", d.project, d.at.format("%H:%M")),
                    card.x + ui.s(28.0),
                    card.y + ui.s(25.0),
                    card.w - ui.s(36.0),
                    10.5,
                    ui::DIM,
                    false,
                    Align::Left,
                );
                cy += card.h + ui.s(8.0);
            }
            continue;
        }
        let cards = &v.columns[i];
        let card_h = ui.s(150.0);
        let fit = (((bottom - cy) / (card_h + ui.s(10.0))).floor() as usize).max(1);
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
            cy += card_h + ui.s(10.0);
        }
    }
    if empty {
        let my = y + ui.s(120.0);
        ui.text("Tu mesa está vacía", x, my, w, 18.0, ui::TEXT, true, Align::Center);
        ui.text(
            "Escribe arriba lo que hay que hacer y pulsa ↵. Cada agente trabaja en su propia copia del proyecto; tú revisas y fusionas.",
            x,
            my + ui.s(30.0),
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
        ui.text("TERMINALES", x, sy + ui.s(8.0), ui.s(100.0), 10.5, ui::DIM, true, Align::Left);
        let mut chip_x = x + ui.s(100.0);
        let chip_w = ui.s(230.0);
        for s in &v.shells {
            if chip_x + chip_w > x + w {
                break;
            }
            let chip = Area::new(chip_x, sy + ui.s(2.0), chip_w, ui.s(28.0));
            let bg = if hover == Some(Hit::BoardShell(s.pane)) { ui::ACTIVE_BG } else { CARD_BG };
            ui.fill(chip, bg);
            ui.text(
                format!("❯ {}", s.label),
                chip.x + ui.s(10.0),
                chip.y + ui.s(6.0),
                chip.w - ui.s(20.0),
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

fn draw_card(ui: &mut Ui, a: Area, c: &Card, col: usize, selected: bool, tick: u64, hover: Option<Hit>) {
    let border = if selected {
        EMBER
    } else if col == 1 {
        ui::ORANGE
    } else if hover == Some(Hit::BoardCard(c.pane)) {
        [0x3a, 0x3f, 0x4b]
    } else {
        ui::BORDER
    };
    let bw = if selected || col == 1 { ui.s(2.0) } else { 1.0 };
    ui.fill(Area::new(a.x - bw, a.y - bw, a.w + 2.0 * bw, a.h + 2.0 * bw), border);
    ui.fill(a, CARD_BG);
    let px = ui.s(12.0);
    let iw = a.w - 2.0 * px;
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

    // Lo que hace ahora y las últimas líneas de su terminal.
    let dcolor = if col == 1 { ui::ORANGE } else { ui::TEXT };
    ui.text(&c.detail, a.x + px, a.y + ui.s(50.0), iw, 11.5, dcolor, false, Align::Left);
    let pv = Area::new(a.x + px, a.y + ui.s(70.0), iw, ui.s(48.0));
    ui.fill(pv, PREVIEW_BG);
    for (k, l) in c.preview.iter().enumerate() {
        ui.text(
            l,
            pv.x + ui.s(8.0),
            pv.y + ui.s(4.0) + k as f32 * ui.s(14.0),
            pv.w - ui.s(16.0),
            10.0,
            ui::DIM,
            false,
            Align::Left,
        );
        if let Some(lab) = ui.labels.last_mut() {
            lab.font = ui::Font::Mono;
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
    ui.text(meta.join(" · "), fx, fy, a.right() - fx - px, 11.0, ui::DIM, false, Align::Left);
    if c.branch.is_some() && c.stats.is_some_and(|s| s.2 > 0) && col != 0 {
        let b = Area::new(a.right() - px - ui.s(84.0), fy - ui.s(4.0), ui.s(84.0), ui.s(22.0));
        let bg = if hover == Some(Hit::BoardReview(c.pane)) { ui::ACTIVE_BG } else { ui::HOVER_BG };
        ui.fill(b, bg);
        ui.text("Revisar ⌘R", b.x, b.y + ui.s(4.0), b.w, 11.0, ui::GREEN, true, Align::Center);
        ui.hit(b, Hit::BoardReview(c.pane));
    }
    if c.unseen {
        ui.fill(Area::new(a.right() - ui.s(14.0), a.y + ui.s(14.0), ui.s(6.0), ui.s(6.0)), ui::ORANGE);
    }
    ui.hit(a, Hit::BoardCard(c.pane));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_skips_assistant_chrome() {
        assert!(is_prompt("curro@mac forge % ls"));
        assert!(!is_prompt("Editando src/ui.rs"));
        let text = "curro@mac demo % echo hola\nHola\n\n⏺ Editando ui.rs\n  ⎿  Updated 3 lines\n╭──────╮\n│ > _  │\n╰──────╯\n  ? for shortcuts\n";
        assert_eq!(preview_lines(text, 3), ["Hola", "Editando ui.rs", "Updated 3 lines"]);
        assert_eq!(preview_lines(text, 1), ["Updated 3 lines"]);
    }
}

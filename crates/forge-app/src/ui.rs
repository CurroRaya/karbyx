// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Interfaz alrededor de la terminal: barra lateral izquierda de sesiones y
//! panel derecho de consumo de Claude.
//!
//! Este módulo no toca la GPU: produce rectángulos, etiquetas de texto y zonas
//! clicables. `main.rs` se encarga de pintarlos.

use chrono::{DateTime, Duration, Local, Utc};

use crate::rects::{Rect, linear};
use forge_ai::Provider;

use crate::agents::AgentState;

use crate::usage::{DAYS, LimitView, UsageState, UsageView};
use crate::worktree::{FileDiff, LineKind};

pub type Rgb = [u8; 3];

// Paleta «forja»: carbón cálido, brasa y hueso.
pub const PANEL_BG: Rgb = [0x15, 0x13, 0x11];
pub const BORDER: Rgb = [0x30, 0x2a, 0x25];
pub const TEXT: Rgb = [0xec, 0xe4, 0xda];
pub const DIM: Rgb = [0x94, 0x89, 0x7e];
/// Brasa: el color de la marca.
pub const EMBER: Rgb = [0xf0, 0x8a, 0x4b];
pub const ACCENT: Rgb = EMBER;
/// Metal al rojo blanco: lo que necesita tu atención.
pub const HOT: Rgb = [0xff, 0xc4, 0x5a];
/// Acero frío: lo terminado.
pub const STEEL: Rgb = [0x8d, 0x9c, 0xab];
pub const ACTIVE_BG: Rgb = [0x2c, 0x24, 0x1e];
pub const HOVER_BG: Rgb = [0x21, 0x1d, 0x19];
const TRACK: Rgb = [0x30, 0x2a, 0x25];
const BAR_IDLE: Rgb = [0x57, 0x4d, 0x44];
pub const GREEN: Rgb = [0x9e, 0xc4, 0x7a];
const YELLOW: Rgb = [0xe9, 0xc0, 0x6e];
pub const RED: Rgb = [0xe8, 0x6a, 0x5c];

const LEFT_W: f32 = 232.0;
const RIGHT_W: f32 = 284.0;
const MIN_TERM_W: f32 = 360.0;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Area {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn intersect(&self, o: Area) -> Area {
        let (x, y) = (self.x.max(o.x), self.y.max(o.y));
        let (r, b) = (self.right().min(o.right()), self.bottom().min(o.bottom()));
        Area::new(x, y, (r - x).max(0.0), (b - y).max(0.0))
    }
    /// Encogida `d` por cada lado.
    pub fn inset(&self, d: f32) -> Area {
        Area::new(self.x + d, self.y + d, (self.w - 2.0 * d).max(0.0), (self.h - 2.0 * d).max(0.0))
    }
}

/// Reparto de la ventana entre paneles.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub left: Option<Area>,
    pub term: Area,
    pub right: Option<Area>,
}

pub fn layout(w: f32, h: f32, scale: f32, show_left: bool, show_right: bool) -> Layout {
    let (lw, rw, min) = (LEFT_W * scale, RIGHT_W * scale, MIN_TERM_W * scale);
    // Si no cabe todo, se oculta primero el panel derecho y luego el izquierdo.
    let show_right = show_right && w - rw - if show_left { lw } else { 0.0 } >= min;
    let show_left = show_left && w - lw - if show_right { rw } else { 0.0 } >= min;
    let left = show_left.then(|| Area::new(0.0, 0.0, lw, h));
    let right = show_right.then(|| Area::new(w - rw, 0.0, rw, h));
    let x = left.map_or(0.0, |a| a.w);
    let term_w = w - x - right.map_or(0.0, |a| a.w);
    Layout { left, term: Area::new(x, 0.0, term_w, h), right }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Font {
    Sans,
    Mono,
}

/// Un texto a pintar dentro de la caja `area` (que también lo recorta).
#[derive(Debug, Clone)]
pub struct Label {
    pub text: String,
    pub area: Area,
    pub px: f32,
    pub color: Rgb,
    pub bold: bool,
    pub align: Align,
    pub font: Font,
    /// Recorte adicional (zonas con scroll).
    pub clip: Option<Area>,
}

/// Zonas clicables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Tab(usize),
    CloseTab(usize),
    NewTab,
    /// Nueva sesión del asistente seleccionado.
    NewAi,
    /// Pestaña de proveedor en la cabecera del panel derecho.
    Provider(usize),
    /// Preguntar a la IA por un bloque fallido: (panel, bloque).
    AskAi(u64, u64),
    /// Abrir la bandeja de agentes.
    OpenInbox,
    /// Volver a la mesa.
    OpenBoard,
    /// Fila de la bandeja de agentes.
    InboxItem(usize),
    /// Fila de agente en el panel de mando.
    DashAgent(usize),
    /// Archivo en el panel de revisión.
    ReviewFile(usize),
    ReviewMerge,
    ReviewDiscard,
    /// El propio panel de revisión (un clic dentro no lo cierra).
    ReviewBox,
    /// Mesa: tarjeta de un agente, terminal normal y botón de revisar (panel).
    BoardCard(u64),
    BoardShell(u64),
    BoardReview(u64),
    /// Mesa: responder a la pregunta de un agente (panel, opción desde 0).
    BoardAnswer(u64, u8),
    /// Mesa: verificar (otra vez) los cambios de un agente y devolverle el fallo.
    BoardVerify(u64),
    BoardFix(u64),
    /// Mesa: activar la verificación automática del proyecto.
    BoardVerifyEnable(u64),
    /// Mesa: conversación del chat y respuesta a su permiso (chat, acción, sí).
    BoardChat(u64),
    BoardChatAnswer(u64, u64, bool),
    /// Mesa: selector de proyecto y botón de lanzar.
    BoardProject,
    /// Mesa: quién hará la tarea.
    BoardWorker,
    BoardLaunch,
    /// Ir al chat o a las terminales.
    OpenChat,
    OpenTerminal,
    Chat(crate::chat::ChatHit),
}

/// Color de cada asistente.
pub fn accent(p: Provider) -> Rgb {
    match p {
        Provider::Claude => ACCENT,
        Provider::Codex => [0x56, 0xb6, 0xc2],
        Provider::Gemini => [0x61, 0xaf, 0xef],
    }
}

#[derive(Default)]
pub struct Ui {
    pub rects: Vec<Rect>,
    pub labels: Vec<Label>,
    pub hits: Vec<(Area, Hit)>,
    /// Recorte de lo que se añada a partir de ahora (texto y clics).
    pub clip: Option<Area>,
    scale: f32,
}

impl Ui {
    pub fn new(scale: f32) -> Self {
        Self { scale, ..Default::default() }
    }

    pub fn s(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }

    /// Rectángulo semitransparente.
    pub fn tint(&mut self, a: Area, color: Rgb, alpha: f32) {
        self.rects.push(Rect {
            pos: [a.x, a.y],
            size: [a.w, a.h],
            color: linear(color, alpha),
            ..Default::default()
        });
    }

    pub fn fill(&mut self, a: Area, color: Rgb) {
        self.rects.push(Rect {
            pos: [a.x, a.y],
            size: [a.w, a.h],
            color: linear(color, 1.0),
            ..Default::default()
        });
    }

    /// Rectángulo de esquinas redondeadas.
    pub fn round(&mut self, a: Area, color: Rgb, alpha: f32, radius: f32) {
        let shape = [self.s(radius), 0.0, 0.0, 0.0];
        self.rects.push(Rect {
            pos: [a.x, a.y],
            size: [a.w, a.h],
            color: linear(color, alpha),
            shape,
            ..Default::default()
        });
    }

    /// Degradado vertical de `top` a `bottom`, con esquinas redondeadas.
    pub fn gradient(&mut self, a: Area, top: (Rgb, f32), bottom: (Rgb, f32), radius: f32) {
        self.rects.push(Rect {
            pos: [a.x, a.y],
            size: [a.w, a.h],
            color: linear(top.0, top.1),
            color2: linear(bottom.0, bottom.1),
            shape: [self.s(radius), 0.0, 1.0, 0.0],
        });
    }

    /// Resplandor difuso alrededor de `a` (se pinta antes que la caja).
    pub fn glow(&mut self, a: Area, color: Rgb, alpha: f32, radius: f32, spread: f32) {
        let g = self.s(spread);
        self.rects.push(Rect {
            pos: [a.x - g, a.y - g],
            size: [a.w + 2.0 * g, a.h + 2.0 * g],
            color: linear(color, alpha),
            shape: [self.s(radius) + g, g, 0.0, 0.0],
            ..Default::default()
        });
    }

    /// Caja redondeada con borde de 1 px (o `bw`).
    pub fn panel(&mut self, a: Area, bg: Rgb, border: Rgb, radius: f32, bw: f32) {
        let r = self.s(radius);
        let shape = [r, 0.0, 0.0, 0.0];
        self.rects.push(Rect {
            pos: [a.x - bw, a.y - bw],
            size: [a.w + 2.0 * bw, a.h + 2.0 * bw],
            color: linear(border, 1.0),
            shape: [r + bw, 0.0, 0.0, 0.0],
            ..Default::default()
        });
        self.rects.push(Rect {
            pos: [a.x, a.y],
            size: [a.w, a.h],
            color: linear(bg, 1.0),
            shape,
            ..Default::default()
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &mut self,
        text: impl Into<String>,
        x: f32,
        y: f32,
        w: f32,
        pt: f32,
        color: Rgb,
        bold: bool,
        align: Align,
    ) {
        let px = pt * self.scale;
        self.labels.push(Label {
            text: text.into(),
            area: Area::new(x, y, w, (px * 1.5).ceil()),
            px,
            color,
            bold,
            align,
            font: Font::Sans,
            clip: self.clip,
        });
    }

    pub fn hit(&mut self, a: Area, h: Hit) {
        let a = self.clip.map_or(a, |c| c.intersect(a));
        self.hits.push((a, h));
    }

    fn bar(&mut self, x: f32, y: f32, w: f32, pct: f64, color: Rgb) {
        let h = self.s(6.0);
        self.fill(Area::new(x, y, w, h), TRACK);
        let fw = (w * (pct.clamp(0.0, 100.0) as f32 / 100.0)).round();
        if fw > 0.0 {
            self.fill(Area::new(x, y, fw, h), color);
        }
    }

    fn section(&mut self, title: &str, right: &str, x: f32, y: f32, w: f32) {
        self.text(title, x, y, w, 10.5, DIM, true, Align::Left);
        if !right.is_empty() {
            self.text(right, x, y, w, 10.5, DIM, false, Align::Right);
        }
    }

    fn kv(&mut self, k: &str, v: &str, x: f32, y: f32, w: f32) {
        self.text(k, x, y, w, 12.5, DIM, false, Align::Left);
        self.text(v, x, y, w, 12.5, TEXT, false, Align::Right);
    }
}

/// La primera zona que contiene el punto (las más específicas van antes).
pub fn hit_at(hits: &[(Area, Hit)], x: f32, y: f32) -> Option<Hit> {
    hits.iter().find(|(a, _)| a.contains(x, y)).map(|(_, h)| *h)
}

// ---------- formato ----------

pub fn tok(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.2}B", n as f64 / 1e9),
        n if n >= 10_000_000 => format!("{:.0}M", n as f64 / 1e6),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.0}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

fn dur(d: Duration) -> String {
    let m = d.num_minutes().max(0);
    if m >= 60 { format!("{}h {:02}m", m / 60, m % 60) } else { format!("{m} min") }
}

fn hm(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%H:%M").to_string()
}

/// "lun 16": día de la semana en español (chrono solo trae nombres en inglés).
fn dia(t: DateTime<Utc>) -> String {
    use chrono::Datelike;
    const DIAS: [&str; 7] = ["lun", "mar", "mié", "jue", "vie", "sáb", "dom"];
    let l = t.with_timezone(&Local);
    format!("{} {}", DIAS[l.weekday().num_days_from_monday() as usize], l.day())
}

fn limit_color(p: f64) -> Rgb {
    match p {
        p if p >= 90.0 => RED,
        p if p >= 75.0 => YELLOW,
        _ => GREEN,
    }
}

// ---------- barra izquierda: sesiones ----------

pub const ORANGE: Rgb = HOT;

/// Icono y color del estado de un agente (`tick` anima el de trabajando).
pub fn state_icon(state: Option<AgentState>, ai: Option<Provider>, tick: u64) -> (&'static str, Rgb) {
    const SPIN: [&str; 4] = ["◐", "◓", "◑", "◒"];
    match (state, ai) {
        (Some(AgentState::Working), _) => (SPIN[(tick % 4) as usize], EMBER),
        (Some(AgentState::Waiting), _) => ("●", ORANGE),
        (Some(AgentState::Done), _) => ("✓", GREEN),
        (None, Some(p)) => ("◆", accent(p)),
        (None, None) => ("❯", DIM),
    }
}

/// Una pestaña tal como se ve en la barra lateral (ya en orden de pantalla).
pub struct TabView {
    /// Índice real de la pestaña.
    pub index: usize,
    /// Número de ⌘N (1-9) en orden de pantalla.
    pub shortcut: Option<usize>,
    pub title: String,
    pub subtitle: String,
    pub project: String,
    pub ai: Option<Provider>,
    pub state: Option<AgentState>,
    pub unseen: bool,
    pub elapsed: Option<String>,
    pub active: bool,
}

/// `ai`: asistente para el botón de nueva sesión. `attention`: agentes que
/// te esperan. `tick`: animación.
pub fn draw_left(
    ui: &mut Ui,
    area: Area,
    tabs: &[TabView],
    ai: Option<Provider>,
    attention: usize,
    tick: u64,
    hover: Option<Hit>,
) {
    ui.fill(area, PANEL_BG);
    ui.fill(Area::new(area.right() - 1.0, area.y, 1.0, area.h), BORDER);

    let pad = ui.s(16.0);
    let mut y = area.y + ui.s(14.0);
    ui.gradient(Area::new(area.x, area.y, area.w - 1.0, ui.s(120.0)), (EMBER, 0.04), (EMBER, 0.0), 0.0);

    // Marca y acceso a la mesa.
    let logo = Area::new(area.x + pad, y + ui.s(2.0), ui.s(20.0), ui.s(20.0));
    ui.glow(logo, EMBER, 0.3, 5.0, 8.0);
    ui.gradient(logo, (HOT, 1.0), ([0xd2, 0x5f, 0x2c], 1.0), 5.0);
    ui.text("❯", logo.x, logo.y + ui.s(2.0), logo.w, 11.0, [0x1c, 0x12, 0x0b], true, Align::Center);
    ui.text("forge", logo.right() + ui.s(8.0), y + ui.s(2.0), ui.s(70.0), 14.0, TEXT, true, Align::Left);
    y += ui.s(36.0);
    let nav = Area::new(area.x + ui.s(12.0), y, area.w - ui.s(24.0), ui.s(32.0));
    crate::chat::draw::draw_nav(ui, nav, 2, hover);
    y += ui.s(46.0);
    ui.text("SESIONES", area.x + pad, y, area.w - 2.0 * pad, 10.5, DIM, true, Align::Left);

    // Aviso de agentes que esperan (abre la bandeja de atención).
    if attention > 0 {
        let label =
            if attention == 1 { "1 te espera".to_string() } else { format!("{attention} te esperan") };
        let w = ui.s(96.0);
        let chip = Area::new(area.right() - pad - ui.s(34.0) - w, y - ui.s(4.0), w, ui.s(22.0));
        ui.round(chip, HOT, if hover == Some(Hit::OpenInbox) { 0.24 } else { 0.14 }, 11.0);
        ui.text(format!("● {label}"), chip.x, chip.y + ui.s(4.0), chip.w, 10.5, ORANGE, true, Align::Center);
        ui.hit(chip, Hit::OpenInbox);
    }

    // Botón "+" (nueva terminal).
    let b = ui.s(24.0);
    let plus = Area::new(area.right() - pad - b + ui.s(4.0), y - ui.s(4.0), b, b);
    if hover == Some(Hit::NewTab) {
        ui.round(plus, ACTIVE_BG, 1.0, 6.0);
    }
    ui.text("+", plus.x, plus.y + ui.s(1.0), plus.w, 15.0, TEXT, false, Align::Center);
    ui.hit(plus, Hit::NewTab);
    y += ui.s(30.0);

    let item_h = ui.s(48.0);
    let ix = area.x + ui.s(8.0);
    let iw = area.w - ui.s(16.0);
    let bottom_limit = area.bottom() - ui.s(92.0);
    let mut last_project: Option<&str> = None;
    for t in tabs {
        if y + item_h > bottom_limit {
            break;
        }
        // Cabecera de proyecto al cambiar de grupo.
        if last_project != Some(t.project.as_str()) {
            if last_project.is_some() {
                y += ui.s(6.0);
            }
            ui.text(&t.project, area.x + pad, y, area.w - 2.0 * pad, 10.0, DIM, true, Align::Left);
            y += ui.s(20.0);
            last_project = Some(t.project.as_str());
        }
        let i = t.index;
        let item = Area::new(ix, y, iw, item_h);
        let hovered = matches!(hover, Some(Hit::Tab(j) | Hit::CloseTab(j)) if j == i);
        if t.active {
            ui.round(item, ACTIVE_BG, 1.0, 8.0);
            let strip = Area::new(ix + ui.s(3.0), y + ui.s(12.0), ui.s(3.0), item_h - ui.s(24.0));
            ui.glow(strip, EMBER, 0.4, 2.0, 4.0);
            ui.round(strip, EMBER, 1.0, 1.5);
        } else if hovered {
            ui.round(item, HOVER_BG, 1.0, 8.0);
        }
        let (icon, icon_color) = state_icon(t.state, t.ai, tick);
        ui.text(icon, ix + ui.s(12.0), y + ui.s(8.0), ui.s(16.0), 12.0, icon_color, false, Align::Left);
        let tx = ix + ui.s(32.0);
        let tw = iw - ui.s(32.0) - ui.s(44.0);
        ui.text(&t.title, tx, y + ui.s(7.0), tw, 13.0, TEXT, t.active || t.unseen, Align::Left);
        let sub_color = match t.state {
            Some(AgentState::Waiting) => ORANGE,
            _ => DIM,
        };
        ui.text(&t.subtitle, tx, y + ui.s(26.0), tw + ui.s(10.0), 11.0, sub_color, false, Align::Left);

        // A la derecha: tiempo en el estado (agentes) o ⌘N; al pasar el ratón, cerrar.
        let close = Area::new(item.right() - ui.s(28.0), y + ui.s(13.0), ui.s(22.0), ui.s(22.0));
        if hovered {
            if hover == Some(Hit::CloseTab(i)) {
                ui.round(close, BORDER, 1.0, 6.0);
            }
            ui.text("×", close.x, close.y + ui.s(1.0), close.w, 14.0, TEXT, false, Align::Center);
            ui.hit(close, Hit::CloseTab(i));
        } else {
            let right = match (&t.elapsed, t.shortcut) {
                (Some(e), _) => Some(e.clone()),
                (None, Some(n)) => Some(format!("⌘{n}")),
                _ => None,
            };
            if let Some(r) = right {
                let rx = close.x - ui.s(20.0);
                let color = if t.unseen { ORANGE } else { DIM };
                ui.text(r, rx, y + ui.s(8.0), close.w + ui.s(20.0), 10.5, color, t.unseen, Align::Right);
            }
            if t.unseen {
                ui.round(
                    Area::new(item.right() - ui.s(12.0), y + ui.s(30.0), ui.s(6.0), ui.s(6.0)),
                    ORANGE,
                    1.0,
                    3.0,
                );
            }
        }
        ui.hit(item, Hit::Tab(i));
        y += item_h + ui.s(2.0);
    }

    // Botón inferior: nueva sesión del asistente.
    let hint = match ai {
        Some(p) => {
            let bh = ui.s(36.0);
            let btn = Area::new(area.x + ui.s(12.0), area.bottom() - ui.s(82.0), area.w - ui.s(24.0), bh);
            if hover == Some(Hit::NewAi) {
                ui.glow(btn, accent(p), 0.18, 9.0, 10.0);
            }
            ui.panel(btn, ACTIVE_BG, if hover == Some(Hit::NewAi) { accent(p) } else { BORDER }, 9.0, 1.0);
            let label = format!("◆  Nueva sesión de {}", p.name());
            ui.text(label, btn.x, btn.y + ui.s(9.0), btn.w, 12.5, accent(p), true, Align::Center);
            ui.hit(btn, Hit::NewAi);
            format!("⌘L chat · ⌘E mesa · ⌘⇧C {}", p.name())
        }
        None => "⌘L chat · ⌘E mesa · ⌘T terminal".to_string(),
    };
    ui.text(
        hint,
        area.x + pad,
        area.bottom() - ui.s(34.0),
        area.w - 2.0 * pad,
        10.0,
        DIM,
        false,
        Align::Left,
    );
}

// ---------- bandeja de atención (⌘J) ----------

pub struct InboxItem {
    pub title: String,
    pub detail: String,
    pub project: String,
    pub state: AgentState,
    pub ai: Option<Provider>,
    pub elapsed: String,
    pub unseen: bool,
}

/// Lista flotante de agentes: primero los que te esperan.
pub fn draw_inbox(
    ui: &mut Ui,
    screen: Area,
    items: &[InboxItem],
    selected: usize,
    tick: u64,
    hover: Option<Hit>,
) {
    ui.tint(screen, [0, 0, 0], 0.45);
    let w = ui.s(600.0).min(screen.w - ui.s(40.0));
    let row_h = ui.s(54.0);
    let shown = items.len().clamp(1, 8);
    let h = ui.s(58.0) + row_h * shown as f32 + ui.s(36.0);
    let bx = Area::new(screen.x + (screen.w - w) / 2.0, screen.y + screen.h * 0.18, w, h);
    ui.fill(Area::new(bx.x - 1.0, bx.y - 1.0, bx.w + 2.0, bx.h + 2.0), BORDER);
    ui.fill(bx, PANEL_BG);

    let pad = ui.s(18.0);
    let waiting = items
        .iter()
        .filter(|i| i.state != AgentState::Working && (i.unseen || i.state == AgentState::Waiting))
        .count();
    let title = match waiting {
        0 => "AGENTES".to_string(),
        1 => "AGENTES · 1 te espera".to_string(),
        n => format!("AGENTES · {n} te esperan"),
    };
    ui.text(title, bx.x + pad, bx.y + ui.s(18.0), bx.w - 2.0 * pad, 10.5, DIM, true, Align::Left);
    ui.text("⌘J", bx.x + pad, bx.y + ui.s(18.0), bx.w - 2.0 * pad, 10.5, DIM, false, Align::Right);

    let mut y = bx.y + ui.s(46.0);
    if items.is_empty() {
        ui.text(
            "No hay agentes abiertos. Abre uno con ⌘⇧C.",
            bx.x + pad,
            y + ui.s(16.0),
            bx.w - 2.0 * pad,
            12.5,
            DIM,
            false,
            Align::Left,
        );
    }
    for (i, it) in items.iter().take(8).enumerate() {
        let row = Area::new(bx.x + ui.s(8.0), y, bx.w - ui.s(16.0), row_h - ui.s(4.0));
        if i == selected {
            ui.fill(row, ACTIVE_BG);
            ui.fill(Area::new(row.x, row.y + ui.s(10.0), ui.s(3.0), row.h - ui.s(20.0)), ACCENT);
        } else if hover == Some(Hit::InboxItem(i)) {
            ui.fill(row, HOVER_BG);
        }
        let (icon, color) = state_icon(Some(it.state), it.ai, tick);
        ui.text(icon, row.x + ui.s(12.0), row.y + ui.s(8.0), ui.s(16.0), 13.0, color, false, Align::Left);
        let tx = row.x + ui.s(34.0);
        let tw = row.w - ui.s(34.0) - ui.s(90.0);
        ui.text(
            format!("{} · {}", it.project, it.title),
            tx,
            row.y + ui.s(7.0),
            tw,
            13.0,
            TEXT,
            it.unseen,
            Align::Left,
        );
        let dcolor = if it.state == AgentState::Waiting { ORANGE } else { DIM };
        ui.text(&it.detail, tx, row.y + ui.s(27.0), tw, 11.0, dcolor, false, Align::Left);
        ui.text(
            &it.elapsed,
            row.right() - ui.s(90.0),
            row.y + ui.s(9.0),
            ui.s(78.0),
            10.5,
            if it.unseen { ORANGE } else { DIM },
            it.unseen,
            Align::Right,
        );
        ui.hit(row, Hit::InboxItem(i));
        y += row_h;
    }
    ui.text(
        "↑↓ elegir  ·  ↵ ir al agente  ·  esc cerrar",
        bx.x + pad,
        bx.bottom() - ui.s(28.0),
        bx.w - 2.0 * pad,
        10.5,
        DIM,
        false,
        Align::Left,
    );
}

// ---------- panel derecho: consumo de los asistentes ----------

pub fn draw_right(
    ui: &mut Ui,
    area: Area,
    state: &UsageState,
    selected: usize,
    hover: Option<Hit>,
    now: DateTime<Utc>,
) {
    ui.fill(area, PANEL_BG);
    ui.fill(Area::new(area.x, area.y, 1.0, area.h), BORDER);

    let x = area.x + ui.s(18.0);
    let w = area.w - ui.s(36.0);
    let mut y = area.y + ui.s(16.0);

    if !state.loaded {
        ui.text("Leyendo historial…", x, y, w, 12.5, DIM, false, Align::Left);
        return;
    }
    let Some(u) = state.views.get(selected.min(state.views.len().saturating_sub(1))) else {
        ui.text("◆ ASISTENTES DE IA", x, y, w, 10.5, DIM, true, Align::Left);
        y += ui.s(34.0);
        ui.text("No se ha detectado ningún asistente.", x, y, w, 12.5, TEXT, false, Align::Left);
        y += ui.s(26.0);
        for l in [
            "Compatibles: Claude Code, Codex CLI",
            "y Gemini CLI. Elige cuáles mostrar en",
            "~/.config/forge/usage.toml",
        ] {
            ui.text(l, x, y, w, 11.5, DIM, false, Align::Left);
            y += ui.s(18.0);
        }
        return;
    };
    let acc = accent(u.provider);

    // Cabecera: una pestaña por asistente si hay varios.
    if state.views.len() > 1 {
        let gap = ui.s(6.0);
        let n = state.views.len() as f32;
        let cw = ((w - gap * (n - 1.0)) / n).floor();
        let ch = ui.s(26.0);
        for (i, v) in state.views.iter().enumerate() {
            let chip = Area::new(x + i as f32 * (cw + gap), y - ui.s(6.0), cw, ch);
            let sel = v.provider == u.provider;
            if sel {
                ui.fill(chip, ACTIVE_BG);
                ui.fill(Area::new(chip.x, chip.bottom() - ui.s(2.0), chip.w, ui.s(2.0)), accent(v.provider));
            } else if hover == Some(Hit::Provider(i)) {
                ui.fill(chip, HOVER_BG);
            }
            let color = if sel { accent(v.provider) } else { DIM };
            ui.text(v.provider.name(), chip.x, chip.y + ui.s(5.0), chip.w, 11.5, color, sel, Align::Center);
            ui.hit(chip, Hit::Provider(i));
        }
        y += ui.s(34.0);
    }

    let line = ui.s(22.0);
    ui.text(format!("◆ {}", u.provider.name().to_uppercase()), x, y, w, 10.5, acc, true, Align::Left);
    let (status, color) = match (&u.active, u.has_data) {
        (_, false) => ("sin historial", DIM),
        (Some(_), _) => ("● activo", GREEN),
        (None, _) => ("○ inactivo", DIM),
    };
    ui.text(status, x, y, w, 10.5, color, false, Align::Right);
    y += ui.s(34.0);

    // Ventana actual.
    match &u.active {
        Some(a) => {
            ui.section("VENTANA ACTUAL", &format!("{} – {}", hm(a.start), hm(a.end)), x, y, w);
            y += ui.s(20.0);
            ui.text(tok(a.tokens), x, y, w, 28.0, TEXT, true, Align::Left);
            if let Some(l) = ui.labels.last_mut() {
                l.font = Font::Mono; // cifra grande en monoespaciada
            }
            y += ui.s(42.0);
            ui.text("tokens en esta ventana", x, y, w, 11.0, DIM, false, Align::Left);
            y += ui.s(24.0);

            let (pct, color, caption) = match &a.limit {
                Some(l) => {
                    let src = if l.official { " (oficial)" } else { "" };
                    (l.percent, limit_color(l.percent), format!("{:.0}% del límite{src}", l.percent))
                }
                None => {
                    let total = (a.end - a.start).num_seconds().max(1) as f64;
                    let p = (now - a.start).num_seconds() as f64 * 100.0 / total;
                    (p, acc, format!("{p:.0}% del tiempo"))
                }
            };
            ui.bar(x, y, w, pct, color);
            y += ui.s(12.0);
            ui.text(caption, x, y, w, 11.0, DIM, false, Align::Left);
            ui.text(format!("quedan {}", dur(a.remaining)), x, y, w, 11.0, TEXT, false, Align::Right);
            y += ui.s(28.0);

            ui.kv("Reinicia", &hm(a.end), x, y, w);
            y += line;
            ui.kv("Ritmo", &format!("{} tok/min", tok(a.tokens_per_min as u64)), x, y, w);
            y += line;
            ui.kv("Proyección", &format!("≈ {} al cierre", tok(a.projected)), x, y, w);
            y += line;
            ui.kv("Peticiones", &a.requests.to_string(), x, y, w);
            y += line;
            if let Some(l) = &a.limit {
                if let Some(left) = l.left {
                    ui.kv("Disponible", &tok(left), x, y, w);
                    y += line;
                }
                if let Some(t) = l.exhausted_at {
                    ui.text(
                        format!("⚠ Al ritmo actual se agota a las {}", hm(t)),
                        x,
                        y,
                        w,
                        11.5,
                        YELLOW,
                        false,
                        Align::Left,
                    );
                    y += line;
                }
            }
            if !a.models.is_empty() {
                y += ui.s(6.0);
                for (m, t) in a.models.iter().take(3) {
                    ui.text(format!("• {m}"), x, y, w, 12.0, DIM, false, Align::Left);
                    ui.text(tok(*t), x, y, w, 12.0, DIM, false, Align::Right);
                    y += ui.s(20.0);
                }
            }
        }
        None => {
            ui.section("VENTANA ACTUAL", "", x, y, w);
            y += ui.s(22.0);
            let msg =
                if u.has_data { "Sin actividad en las últimas 5 h" } else { "Todavía no hay historial" };
            ui.text(msg, x, y, w, 12.5, DIM, false, Align::Left);
            y += line;
        }
    }

    // Hoy.
    y += ui.s(14.0);
    ui.fill(Area::new(x, y, w, 1.0), BORDER);
    y += ui.s(16.0);
    ui.section("HOY", "", x, y, w);
    y += ui.s(22.0);
    ui.kv("Tokens", &tok(u.today), x, y, w);
    y += line;
    ui.kv("Peticiones", &u.today_requests.to_string(), x, y, w);
    y += line;
    ui.kv("Caché leída", &tok(u.today_cache_read), x, y, w);
    y += line;

    // Semana.
    y += ui.s(14.0);
    ui.fill(Area::new(x, y, w, 1.0), BORDER);
    y += ui.s(16.0);
    let since = u.week_start.map(|t| format!("desde {}", dia(t))).unwrap_or_default();
    ui.section("SEMANA", &since, x, y, w);
    y += ui.s(22.0);
    ui.kv("Tokens", &tok(u.week), x, y, w);
    y += line;
    if let Some(l) = &u.week_limit {
        week_limit(ui, l, x, &mut y, w);
    }

    // Límites publicados por el propio proveedor.
    if !u.official.is_empty() {
        y += ui.s(14.0);
        ui.fill(Area::new(x, y, w, 1.0), BORDER);
        y += ui.s(16.0);
        ui.section("LÍMITES OFICIALES", "", x, y, w);
        y += ui.s(22.0);
        for o in &u.official {
            ui.text(&o.label, x, y, w, 12.0, TEXT, false, Align::Left);
            ui.text(format!("{:.0}%", o.percent), x, y, w, 12.0, TEXT, false, Align::Right);
            y += ui.s(20.0);
            ui.bar(x, y, w, o.percent, limit_color(o.percent));
            y += ui.s(10.0);
            if let Some(t) = o.resets_at {
                let when = if t - now < Duration::hours(24) { hm(t) } else { dia(t) };
                ui.text(format!("reinicia {when}"), x, y, w, 10.5, DIM, false, Align::Left);
                y += ui.s(18.0);
            }
            y += ui.s(6.0);
        }
    }

    // Últimas ventanas: mini gráfico de barras.
    if !u.recent.is_empty() {
        y += ui.s(14.0);
        ui.fill(Area::new(x, y, w, 1.0), BORDER);
        y += ui.s(16.0);
        ui.section("ÚLTIMAS VENTANAS", "", x, y, w);
        y += ui.s(24.0);
        let chart_h = ui.s(56.0);
        let n = u.recent.len() as f32;
        let gap = ui.s(6.0);
        // Barras de ancho acotado: con pocas ventanas no deben ocupar todo el panel.
        let bw = ((w - gap * (n - 1.0)) / n).floor().min(ui.s(40.0));
        let max = u.recent.iter().map(|b| b.tokens).max().unwrap_or(1).max(1) as f32;
        for (i, b) in u.recent.iter().enumerate() {
            let bx = x + i as f32 * (bw + gap);
            let bh = (chart_h * b.tokens as f32 / max).max(ui.s(2.0));
            ui.fill(Area::new(bx, y + chart_h - bh, bw, bh), if b.active { acc } else { BAR_IDLE });
            // Hora si es de hoy; si no, el día.
            let local = b.start.with_timezone(&Local);
            let fmt =
                if local.date_naive() == now.with_timezone(&Local).date_naive() { "%Hh" } else { "%d/%m" };
            let label = local.format(fmt).to_string();
            ui.text(
                label,
                bx - ui.s(4.0),
                y + chart_h + ui.s(4.0),
                bw + ui.s(8.0),
                9.5,
                DIM,
                false,
                Align::Center,
            );
        }
    }

    // Pie.
    let foot = area.bottom() - ui.s(34.0);
    if !u.has_limits {
        ui.text(
            "Sin límites definidos · ~/.config/forge/usage.toml",
            x,
            foot - ui.s(18.0),
            w,
            10.0,
            DIM,
            false,
            Align::Left,
        );
    }
    ui.text("⌘⌥B ocultar panel", x, foot, w, 10.0, DIM, false, Align::Left);
}

// ---------- panel de mando: todos los agentes de un vistazo ----------

const CARD_BG: Rgb = [0x1d, 0x1a, 0x17];

/// Un agente abierto en forge tal como se ve en el panel de mando.
pub struct DashAgent {
    pub title: String,
    pub project: String,
    pub ai: Option<Provider>,
    pub state: Option<AgentState>,
    /// Tokens que lleva su sesión (si se conoce: Claude con hooks).
    pub tokens: Option<u64>,
}

/// Porcentaje y texto de la ventana de 5 h de un asistente.
fn window_line(v: &UsageView, now: DateTime<Utc>) -> (Option<f64>, Rgb, String) {
    let acc = accent(v.provider);
    match &v.active {
        Some(a) => match &a.limit {
            Some(l) => (
                Some(l.percent),
                limit_color(l.percent),
                format!("{:.0}% · reinicia {}", l.percent, hm(a.end)),
            ),
            None => {
                let total = (a.end - a.start).num_seconds().max(1) as f64;
                let p = (now - a.start).num_seconds() as f64 * 100.0 / total;
                (Some(p), acc, format!("{} · quedan {}", tok(a.tokens), dur(a.remaining)))
            }
        },
        None => (None, DIM, "sin actividad".into()),
    }
}

/// Porcentaje y texto de la semana: límite propio, oficial o solo tokens.
fn week_line(v: &UsageView) -> (Option<f64>, String) {
    if let Some(l) = &v.week_limit {
        return (Some(l.percent), format!("{:.0}% · {}", l.percent, tok(v.week)));
    }
    if let Some(o) = v.official.iter().find(|o| o.label == "semana") {
        return (Some(o.percent), format!("{:.0}% (oficial)", o.percent));
    }
    (None, tok(v.week))
}

fn provider_card(ui: &mut Ui, card: Area, v: &UsageView, now: DateTime<Utc>) {
    let acc = accent(v.provider);
    ui.fill(card, CARD_BG);
    ui.fill(Area::new(card.x, card.y, card.w, ui.s(2.0)), acc);
    let x = card.x + ui.s(16.0);
    let w = card.w - ui.s(32.0);
    let mut y = card.y + ui.s(14.0);

    ui.text(v.provider.name(), x, y, w, 12.5, acc, true, Align::Left);
    let (status, color) = match (&v.active, v.has_data) {
        (_, false) => ("sin historial", DIM),
        (Some(_), _) => ("● activo", GREEN),
        (None, _) => ("○ inactivo", DIM),
    };
    ui.text(status, x, y, w, 10.5, color, false, Align::Right);
    y += ui.s(26.0);

    ui.text(tok(v.today), x, y, w, 24.0, TEXT, true, Align::Left);
    if let Some(l) = ui.labels.last_mut() {
        l.font = Font::Mono;
    }
    y += ui.s(36.0);
    ui.text(format!("tokens hoy · {} peticiones", v.today_requests), x, y, w, 10.5, DIM, false, Align::Left);
    y += ui.s(26.0);

    let (pct, color, caption) = window_line(v, now);
    ui.text("Ventana 5 h", x, y, w, 11.0, DIM, false, Align::Left);
    ui.text(caption, x, y, w, 11.0, TEXT, false, Align::Right);
    y += ui.s(18.0);
    ui.bar(x, y, w, pct.unwrap_or(0.0), color);
    y += ui.s(16.0);

    let (pct, caption) = week_line(v);
    ui.text("Semana", x, y, w, 11.0, DIM, false, Align::Left);
    ui.text(caption, x, y, w, 11.0, TEXT, false, Align::Right);
    y += ui.s(18.0);
    if let Some(p) = pct {
        ui.bar(x, y, w, p, limit_color(p));
    } else {
        ui.fill(Area::new(x, y + ui.s(2.0), w, 1.0), TRACK);
    }

    // Últimas 24 h: una barra por hora.
    let spark_h = ui.s(32.0);
    let sy = card.bottom() - ui.s(14.0) - spark_h;
    ui.text("últimas 24 h", x, sy - ui.s(18.0), w, 9.5, DIM, false, Align::Left);
    let max = v.hourly.iter().copied().max().unwrap_or(0);
    ui.text(
        if max > 0 { format!("máx {}/h", tok(max)) } else { String::new() },
        x,
        sy - ui.s(18.0),
        w,
        9.5,
        DIM,
        false,
        Align::Right,
    );
    let n = v.hourly.len().max(1) as f32;
    let gap = ui.s(2.0);
    let bw = ((w - gap * (n - 1.0)) / n).max(1.0);
    for (i, t) in v.hourly.iter().enumerate() {
        let bx = x + i as f32 * (bw + gap);
        let last = i + 1 == v.hourly.len();
        ui.fill(Area::new(bx, sy + spark_h - 1.0, bw, 1.0), TRACK);
        if *t > 0 && max > 0 {
            let bh = (spark_h * *t as f32 / max as f32).max(ui.s(2.0));
            ui.fill(Area::new(bx, sy + spark_h - bh, bw, bh), if last { acc } else { BAR_IDLE });
        }
    }
}

/// Histórico diario apilado por asistente.
fn daily_chart(ui: &mut Ui, x: f32, y: f32, w: f32, h: f32, views: &[UsageView], now: DateTime<Utc>) {
    let totals: Vec<u64> =
        (0..DAYS).map(|i| views.iter().map(|v| v.daily.get(i).copied().unwrap_or(0)).sum()).collect();
    let max = totals.iter().copied().max().unwrap_or(0).max(1) as f32;
    let label_h = ui.s(16.0);
    let chart_h = h - label_h;
    let gap = ui.s(5.0);
    let bw = ((w - gap * (DAYS as f32 - 1.0)) / DAYS as f32).max(2.0);
    let today = now.with_timezone(&Local).date_naive();
    for i in 0..DAYS {
        let bx = x + i as f32 * (bw + gap);
        ui.fill(Area::new(bx, y + chart_h - 1.0, bw, 1.0), TRACK);
        let mut top = y + chart_h;
        for v in views {
            let t = v.daily.get(i).copied().unwrap_or(0);
            if t == 0 {
                continue;
            }
            let sh = (chart_h * t as f32 / max).max(ui.s(2.0));
            top -= sh;
            ui.fill(Area::new(bx, top, bw, sh), accent(v.provider));
        }
        let day = today - Duration::days((DAYS - 1 - i) as i64);
        let last = i + 1 == DAYS;
        if last || (DAYS - 1 - i).is_multiple_of(2) {
            let lbl = if last { "hoy".to_string() } else { day.format("%d").to_string() };
            ui.text(
                lbl,
                bx - ui.s(8.0),
                y + chart_h + ui.s(3.0),
                bw + ui.s(16.0),
                9.5,
                if last { TEXT } else { DIM },
                false,
                Align::Center,
            );
        }
    }
}

/// Panel de mando (⌘U): consumo de todos los asistentes y agentes abiertos.
#[allow(clippy::too_many_arguments)]
pub fn draw_dashboard(
    ui: &mut Ui,
    screen: Area,
    state: &UsageState,
    agents: &[DashAgent],
    selected: Option<usize>,
    tick: u64,
    hover: Option<Hit>,
    now: DateTime<Utc>,
) {
    ui.tint(screen, [0, 0, 0], 0.55);
    let margin = ui.s(28.0);
    let w = (screen.w - 2.0 * margin).min(ui.s(1180.0));
    let h = screen.h - 2.0 * margin;
    let bx = Area::new(screen.x + (screen.w - w) / 2.0, screen.y + margin, w, h);
    ui.fill(Area::new(bx.x - 1.0, bx.y - 1.0, bx.w + 2.0, bx.h + 2.0), BORDER);
    ui.fill(bx, PANEL_BG);

    let pad = ui.s(24.0);
    let x = bx.x + pad;
    let iw = bx.w - 2.0 * pad;
    let mut y = bx.y + ui.s(20.0);

    // Cabecera.
    ui.text("PANEL DE MANDO", x, y, iw, 10.5, DIM, true, Align::Left);
    ui.text("⌘U · esc", x, y, iw, 10.5, DIM, false, Align::Right);
    y += ui.s(20.0);
    ui.text("Consumo de tus agentes", x, y, iw, 17.0, TEXT, true, Align::Left);
    let today: u64 = state.views.iter().map(|v| v.today).sum();
    let week: u64 = state.views.iter().map(|v| v.week).sum();
    if state.views.len() > 1 {
        ui.text(
            format!("hoy {} · semana {} tokens", tok(today), tok(week)),
            x,
            y + ui.s(4.0),
            iw,
            12.0,
            DIM,
            false,
            Align::Right,
        );
    }
    y += ui.s(42.0);

    if !state.loaded {
        ui.text("Leyendo historial…", x, y, iw, 12.5, DIM, false, Align::Left);
        return;
    }
    if state.views.is_empty() {
        ui.text(
            "No se ha detectado ningún asistente (Claude Code, Codex CLI, Gemini CLI).",
            x,
            y,
            iw,
            12.5,
            DIM,
            false,
            Align::Left,
        );
        y += ui.s(40.0);
    } else {
        let n = state.views.len() as f32;
        let gap = ui.s(14.0);
        let cw = ((iw - gap * (n - 1.0)) / n).floor();
        let ch = ui.s(236.0);
        for (i, v) in state.views.iter().enumerate() {
            provider_card(ui, Area::new(x + i as f32 * (cw + gap), y, cw, ch), v, now);
        }
        y += ch + ui.s(26.0);
    }

    // Parte inferior: histórico | agentes y proyectos.
    let bottom = bx.bottom() - ui.s(40.0);
    let gap = ui.s(32.0);
    let lw = ((iw - gap) * 0.5).floor();
    let rx = x + lw + gap;
    let rw = iw - lw - gap;

    if !state.views.is_empty() {
        ui.section(&format!("ÚLTIMOS {DAYS} DÍAS"), "", x, y, lw);
        // Leyenda.
        let mut lx = x + lw;
        for v in state.views.iter().rev() {
            let lw_ = ui.s(14.0 + 7.0 * v.provider.name().len() as f32);
            lx -= lw_;
            ui.text(
                format!("■ {}", v.provider.name()),
                lx,
                y,
                lw_,
                10.0,
                accent(v.provider),
                false,
                Align::Left,
            );
        }
        let ch = (bottom - y - ui.s(26.0)).min(ui.s(200.0));
        if ch > ui.s(40.0) {
            daily_chart(ui, x, y + ui.s(26.0), lw, ch, &state.views, now);
        }
    }

    // Agentes abiertos en forge.
    let mut ry = y;
    ui.section("AGENTES EN FORGE", &agents.len().to_string(), rx, ry, rw);
    ry += ui.s(24.0);
    if agents.is_empty() {
        ui.text("Ningún agente abierto · ⌘⇧C para abrir uno", rx, ry, rw, 12.0, DIM, false, Align::Left);
        ry += ui.s(30.0);
    }
    let row_h = ui.s(30.0);
    let max_rows = 6;
    for (i, a) in agents.iter().take(max_rows).enumerate() {
        if ry + row_h > bottom {
            break;
        }
        let row = Area::new(rx - ui.s(8.0), ry, rw + ui.s(16.0), row_h - ui.s(2.0));
        if selected == Some(i) {
            ui.fill(row, ACTIVE_BG);
            ui.fill(Area::new(row.x, row.y + ui.s(7.0), ui.s(3.0), row.h - ui.s(14.0)), ACCENT);
        } else if hover == Some(Hit::DashAgent(i)) {
            ui.fill(row, HOVER_BG);
        }
        let (icon, color) = state_icon(a.state, a.ai, tick);
        ui.text(icon, rx, ry + ui.s(6.0), ui.s(16.0), 12.5, color, false, Align::Left);
        let tw = rw - ui.s(22.0) - ui.s(80.0);
        ui.text(
            format!("{} · {}", a.project, a.title),
            rx + ui.s(22.0),
            ry + ui.s(6.0),
            tw,
            12.0,
            TEXT,
            false,
            Align::Left,
        );
        let t = a.tokens.map(|t| format!("{} tok", tok(t))).unwrap_or_else(|| "—".into());
        ui.text(
            t,
            rx,
            ry + ui.s(6.0),
            rw,
            11.5,
            if a.tokens.is_some() { TEXT } else { DIM },
            false,
            Align::Right,
        );
        ui.hit(row, Hit::DashAgent(i));
        ry += row_h;
    }
    if agents.len() > max_rows {
        ui.text(format!("y {} más · ⌘J", agents.len() - max_rows), rx, ry, rw, 10.5, DIM, false, Align::Left);
        ry += ui.s(18.0);
    }

    // Proyectos de la semana, sumando asistentes.
    let mut projects: Vec<(String, Vec<(Provider, u64)>)> = Vec::new();
    for v in &state.views {
        for (name, t) in &v.projects {
            match projects.iter_mut().find(|(n, _)| n == name) {
                Some((_, parts)) => parts.push((v.provider, *t)),
                None => projects.push((name.clone(), vec![(v.provider, *t)])),
            }
        }
    }
    let total = |p: &[(Provider, u64)]| p.iter().map(|e| e.1).sum::<u64>();
    projects.sort_by_key(|a| std::cmp::Reverse(total(&a.1)));
    let prow = ui.s(34.0);
    if !projects.is_empty() && ry + ui.s(24.0) + prow <= bottom {
        ry += ui.s(16.0);
        ui.section("PROYECTOS · SEMANA", "", rx, ry, rw);
        ry += ui.s(24.0);
        let max = projects.first().map(|p| total(&p.1)).unwrap_or(1).max(1) as f32;
        for (name, parts) in projects.iter().take(5) {
            if ry + prow > bottom {
                break;
            }
            ui.text(name, rx, ry, rw, 12.0, TEXT, false, Align::Left);
            ui.text(tok(total(parts)), rx, ry, rw, 12.0, DIM, false, Align::Right);
            let by = ry + ui.s(19.0);
            ui.fill(Area::new(rx, by, rw, ui.s(5.0)), TRACK);
            let mut sx = rx;
            for (p, t) in parts {
                let sw = (rw * *t as f32 / max).round();
                if sw > 0.0 {
                    ui.fill(Area::new(sx, by, sw, ui.s(5.0)), accent(*p));
                    sx += sw;
                }
            }
            ry += prow;
        }
    }

    ui.text(
        "↑↓ elegir agente  ·  ↵ ir  ·  esc cerrar  ·  tokens frescos (sin lecturas de caché)",
        x,
        bx.bottom() - ui.s(28.0),
        iw,
        10.0,
        DIM,
        false,
        Align::Left,
    );
}

// ---------- revisar cambios (⌘R) ----------

/// Lo que necesita el panel de revisión para pintarse.
pub struct ReviewView<'a> {
    pub project: &'a str,
    /// Rama del agente (`forge/agente-1`) si trabaja en un worktree.
    pub branch: Option<&'a str>,
    pub target: &'a str,
    pub files: Option<Result<&'a [FileDiff], &'a str>>,
    pub sel: usize,
    pub scroll: usize,
    pub confirm_discard: bool,
    pub message: Option<(&'a str, bool)>,
    pub busy: bool,
}

fn status_color(s: char) -> Rgb {
    match s {
        'A' => GREEN,
        'D' => RED,
        'R' => ACCENT,
        _ => YELLOW,
    }
}

#[allow(clippy::too_many_arguments)]
fn mono(ui: &mut Ui, text: impl Into<String>, x: f32, y: f32, w: f32, pt: f32, color: Rgb, align: Align) {
    ui.text(text, x, y, w, pt, color, false, align);
    if let Some(l) = ui.labels.last_mut() {
        l.font = Font::Mono;
    }
}

/// Botón con texto; devuelve su caja.
#[allow(clippy::too_many_arguments)]
fn button(
    ui: &mut Ui,
    label: &str,
    right: f32,
    y: f32,
    w: f32,
    color: Rgb,
    hit: Hit,
    hover: Option<Hit>,
) -> f32 {
    let h = ui.s(26.0);
    let b = Area::new(right - w, y, w, h);
    let bg = if hover == Some(hit) { ACTIVE_BG } else { HOVER_BG };
    ui.fill(Area::new(b.x - 1.0, b.y - 1.0, b.w + 2.0, b.h + 2.0), BORDER);
    ui.fill(b, bg);
    ui.text(label, b.x, b.y + ui.s(5.0), b.w, 12.0, color, true, Align::Center);
    ui.hit(b, hit);
    b.x
}

/// Panel a pantalla casi completa con el diff. Devuelve cuántas filas de
/// diff caben (para avanzar página).
pub fn draw_review(ui: &mut Ui, screen: Area, v: &ReviewView, hover: Option<Hit>) -> usize {
    ui.tint(screen, [0, 0, 0], 0.55);
    let margin = ui.s(28.0);
    let w = (screen.w - 2.0 * margin).min(ui.s(1500.0));
    let h = screen.h - 2.0 * margin;
    let bx = Area::new(screen.x + (screen.w - w) / 2.0, screen.y + margin, w, h);
    ui.fill(Area::new(bx.x - 1.0, bx.y - 1.0, bx.w + 2.0, bx.h + 2.0), BORDER);
    ui.fill(bx, PANEL_BG);

    let pad = ui.s(22.0);
    let x = bx.x + pad;
    let iw = bx.w - 2.0 * pad;
    let mut y = bx.y + ui.s(18.0);

    // Cabecera: qué se revisa y botones.
    ui.text("REVISAR CAMBIOS", x, y, iw, 10.5, DIM, true, Align::Left);
    ui.text("⌘R · esc", x, y, iw, 10.5, DIM, false, Align::Right);
    y += ui.s(20.0);
    let title = match v.branch {
        Some(b) => format!("{} · ⎇ {b}", v.project),
        None if v.target.is_empty() => v.project.to_string(),
        None => format!("{} · {}", v.project, v.target),
    };
    let mut right = x + iw;
    if v.branch.is_some() && !v.busy {
        let (label, color) = if v.confirm_discard { ("¿Borrar? ⌫", RED) } else { ("Descartar ⌫", DIM) };
        right = button(ui, label, right, y, ui.s(118.0), color, Hit::ReviewDiscard, hover) - ui.s(8.0);
        let merge = if v.target.is_empty() {
            "Fusionar ↵".to_string()
        } else {
            format!("Fusionar en {} ↵", v.target)
        };
        right = button(ui, &merge, right, y, ui.s(190.0), GREEN, Hit::ReviewMerge, hover) - ui.s(8.0);
    }
    ui.text(title, x, y, right - x - ui.s(8.0), 17.0, TEXT, true, Align::Left);
    y += ui.s(28.0);
    let (added, removed, nfiles) = match v.files {
        Some(Ok(f)) => (f.iter().map(|f| f.added).sum::<usize>(), f.iter().map(|f| f.removed).sum(), f.len()),
        _ => (0, 0, 0),
    };
    let what = match v.branch {
        Some(_) => "desde que empezó el agente".to_string(),
        None => "sin commit respecto a HEAD".to_string(),
    };
    let summary = match nfiles {
        0 => String::new(),
        1 => format!("1 archivo · {what}"),
        n => format!("{n} archivos · {what}"),
    };
    ui.text(&summary, x, y, iw, 12.0, DIM, false, Align::Left);
    if nfiles > 0 {
        ui.text(format!("−{removed}"), x, y, iw, 12.0, RED, true, Align::Right);
        ui.text(format!("+{added}"), x, y, iw - ui.s(44.0), 12.0, GREEN, true, Align::Right);
    }
    y += ui.s(26.0);
    ui.fill(Area::new(bx.x, y, bx.w, 1.0), BORDER);

    let footer_h = ui.s(40.0);
    let body = Area::new(bx.x, y + 1.0, bx.w, bx.bottom() - footer_h - y - 1.0);
    ui.fill(Area::new(bx.x, body.bottom(), bx.w, 1.0), BORDER);

    // Pie: mensaje o ayuda.
    let fy = body.bottom() + ui.s(12.0);
    let (hint, hcolor) = if v.confirm_discard {
        (
            format!(
                "⌫ otra vez para cerrar el agente y BORRAR su worktree y la rama {} (lo no fusionado se pierde) · esc cancelar",
                v.branch.unwrap_or_default()
            ),
            RED,
        )
    } else if let Some((m, err)) = v.message {
        (m.to_string(), if err { RED } else { GREEN })
    } else if v.branch.is_some() {
        ("↑↓ archivo · rueda, espacio o j/k desplazar · ↵ fusionar · ⌫ descartar · r recargar · esc cerrar".into(), DIM)
    } else {
        ("↑↓ archivo · rueda, espacio o j/k desplazar · r recargar · esc cerrar".into(), DIM)
    };
    ui.text(hint, x, fy, iw, 11.0, hcolor, false, Align::Left);

    let files = match v.files {
        None => {
            ui.text("Cargando cambios…", x, body.y + ui.s(24.0), iw, 13.0, DIM, false, Align::Left);
            return 0;
        }
        Some(Err(e)) => {
            ui.text(e, x, body.y + ui.s(24.0), iw, 13.0, RED, false, Align::Left);
            return 0;
        }
        Some(Ok([])) => {
            let msg = match v.branch {
                Some(_) => "El agente todavía no ha cambiado nada.",
                None => "No hay cambios sin commit.",
            };
            ui.text(msg, x, body.y + ui.s(24.0), iw, 13.0, DIM, false, Align::Left);
            return 0;
        }
        Some(Ok(f)) => f,
    };

    // Lista de archivos.
    let list_w = ui.s(300.0).min(body.w * 0.35);
    ui.fill(Area::new(body.x + list_w, body.y, 1.0, body.h), BORDER);
    let row_h = ui.s(26.0);
    let fit = ((body.h - ui.s(12.0)) / row_h).floor().max(1.0) as usize;
    let first = (v.sel + 1).saturating_sub(fit);
    let mut ry = body.y + ui.s(6.0);
    for (i, f) in files.iter().enumerate().skip(first).take(fit) {
        let row = Area::new(body.x + ui.s(6.0), ry, list_w - ui.s(12.0), row_h - ui.s(2.0));
        if i == v.sel {
            ui.fill(row, ACTIVE_BG);
            ui.fill(Area::new(row.x, row.y + ui.s(5.0), ui.s(3.0), row.h - ui.s(10.0)), ACCENT);
        } else if hover == Some(Hit::ReviewFile(i)) {
            ui.fill(row, HOVER_BG);
        }
        let ty = row.y + ui.s(4.0);
        mono(
            ui,
            f.status.to_string(),
            row.x + ui.s(10.0),
            ty,
            ui.s(14.0),
            12.0,
            status_color(f.status),
            Align::Left,
        );
        let name = f.path.rsplit('/').next().unwrap_or(&f.path);
        let stats = format!("+{} −{}", f.added, f.removed);
        let sw = ui.s(78.0);
        ui.text(name, row.x + ui.s(28.0), ty, row.w - ui.s(28.0) - sw, 12.5, TEXT, i == v.sel, Align::Left);
        ui.text(stats, row.right() - sw - ui.s(8.0), ty + ui.s(1.0), sw, 10.5, DIM, false, Align::Right);
        ui.hit(row, Hit::ReviewFile(i));
        ry += row_h;
    }

    // Diff del archivo elegido.
    let Some(file) = files.get(v.sel) else { return 0 };
    let dx = body.x + list_w + 1.0;
    let dw = body.right() - dx;
    let tx = dx + ui.s(14.0);
    ui.text(&file.path, tx, body.y + ui.s(8.0), dw - ui.s(28.0), 12.5, TEXT, true, Align::Left);
    let top = body.y + ui.s(34.0);
    let line_h = ui.s(17.0);
    let rows = ((body.bottom() - top - ui.s(6.0)) / line_h).floor().max(1.0) as usize;
    let gutter = ui.s(40.0);
    let mut ly = top;
    for l in file.lines.iter().skip(v.scroll).take(rows) {
        let band = Area::new(dx, ly, dw, line_h);
        let (color, sign) = match l.kind {
            LineKind::Added => {
                ui.tint(band, GREEN, 0.05);
                (TEXT, "+")
            }
            LineKind::Removed => {
                ui.tint(band, RED, 0.07);
                (TEXT, "−")
            }
            LineKind::Hunk => {
                ui.fill(band, HOVER_BG);
                ([0x61, 0xaf, 0xef], "")
            }
            LineKind::Note => (DIM, ""),
            LineKind::Context => ([0xc0, 0xb6, 0xab], " "),
        };
        let num = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
        if matches!(l.kind, LineKind::Hunk | LineKind::Note) {
            mono(ui, &l.text, tx, ly + ui.s(1.0), dw - ui.s(28.0), 11.0, color, Align::Left);
        } else {
            mono(ui, num(l.old), tx, ly + ui.s(1.0), gutter - ui.s(6.0), 11.0, DIM, Align::Right);
            mono(ui, num(l.new), tx + gutter, ly + ui.s(1.0), gutter - ui.s(6.0), 11.0, DIM, Align::Right);
            let sx = tx + 2.0 * gutter + ui.s(4.0);
            let scolor = if l.kind == LineKind::Added { GREEN } else { RED };
            mono(ui, sign, sx, ly + ui.s(1.0), ui.s(12.0), 11.5, scolor, Align::Left);
            mono(
                ui,
                &l.text,
                sx + ui.s(16.0),
                ly + ui.s(1.0),
                dx + dw - sx - ui.s(24.0),
                11.5,
                color,
                Align::Left,
            );
        }
        ly += line_h;
    }
    // Indicador de posición si no cabe entero.
    let total = file.lines.len();
    if total > rows {
        let track = Area::new(body.right() - ui.s(6.0), top, ui.s(3.0), rows as f32 * line_h);
        ui.fill(track, TRACK);
        let th = (track.h * rows as f32 / total as f32).max(ui.s(20.0));
        let ty = track.y + (track.h - th) * (v.scroll as f32 / (total - rows) as f32).min(1.0);
        ui.fill(Area::new(track.x, ty, track.w, th), BAR_IDLE);
    }
    // Clic en el panel: no lo cierra.
    ui.hit(bx, Hit::ReviewBox);
    rows
}

/// Barra de búsqueda flotante en la esquina superior derecha de un panel.
pub fn draw_search(ui: &mut Ui, pane: Area, query: &str, found: bool) {
    let w = ui.s(300.0).min(pane.w - ui.s(16.0));
    let h = ui.s(34.0);
    let bx = Area::new(pane.right() - w - ui.s(10.0), pane.y + ui.s(8.0), w, h);
    ui.fill(Area::new(bx.x - 1.0, bx.y - 1.0, bx.w + 2.0, bx.h + 2.0), BORDER);
    ui.fill(bx, PANEL_BG);
    let color = if query.is_empty() || found { TEXT } else { RED };
    let shown = if query.is_empty() { "Buscar…".to_string() } else { format!("{query}▏") };
    let qcolor = if query.is_empty() { DIM } else { color };
    ui.text("⌕", bx.x + ui.s(10.0), bx.y + ui.s(8.0), ui.s(16.0), 13.0, DIM, false, Align::Left);
    ui.text(shown, bx.x + ui.s(30.0), bx.y + ui.s(8.0), bx.w - ui.s(130.0), 13.0, qcolor, false, Align::Left);
    let hint = if !query.is_empty() && !found { "sin resultados" } else { "↵ ↑  ⇧↵ ↓  esc" };
    ui.text(hint, bx.x, bx.y + ui.s(10.0), bx.w - ui.s(10.0), 10.5, DIM, false, Align::Right);
}

/// "320 ms", "2,4 s", "1 min 05 s".
pub fn fmt_duration(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    match ms {
        0..=999 => format!("{ms} ms"),
        1000..=59_999 => format!("{:.1} s", ms as f64 / 1000.0).replace('.', ","),
        _ => format!("{} min {:02} s", ms / 60_000, (ms / 1000) % 60),
    }
}

/// Datos de un panel necesarios para pintar sus bloques.
pub struct BlockCtx<'a> {
    pub pane: u64,
    pub area: Area,
    pub origin: (f32, f32),
    pub cell: (f32, f32),
    pub snap: &'a forge_term::Snapshot,
    pub selected: Option<u64>,
    pub hovered: Option<u64>,
    pub ai: Option<Provider>,
}

/// Decoración de los bloques de un panel: separadores, barra de estado,
/// tinte de fallos y selección, etiqueta de duración y botón de IA.
pub fn draw_blocks(base: &mut Ui, overlay: &mut Ui, c: &BlockCtx) {
    let (ox, oy) = c.origin;
    let (cw, ch) = c.cell;
    let snap = c.snap;
    for b in &snap.blocks {
        let top = oy + b.first_row as f32 * ch;
        let h = (b.last_row - b.first_row + 1) as f32 * ch;
        let rows = Area::new(c.area.x, top, c.area.w, h);
        let selected = c.selected == Some(b.id);
        let failed = b.info.failed();

        if b.starts_here && b.first_row > 0 {
            base.fill(Area::new(c.area.x + base.s(6.0), top, c.area.w - base.s(12.0), 1.0), BORDER);
        }
        if selected {
            base.tint(rows, ACCENT, 0.10);
        } else if failed {
            base.tint(rows, RED, 0.06);
        }
        let bar = if selected {
            Some(ACCENT)
        } else if failed {
            Some(RED)
        } else if b.info.running() {
            Some(YELLOW)
        } else {
            None
        };
        if let Some(col) = bar {
            base.fill(Area::new(c.area.x + base.s(3.0), top + 2.0, base.s(3.0), h - 4.0), col);
        }

        if !b.starts_here {
            continue;
        }
        // Etiqueta a la derecha de la primera fila, si hay hueco libre.
        // Etiqueta a la derecha de la primera fila: versión larga si cabe, si no corta.
        let labels: Vec<(String, Rgb)> = if b.info.running() {
            vec![("● en curso".into(), YELLOW), ("●".into(), YELLOW)]
        } else if let Some(d) = b.info.duration() {
            let code = b.info.exit.unwrap_or(0);
            if failed {
                vec![
                    (format!("✗ código {code} · {}", fmt_duration(d)), RED),
                    (format!("✗ {code} · {}", fmt_duration(d)), RED),
                    (format!("✗ {code}"), RED),
                ]
            } else if d.as_millis() >= 500 {
                vec![(format!("✓ {}", fmt_duration(d)), DIM)]
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let free_at = |r: usize| snap.row(r).iter().rev().take_while(|x| x.c == ' ' || x.c == '\0').count();
        let free = free_at(b.first_row);
        if let Some((text, color)) = labels.into_iter().find(|(t, _)| free >= t.chars().count() + 3) {
            let need = text.chars().count() + 3;
            let w = need as f32 * cw;
            let x = ox + snap.cols as f32 * cw - w;
            base.text(
                text,
                x,
                top + (ch - base.s(16.0)) / 2.0,
                w - base.s(4.0),
                11.0,
                color,
                false,
                Align::Right,
            );
        }
        // Botón de IA en los bloques fallidos (al pasar el ratón o seleccionados):
        // en la última fila del bloque con hueco a la derecha, para no tapar texto.
        if failed
            && (selected || c.hovered == Some(b.id))
            && let Some(p) = c.ai
        {
            let text = format!("◆ Preguntar a {}", p.name());
            let bw = overlay.s(170.0);
            let bh = (ch - 2.0).min(overlay.s(24.0));
            let need = (bw / cw).ceil() as usize + 3;
            let first_out = b.output_row.unwrap_or(b.first_row);
            let row = (first_out..=b.last_row)
                .rev()
                .find(|r| *r != b.first_row && free_at(*r) >= need)
                .or_else(|| (free_at(b.first_row) >= need + 20).then_some(b.first_row))
                .unwrap_or(b.last_row);
            let y = oy + row as f32 * ch + (ch - bh) / 2.0;
            let btn = Area::new(c.area.right() - bw - overlay.s(14.0), y, bw, bh);
            overlay.fill(Area::new(btn.x - 1.0, btn.y - 1.0, btn.w + 2.0, btn.h + 2.0), accent(p));
            overlay.fill(btn, PANEL_BG);
            overlay.text(
                text,
                btn.x,
                btn.y + (bh - overlay.s(17.0)) / 2.0,
                btn.w,
                11.5,
                accent(p),
                true,
                Align::Center,
            );
            overlay.hit(btn, Hit::AskAi(c.pane, b.id));
        }
    }
}

fn week_limit(ui: &mut Ui, l: &LimitView, x: f32, y: &mut f32, w: f32) {
    ui.bar(x, *y + ui.s(4.0), w, l.percent, limit_color(l.percent));
    *y += ui.s(16.0);
    ui.text(format!("{:.0}% del límite", l.percent), x, *y, w, 11.0, DIM, false, Align::Left);
    if let Some(left) = l.left {
        ui.text(format!("quedan {}", tok(left)), x, *y, w, 11.0, TEXT, false, Align::Right);
    }
    *y += ui.s(22.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_hides_panels_when_narrow() {
        let l = layout(2000.0, 800.0, 1.0, true, true);
        assert!(l.left.is_some() && l.right.is_some());
        assert_eq!(l.term.x, LEFT_W);
        assert_eq!(l.term.w, 2000.0 - LEFT_W - RIGHT_W);
        // Estrecho: cae el derecho primero.
        let l = layout(700.0, 800.0, 1.0, true, true);
        assert!(l.left.is_some() && l.right.is_none());
        let l = layout(400.0, 800.0, 1.0, true, true);
        assert!(l.left.is_none() && l.right.is_none());
        assert_eq!(l.term.w, 400.0);
    }

    #[test]
    fn hit_priority() {
        let mut ui = Ui::new(1.0);
        ui.hit(Area::new(0.0, 0.0, 10.0, 10.0), Hit::CloseTab(0));
        ui.hit(Area::new(0.0, 0.0, 100.0, 100.0), Hit::Tab(0));
        assert_eq!(hit_at(&ui.hits, 5.0, 5.0), Some(Hit::CloseTab(0)));
        assert_eq!(hit_at(&ui.hits, 50.0, 50.0), Some(Hit::Tab(0)));
        assert_eq!(hit_at(&ui.hits, 500.0, 50.0), None);
    }
}

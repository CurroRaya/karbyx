// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! Cómo se ve el chat: barra de conversaciones, mensajes, herramientas y
//! caja de escritura.

use chrono::{DateTime, Local, NaiveDate};
use karbyx_agent::agent::Mode;
use karbyx_agent::know::Source;
use karbyx_agent::tools::{Detail, DiffLine, LineKind};

use super::md::{self, Block, mono, span};
use super::panels;
use super::{
    Body, Chat, ChatHit, Chats, Form, Item, MemoryView, Phase, Popup, Status, ToolItem, ToolState,
    folder_name,
};
use crate::gfx::{Gfx, Rich, RichDraw, RichSpan, TextPos};
use crate::ui::{self, Align, Area, DIM, EMBER, Font, HOT, Hit, Rgb, TEXT, Ui};

/// Fondo de la conversación: un punto más oscuro que el de los paneles.
const BG: Rgb = [0x12, 0x10, 0x0e];
const SIDE_BG: Rgb = [0x16, 0x14, 0x12];
const CARD: Rgb = [0x1a, 0x17, 0x14];
const CODE_BG: Rgb = [0x18, 0x15, 0x13];
const BUBBLE: Rgb = [0x2a, 0x23, 0x1d];
const LINE: Rgb = [0x2a, 0x25, 0x21];
pub(super) const FAINT: Rgb = [0x6a, 0x60, 0x57];
const EMBER_DEEP: Rgb = [0xd2, 0x5f, 0x2c];
const ON_EMBER: Rgb = [0x1c, 0x12, 0x0b];
const ADD_BG: Rgb = [0x7e, 0xb0, 0x5c];
const DEL_BG: Rgb = [0xd8, 0x5a, 0x4c];
const SELECT: Rgb = [0xf0, 0x8a, 0x4b];

/// Ancho máximo de la columna de lectura.
const COLUMN: f32 = 780.0;
/// Líneas que se ven de un diff o una salida sin desplegar.
const DIFF_FOLD: usize = 16;
const OUT_FOLD: usize = 8;

/// (título, texto) de las sugerencias de una conversación nueva.
pub const SUGGESTIONS: [(&str, &str); 3] = [
    ("Explícame este proyecto", "Explícame cómo está organizado este proyecto y qué hace cada parte."),
    (
        "Busca errores",
        "Revisa el código en busca de errores reales. Enséñame los más importantes antes de cambiar nada.",
    ),
    ("Arregla los tests", "Ejecuta los tests del proyecto y arregla los que fallen."),
];

/// Un texto colocado en pantalla (para selección y cursor).
#[derive(Debug, Clone)]
pub struct Placed {
    pub item: usize,
    pub piece: usize,
    pub key: u64,
    pub area: Area,
    pub line: f32,
}

/// Parte de un elemento, con posición relativa a él.
struct Piece {
    a: Area,
    what: What,
    /// Lo que va delante al copiar: separación, viñeta...
    lead: String,
}

/// Parte copiable de un elemento.
pub struct Copied {
    pub piece: usize,
    pub lead: String,
    pub text: String,
}

enum What {
    Text {
        rich: Rich,
        /// Se puede seleccionar y copiar.
        select: bool,
        /// Trozos de código dentro del texto (llevan fondo).
        codes: Vec<(TextPos, TextPos)>,
    },
    Panel {
        bg: Rgb,
        border: Rgb,
        radius: f32,
    },
    Round {
        color: Rgb,
        alpha: f32,
        radius: f32,
    },
    Label {
        text: String,
        pt: f32,
        color: Rgb,
        bold: bool,
        align: Align,
        mono: bool,
    },
    Button {
        text: String,
        hit: ChatHit,
        style: Btn,
    },
    /// Botón de un bloque de código («Copiar», «Ver»). Al desplazar se queda
    /// arriba de lo visible mientras se vea el bloque, que acaba en `end`.
    Copy {
        hit: ChatHit,
        text: &'static str,
        end: f32,
    },
    /// Icono de estado animado.
    Spin,
    /// No se ve: es lo que se copia de algo que no se puede seleccionar.
    Mark(String),
    /// Segundos desde un momento.
    Clock(std::time::Instant, &'static str),
    Hit(ChatHit),
}

#[derive(Clone, Copy, PartialEq)]
enum Btn {
    Primary,
    Plain,
}

/// Maquetación de un elemento, reutilizable mientras no cambie.
pub struct Cached {
    w: f32,
    scale: f32,
    rev: u64,
    h: f32,
    pieces: Vec<Piece>,
}

impl Cached {
    /// Lo que se copia, en orden.
    pub fn copied(&self) -> impl Iterator<Item = Copied> {
        self.pieces.iter().enumerate().filter_map(|(piece, p)| {
            let text = match &p.what {
                What::Text { rich, select: true, .. } => rich.spans.iter().map(|s| s.text.as_str()).collect(),
                What::Mark(t) => t.clone(),
                _ => return None,
            };
            Some(Copied { piece, lead: p.lead.clone(), text })
        })
    }

    /// Textos seleccionables: (trozo, texto).
    /// El primer «Ver» de este elemento (para las capturas).
    pub fn preview(&self) -> Option<ChatHit> {
        self.pieces.iter().find_map(|p| match p.what {
            What::Copy { hit: h @ ChatHit::Preview(..), .. } => Some(h),
            _ => None,
        })
    }

    pub fn texts(&self) -> impl Iterator<Item = (usize, String)> {
        self.pieces.iter().enumerate().filter_map(|(i, p)| match &p.what {
            What::Text { rich, select: true, .. } => {
                Some((i, rich.spans.iter().map(|s| s.text.as_str()).collect::<String>()))
            }
            _ => None,
        })
    }
}

/// Constructor de un elemento: va apilando trozos.
struct Build<'a> {
    gfx: &'a mut Gfx,
    s: f32,
    /// Escala del texto (la de la pantalla por el zoom).
    z: f32,
    w: f32,
    y: f32,
    pieces: Vec<Piece>,
}

impl Build<'_> {
    fn d(&self, v: f32) -> f32 {
        (v * self.s).round()
    }

    fn push(&mut self, a: Area, what: What) -> usize {
        self.pieces.push(Piece { a, what, lead: "\n".into() });
        self.pieces.len() - 1
    }

    /// Qué va delante del último trozo al copiarlo.
    fn lead(&mut self, s: impl Into<String>) {
        if let Some(p) = self.pieces.last_mut() {
            p.lead = s.into();
        }
    }

    fn rich(&self, spans: Vec<RichSpan>, pt: f32, lh: f32) -> Rich {
        let px = pt * self.z;
        Rich { spans, px, line: (px * lh).round() }
    }

    /// Texto en (x, y actual); devuelve su alto y avanza.
    fn text(&mut self, rich: Rich, x: f32, w: f32, select: bool) -> f32 {
        let (_, h) = self.gfx.rich(&rich, w);
        let codes = code_ranges(&rich.spans);
        let y = self.y;
        self.push(Area::new(x, y, w, h), What::Text { rich, select, codes });
        self.y += h;
        h
    }

    #[allow(clippy::too_many_arguments)]
    fn label(
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
        let h = (pt * self.s * 1.5).ceil();
        self.push(
            Area::new(x, y, w, h),
            What::Label { text: text.into(), pt, color, bold, align, mono: false },
        );
    }

    fn mono_label(&mut self, text: impl Into<String>, x: f32, y: f32, w: f32, pt: f32, color: Rgb) {
        let h = (pt * self.s * 1.5).ceil();
        self.push(
            Area::new(x, y, w, h),
            What::Label { text: text.into(), pt, color, bold: false, align: Align::Left, mono: true },
        );
    }

    /// Ancho aproximado de una etiqueta corta.
    fn label_w(&mut self, text: &str, pt: f32, bold: bool) -> f32 {
        let r = Rich {
            spans: vec![RichSpan { bold, ..span(text, TEXT) }],
            px: pt * self.s,
            line: pt * self.s * 1.5,
        };
        let (k, _) = self.gfx.rich(&r, 4000.0);
        self.gfx.rich_width(k).ceil()
    }

    fn code(&mut self, item: usize, lang: &str, text: &str, x: f32, w: f32) {
        let pad = self.d(14.0);
        let head = self.d(30.0);
        let top = self.y;
        let panel =
            self.push(Area::new(x, top, w, 0.0), What::Panel { bg: CODE_BG, border: LINE, radius: 10.0 });
        self.label(
            if lang.is_empty() { "código" } else { lang },
            x + pad,
            top + self.d(7.0),
            w / 2.0,
            10.5,
            FAINT,
            false,
            Align::Left,
        );
        let bw = self.d(64.0);
        let copy = self.push(
            Area::new(x + w - bw - self.d(6.0), top + self.d(4.0), bw, self.d(22.0)),
            What::Copy { hit: ChatHit::Inside, text: "Copiar", end: 0.0 },
        );
        let see = crate::preview::can_preview(lang, text).then(|| {
            let vw = self.d(44.0);
            self.push(
                Area::new(x + w - bw - vw - self.d(8.0), top + self.d(4.0), vw, self.d(22.0)),
                What::Copy { hit: ChatHit::Inside, text: "Ver", end: 0.0 },
            )
        });
        self.y = top + head;
        let rich = self.rich(md::highlight(lang, text), 14.0, 1.5);
        let body = self.pieces.len();
        self.text(rich, x + pad, w - 2.0 * pad, true);
        // El código no lleva fondo de «código en línea».
        if let What::Text { codes, .. } = &mut self.pieces[body].what {
            codes.clear();
        }
        self.y += pad;
        self.pieces[panel].a.h = self.y - top;
        let end = self.y;
        self.pieces[copy].what =
            What::Copy { hit: ChatHit::CopyCode(item as u16, body as u16), text: "Copiar", end };
        if let Some(see) = see {
            self.pieces[see].what =
                What::Copy { hit: ChatHit::Preview(item as u16, body as u16), text: "Ver", end };
        }
    }

    fn markdown(&mut self, item: usize, text: &str) {
        let w = self.w;
        let mut prev_item = false;
        for (n, b) in md::parse(text).into_iter().enumerate() {
            let is_item = matches!(b, Block::Item { .. });
            if n > 0 {
                let gap = match &b {
                    Block::Heading(..) => 22.0,
                    Block::Item { .. } if prev_item => 5.0,
                    _ => 12.0,
                };
                self.y += self.d(gap);
            }
            let tight = prev_item && is_item;
            prev_item = is_item;
            let from = self.pieces.len();
            let item_lead = match &b {
                Block::Item { depth, marker, .. } => format!("{}{marker} ", "  ".repeat(*depth)),
                _ => String::new(),
            };
            match b {
                Block::Para(s) => {
                    let r = self.rich(s, 15.0, 1.58);
                    self.text(r, 0.0, w, true);
                }
                Block::Heading(l, mut s) => {
                    for x in &mut s {
                        x.bold = true;
                    }
                    let pt = match l {
                        1 => 21.0,
                        2 => 18.0,
                        _ => 16.0,
                    };
                    let r = self.rich(s, pt, 1.35);
                    self.text(r, 0.0, w, true);
                }
                Block::Item { depth, marker, spans } => {
                    let x = self.d(22.0) * (depth as f32 + 1.0);
                    let y = self.y;
                    let mk = self.rich(vec![span(marker.clone(), EMBER)], 15.0, 1.58);
                    let (k, _) = self.gfx.rich(&mk, self.d(40.0));
                    let mw = self.gfx.rich_width(k);
                    self.push(
                        Area::new(x - mw - self.d(8.0), y, mw + 2.0, mk.line),
                        What::Text { rich: mk, select: false, codes: vec![] },
                    );
                    let r = self.rich(spans, 15.0, 1.58);
                    self.text(r, x, w - x, true);
                }
                Block::Quote(s) => {
                    let top = self.y;
                    let x = self.d(16.0);
                    let r = self.rich(s, 15.0, 1.55);
                    let h = self.text(r, x, w - x, true);
                    self.push(
                        Area::new(0.0, top + self.d(2.0), self.d(3.0), h - self.d(4.0)),
                        What::Round { color: EMBER, alpha: 0.6, radius: 1.5 },
                    );
                }
                Block::Code { lang, text } => self.code(item, &lang, &text, 0.0, w),
                Block::Rule => {
                    self.y += self.d(4.0);
                    let y = self.y;
                    self.push(
                        Area::new(0.0, y, w, 1.0),
                        What::Round { color: LINE, alpha: 1.0, radius: 0.0 },
                    );
                    self.y += self.d(4.0);
                }
                Block::Table(rows) => self.table(rows),
            }
            // Entre bloques, una línea en blanco; en una lista, no.
            if let Some(p) =
                self.pieces[from..].iter_mut().find(|p| matches!(p.what, What::Text { select: true, .. }))
            {
                p.lead = format!("{}{item_lead}", if tight { "\n" } else { "\n\n" });
            }
        }
    }

    fn table(&mut self, rows: Vec<Vec<Vec<RichSpan>>>) {
        let n = rows[0].len().max(1);
        let (pad_x, pad_y) = (self.d(12.0), self.d(7.0));
        // Anchos según el contenido, repartiendo lo que haya.
        let mut want = vec![self.d(40.0); n];
        for row in &rows {
            for (i, c) in row.iter().enumerate() {
                let r = self.rich(c.clone(), 14.0, 1.5);
                let (k, _) = self.gfx.rich(&r, 100_000.0);
                want[i] = want[i].max(self.gfx.rich_width(k) + 2.0 * pad_x + 2.0);
            }
        }
        let total: f32 = want.iter().sum();
        if total > self.w {
            let fair = self.w / n as f32;
            let over: f32 = want.iter().map(|x| (x - fair).max(0.0)).sum();
            let excess = total - self.w;
            for x in &mut want {
                *x -= ((*x - fair).max(0.0) / over.max(1.0)) * excess;
            }
        }
        let w: f32 = want.iter().sum();
        let top = self.y;
        let panel =
            self.push(Area::new(0.0, top, w, 0.0), What::Panel { bg: BG, border: LINE, radius: 10.0 });
        let head =
            self.push(Area::new(0.0, top, w, 0.0), What::Round { color: CARD, alpha: 1.0, radius: 10.0 });
        for (r, row) in rows.into_iter().enumerate() {
            let y = self.y;
            if r > 0 {
                self.push(Area::new(0.0, y, w, 1.0), What::Round { color: LINE, alpha: 1.0, radius: 0.0 });
            }
            let (mut x, mut h) = (0.0, 0.0f32);
            for (i, mut c) in row.into_iter().enumerate() {
                if r == 0 {
                    c.iter_mut().for_each(|s| s.bold = true);
                }
                let rich = self.rich(c, 14.0, 1.5);
                self.y = y + pad_y;
                h = h.max(self.text(rich, x + pad_x, want[i] - 2.0 * pad_x, true));
                self.lead(if i == 0 { "\n" } else { "\t" });
                x += want[i];
            }
            self.y = y + h + 2.0 * pad_y;
            if r == 0 {
                self.pieces[head].a.h = self.y - y;
            }
        }
        self.pieces[panel].a.h = self.y - top;
    }

    fn button(&mut self, text: &str, x: f32, hit: ChatHit, style: Btn) -> f32 {
        let w = self.label_w(text, 12.5, true) + self.d(26.0);
        let a = Area::new(x, self.y, w, self.d(30.0));
        self.push(a, What::Button { text: text.into(), hit, style });
        w
    }

    /// Cabecera de una herramienta: estado, verbo, objeto y resultado.
    fn tool_head(&mut self, i: usize, t: &ToolItem, x: f32, w: f32, foldable: bool) {
        let y = self.y;
        self.push(Area::new(x, y, 0.0, 0.0), What::Mark(format!("{} {}", t.verb, t.target).trim().into()));
        let icon = Area::new(x, y + self.d(5.0), self.d(16.0), self.d(20.0));
        match &t.state {
            ToolState::Running | ToolState::Queued => {
                self.push(icon, What::Spin);
            }
            ToolState::Asking(_) => {
                self.label("●", x, y + self.d(5.0), self.d(16.0), 11.0, HOT, false, Align::Left)
            }
            ToolState::Done { ok: true, .. } => {
                self.label("✓", x, y + self.d(5.0), self.d(16.0), 12.0, ui::GREEN, true, Align::Left)
            }
            ToolState::Done { .. } => {
                self.label("✕", x, y + self.d(5.0), self.d(16.0), 12.0, ui::RED, true, Align::Left)
            }
        }
        let vx = x + self.d(22.0);
        let vw = self.label_w(&t.verb, 13.0, true);
        self.label(t.verb.as_str(), vx, y + self.d(5.0), vw + self.d(4.0), 13.0, DIM, true, Align::Left);
        let right = self.d(170.0);
        let tx = vx + vw + self.d(8.0);
        // Un comando por aprobar ya se ve entero debajo.
        let whole = matches!((&t.state, &t.detail), (ToolState::Asking(_), Detail::Command { .. }));
        self.mono_label(
            if whole { String::new() } else { t.target.clone() },
            tx,
            y + self.d(6.0),
            x + w - right - tx,
            12.0,
            TEXT,
        );
        let rx = x + w - right;
        match &t.state {
            ToolState::Done { summary, took, ok } => {
                let mut r = summary.clone();
                if let Detail::Diff { added, removed, .. } = &t.detail
                    && *ok
                {
                    r = format!("+{added}  −{removed}");
                }
                if took.as_secs() >= 1 {
                    let secs = ui::fmt_duration(*took);
                    r = if r.is_empty() { secs } else { format!("{r} · {secs}") };
                }
                let c = if *ok { FAINT } else { ui::RED };
                self.label(r, rx, y + self.d(6.0), right, 11.5, c, false, Align::Right);
            }
            ToolState::Running => {
                self.push(Area::new(rx, y + self.d(6.0), right, self.d(18.0)), What::Clock(t.since, ""));
            }
            _ => {}
        }
        if foldable {
            self.push(Area::new(x, y, w, self.d(30.0)), What::Hit(ChatHit::Toggle(i as u16)));
        }
        self.y += self.d(30.0);
    }

    fn diff(&mut self, lines: &[DiffLine], x: f32, w: f32, fold: usize) {
        let shown = if lines.len() <= fold + 2 { lines.len() } else { fold };
        let pad = self.d(12.0);
        for l in &lines[..shown] {
            let (mark, color, bg) = match l.kind {
                LineKind::Add => ("+", [0xc9, 0xdf, 0xb2], Some(ADD_BG)),
                LineKind::Del => ("−", [0xea, 0xb9, 0xb0], Some(DEL_BG)),
                LineKind::Same => (" ", DIM, None),
                LineKind::Gap => ("⋯", FAINT, None),
            };
            let y = self.y;
            let text = l.text.replace('\t', "    ");
            let rich = self.rich(vec![mono(text, color)], 13.5, 1.45);
            let bar = self.push(
                Area::new(x, y, w, 0.0),
                What::Round {
                    color: bg.unwrap_or(BG),
                    alpha: if bg.is_some() { 0.065 } else { 0.0 },
                    radius: 0.0,
                },
            );
            let h = self.text(rich, x + pad + self.d(18.0), w - 2.0 * pad - self.d(18.0), true);
            if let Some(Piece { what: What::Text { codes, .. }, .. }) = self.pieces.last_mut() {
                codes.clear();
            }
            self.lead(format!("\n{mark} "));
            self.pieces[bar].a.h = h;
            let m = self.rich(vec![mono(mark, bg.unwrap_or(FAINT))], 13.5, 1.45);
            self.push(
                Area::new(x + pad, y, self.d(16.0), h),
                What::Text { rich: m, select: false, codes: vec![] },
            );
        }
    }

    fn more(&mut self, i: usize, hidden: usize, open: bool, x: f32, w: f32) {
        let y = self.y;
        let t = if open { "Plegar".to_string() } else { format!("Ver {hidden} líneas más") };
        self.label(t, x + self.d(12.0), y + self.d(5.0), w, 11.5, EMBER, false, Align::Left);
        self.push(Area::new(x, y, w, self.d(26.0)), What::Hit(ChatHit::Toggle(i as u16)));
        self.y += self.d(26.0);
    }

    fn output(&mut self, text: &str, x: f32, w: f32, color: Rgb) {
        let rich = self.rich(vec![mono(text.replace('\t', "    "), color)], 13.0, 1.45);
        self.text(rich, x, w, true);
        if let Some(Piece { what: What::Text { codes, .. }, .. }) = self.pieces.last_mut() {
            codes.clear();
        }
    }

    /// Final de la salida de una herramienta.
    fn tail(&mut self, i: usize, t: &ToolItem, open: bool, x: f32, cw: f32, w: f32) {
        let out = t.output.trim_end();
        if out.is_empty() {
            return;
        }
        let pad = self.d(14.0);
        let all: Vec<&str> = out.lines().collect();
        let n = all.len();
        let shown = if open { &all[n.saturating_sub(400)..] } else { &all[n.saturating_sub(OUT_FOLD)..] };
        let y = self.y;
        self.push(Area::new(0.0, y, w, 1.0), What::Round { color: LINE, alpha: 1.0, radius: 0.0 });
        self.y += self.d(8.0);
        self.output(&shown.join("\n"), x, cw, DIM);
        self.y += self.d(4.0);
        if n > OUT_FOLD {
            self.more(i, n - OUT_FOLD, open, pad + self.d(10.0), w - 2.0 * pad);
        }
    }

    fn tool(&mut self, i: usize, t: &ToolItem, open: bool) {
        let w = self.w;
        let asking = match &t.state {
            ToolState::Asking(a) => Some(a),
            _ => None,
        };
        let plain = matches!(t.detail, Detail::None);
        if plain && asking.is_none() {
            self.tool_head(i, t, 0.0, w, !t.output.is_empty());
            // Buscar sin buscador, o con una clave que no vale: se arregla desde aquí.
            let fix = |s: &str| s == karbyx_agent::browse::NO_ENGINE || s == karbyx_agent::browse::BAD_KEY;
            if matches!(&t.state, ToolState::Done { summary, .. } if fix(summary)) {
                self.y += self.d(4.0);
                let x = self.d(22.0);
                self.button("Elegir buscador", x, ChatHit::SearchSetup, Btn::Primary);
                self.y += self.d(36.0);
            }
            if open && !t.output.is_empty() {
                let (x, top) = (self.d(22.0), self.y);
                let panel = self.push(
                    Area::new(x, top, w - x, 0.0),
                    What::Panel { bg: CODE_BG, border: LINE, radius: 9.0 },
                );
                self.y += self.d(10.0);
                let lines: Vec<&str> = t.output.lines().take(200).collect();
                self.output(&lines.join("\n"), x + self.d(12.0), w - x - self.d(24.0), DIM);
                self.y += self.d(10.0);
                self.pieces[panel].a.h = self.y - top;
                self.y += self.d(6.0);
            }
            return;
        }
        // Tarjeta: ediciones, comandos y lo que pide permiso.
        let top = self.y;
        let border = if asking.is_some() { [0x7a, 0x55, 0x2c] } else { LINE };
        let panel = self.push(Area::new(0.0, top, w, 0.0), What::Panel { bg: CARD, border, radius: 11.0 });
        let pad = self.d(14.0);
        self.y += self.d(5.0);
        let fold = !matches!(t.detail, Detail::None);
        self.tool_head(i, t, pad, w - 2.0 * pad, fold && asking.is_none());
        match &t.detail {
            Detail::Diff { lines, new, .. } => {
                // Una edición ya hecha se ve resumida; una nueva, desde su principio.
                if !lines.is_empty() {
                    self.y += self.d(3.0);
                    // Lo que hay que aprobar se enseña casi entero.
                    let fold = if asking.is_some() { 5 * DIFF_FOLD } else { DIFF_FOLD };
                    self.diff(lines, 0.0, w, if open { lines.len() } else { fold });
                    let hidden = lines.len().saturating_sub(fold);
                    if hidden > 2 {
                        self.more(i, hidden, open, 0.0, w);
                    } else {
                        self.y += self.d(8.0);
                    }
                } else if *new {
                    self.y += self.d(4.0);
                }
            }
            Detail::Command { cmd, sandboxed } => {
                let x = pad + self.d(22.0);
                let cw = w - x - pad;
                if cmd.lines().count() > 1 || cmd.len() > 70 || asking.is_some() {
                    let rich = self.rich(md::highlight("bash", cmd), 13.5, 1.45);
                    self.text(rich, x, cw, true);
                    if let Some(Piece { what: What::Text { codes, .. }, .. }) = self.pieces.last_mut() {
                        codes.clear();
                    }
                    self.y += self.d(6.0);
                }
                self.tail(i, t, open, x, cw, w);
                if !sandboxed && asking.is_none() {
                    let y = self.y;
                    self.label("⚠ Fuera del sandbox", x, y, cw, 11.5, HOT, true, Align::Left);
                    self.y += self.d(20.0);
                }
                self.y += self.d(6.0);
            }
            Detail::Call(args) => {
                let x = pad + self.d(22.0);
                let cw = w - x - pad;
                // Lo que hay que aprobar se enseña entero.
                let lines: Vec<&str> = args.lines().collect();
                let n = if asking.is_some() || open { 400 } else { 8 };
                let more = if lines.len() > n { "\n…" } else { "" };
                let shown = format!("{}{more}", lines[..lines.len().min(n)].join("\n"));
                let rich = self.rich(md::highlight("json", &shown), 13.0, 1.45);
                self.text(rich, x, cw, true);
                if let Some(Piece { what: What::Text { codes, .. }, .. }) = self.pieces.last_mut() {
                    codes.clear();
                }
                self.y += self.d(6.0);
                self.tail(i, t, open, x, cw, w);
                self.y += self.d(6.0);
            }
            Detail::None => {
                // Una herramienta MCP sin argumentos también tiene salida.
                if open && !t.output.is_empty() {
                    self.tail(i, t, open, pad + self.d(22.0), w - 2.0 * pad - self.d(22.0), w);
                }
                self.y += self.d(4.0);
            }
        }
        if let Some(a) = asking {
            let y = self.y;
            self.push(Area::new(0.0, y, w, 1.0), What::Round { color: LINE, alpha: 1.0, radius: 0.0 });
            self.y += self.d(12.0);
            if let Some(r) = &a.reason {
                let y = self.y;
                self.label(
                    format!("⚠ Te pregunto porque {r}."),
                    pad,
                    y,
                    w - 2.0 * pad,
                    12.5,
                    HOT,
                    false,
                    Align::Left,
                );
                self.y += self.d(26.0);
            }
            let mut x = pad;
            x += self.button("Permitir  ⌘↵", x, ChatHit::Allow(t.id), Btn::Primary) + self.d(8.0);
            if a.reason.is_none() {
                x +=
                    self.button("Siempre en este chat", x, ChatHit::AllowAll(t.id), Btn::Plain) + self.d(8.0);
            }
            if a.tool.is_some() {
                let hit = ChatHit::AllowTool(t.id);
                x += self.button("Siempre esta herramienta", x, hit, Btn::Plain) + self.d(8.0);
            }
            if let Some(site) = &a.site {
                let text = if site == karbyx_agent::browse::SEARCH_SITE {
                    "Siempre al buscar".to_string()
                } else {
                    format!("Siempre en {site}")
                };
                x += self.button(&text, x, ChatHit::AllowSite(t.id), Btn::Plain) + self.d(8.0);
            }
            x += self.button("No, para  esc", x, ChatHit::Deny(t.id), Btn::Plain) + self.d(14.0);
            let y = self.y;
            self.label(
                "o escribe qué prefieres",
                x,
                y + self.d(7.0),
                (w - pad - x).max(0.0),
                11.5,
                FAINT,
                false,
                Align::Left,
            );
            self.y += self.d(30.0) + self.d(14.0);
        }
        self.pieces[panel].a.h = self.y - top;
    }
}

/// Rangos de los trozos de código dentro de un texto.
fn code_ranges(spans: &[RichSpan]) -> Vec<(TextPos, TextPos)> {
    let mut out = Vec::new();
    let mut pos: TextPos = (0, 0);
    for s in spans {
        let start = pos;
        for (i, l) in s.text.split('\n').enumerate() {
            if i > 0 {
                pos = (pos.0 + 1, 0);
            }
            pos.1 += l.len();
        }
        if s.mono && !s.text.is_empty() {
            out.push((start, pos));
        }
    }
    out
}

/// Espacio antes de un elemento.
fn gap(prev: Option<&Body>, body: &Body) -> f32 {
    let compact = |b: &Body| matches!(b, Body::Tool(t) if matches!(t.detail, Detail::None));
    match (prev, body) {
        (None, _) => 0.0,
        (_, Body::Think { took: Some(d), .. }) if d.as_secs_f32() < 2.0 => 0.0,
        (_, Body::User(_)) => 34.0,
        (Some(p), b) if compact(p) && compact(b) => 0.0,
        (Some(Body::Think { .. }), _) | (_, Body::Think { .. }) => 10.0,
        _ => 16.0,
    }
}

fn build(gfx: &mut Gfx, s: f32, z: f32, w: f32, i: usize, item: &Item) -> Cached {
    let mut b = Build { gfx, s, z, w, y: 0.0, pieces: Vec::new() };
    match &item.body {
        Body::User(t) => {
            let (px, py) = (b.d(16.0), b.d(11.0));
            let max = (w * 0.82).round();
            let rich = b.rich(vec![span(t.clone(), TEXT)], 15.0, 1.5);
            let (k, _) = b.gfx.rich(&rich, max - 2.0 * px);
            let tw = (b.gfx.rich_width(k).ceil() + 2.0).min(max - 2.0 * px);
            let x = w - tw - 2.0 * px;
            let panel = b.push(
                Area::new(x, 0.0, tw + 2.0 * px, 0.0),
                What::Round { color: BUBBLE, alpha: 1.0, radius: 15.0 },
            );
            b.y = py;
            let h = b.text(rich, x + px, tw, true);
            b.pieces[panel].a.h = h + 2.0 * py;
            b.y = h + 2.0 * py;
        }
        Body::Assistant(t) => b.markdown(i, t.trim_end()),
        // Los pensamientos cortos solo harían ruido.
        Body::Think { took: Some(d), .. } if d.as_secs_f32() < 2.0 => {}
        Body::Think { text, since, took } => {
            match took {
                Some(d) => b.label(
                    format!("{} Pensó {}", if item.open { "▾" } else { "▸" }, ui::fmt_duration(*d)),
                    0.0,
                    0.0,
                    w,
                    12.0,
                    FAINT,
                    false,
                    Align::Left,
                ),
                None => {
                    let h = b.d(18.0);
                    b.push(Area::new(0.0, 0.0, w, h), What::Clock(*since, "Pensando"));
                }
            }
            let h = b.d(22.0);
            b.push(Area::new(0.0, 0.0, w, h), What::Hit(ChatHit::Toggle(i as u16)));
            b.y = h;
            // Mientras piensa se ven sus últimas líneas.
            let shown = if item.open {
                text.trim().to_string()
            } else if took.is_none() {
                let l: Vec<&str> = text.trim().lines().filter(|l| !l.trim().is_empty()).collect();
                l[l.len().saturating_sub(2)..].join("\n")
            } else {
                String::new()
            };
            if !shown.is_empty() {
                let x = b.d(14.0);
                let top = b.y + b.d(2.0);
                b.y = top;
                let rich = b.rich(vec![RichSpan { italic: true, ..span(shown, DIM) }], 13.5, 1.5);
                let h = b.text(rich, x, w - x, true);
                b.push(Area::new(0.0, top, 2.0, h), What::Round { color: LINE, alpha: 1.0, radius: 1.0 });
            }
        }
        Body::Tool(t) => b.tool(i, t, item.open),
        Body::Summary { text, before, after } => {
            let mark = if item.open { "▾" } else { "▸" };
            let (b4, af) = (ui::tok(*before as u64), ui::tok(*after as u64));
            let h = b.d(34.0);
            let panel =
                b.push(Area::new(0.0, 0.0, w, h), What::Panel { bg: CARD, border: LINE, radius: 10.0 });
            let (px, ty) = (b.d(14.0), b.d(8.0));
            b.label(format!("{mark} Conversación resumida"), px, ty, w, 12.5, DIM, true, Align::Left);
            b.label(format!("{b4} → ≈{af}"), 0.0, ty, w - px, 11.5, FAINT, false, Align::Right);
            b.push(Area::new(0.0, 0.0, w, h), What::Hit(ChatHit::Toggle(i as u16)));
            b.y = h;
            if item.open {
                // El resumen, con sangría, dentro de la tarjeta.
                let from = b.pieces.len();
                b.w = w - 2.0 * px;
                b.markdown(i, text);
                b.w = w;
                for p in &mut b.pieces[from..] {
                    p.a.x += px;
                }
                b.y += px;
            }
            b.pieces[panel].a.h = b.y;
        }
        Body::Notice(t) => {
            let rich = b.rich(vec![span(t.clone(), FAINT)], 12.5, 1.45);
            b.text(rich, 0.0, w, true);
        }
        Body::Error(t) => {
            let (px, py) = (b.d(14.0), b.d(11.0));
            let panel = b.push(
                Area::new(0.0, 0.0, w, 0.0),
                What::Panel { bg: [0x24, 0x17, 0x14], border: [0x5a, 0x2e, 0x28], radius: 10.0 },
            );
            b.y = py;
            let rich = b.rich(vec![span(t.clone(), [0xf0, 0xb4, 0xaa])], 14.0, 1.5);
            b.text(rich, px, w - 2.0 * px, true);
            b.y += py;
            b.pieces[panel].a.h = b.y;
        }
    }
    let (h, pieces) = (b.y, b.pieces);
    Cached { w, scale: s * 1000.0 + z, rev: item.rev, h, pieces }
}

/// Latido suave para lo que está vivo.
fn pulse(tick: u64) -> f32 {
    [0.55, 0.8, 1.0, 0.8][(tick % 4) as usize]
}

const SPIN: [&str; 4] = ["◐", "◓", "◑", "◒"];

/// Lo que necesita el dibujo además del estado.
pub struct Env {
    pub tick: u64,
    pub hover: Option<ChatHit>,
    /// Zoom del texto (⌘+ y ⌘−).
    pub zoom: f32,
    pub focused: bool,
    pub projects: Vec<std::path::PathBuf>,
    pub claude: bool,
    /// Hay servidores MCP que valen en cualquier conversación.
    pub mcp: bool,
}

/// Botones para cambiar de vista. `on`: 0 chat, 1 mesa, 2 terminal.
pub fn draw_nav(ui: &mut Ui, a: Area, on: usize, hover: Option<Hit>) {
    ui.round(a, [0x0f, 0x0d, 0x0c], 1.0, 9.0);
    let w = a.w / 3.0;
    let tabs = [("Chat", Hit::OpenChat), ("Mesa", Hit::OpenBoard), ("Terminal", Hit::OpenTerminal)];
    for (i, (t, hit)) in tabs.into_iter().enumerate() {
        let b = Area::new(a.x + w * i as f32, a.y, w, a.h).inset(ui.s(3.0));
        if i == on {
            ui.round(b, ui::ACTIVE_BG, 1.0, 7.0);
        } else if hover == Some(hit) {
            ui.round(b, ui::HOVER_BG, 1.0, 7.0);
        }
        let c = if i == on {
            EMBER
        } else if hover == Some(hit) {
            TEXT
        } else {
            DIM
        };
        ui.text(t, b.x, b.y + (b.h - ui.s(17.0)) / 2.0, b.w, 11.5, c, true, Align::Center);
        ui.hit(b, hit);
    }
}

/// Apartado de la lista según la fecha.
fn day_group(updated: i64, today: NaiveDate) -> &'static str {
    let Some(d) = DateTime::from_timestamp(updated, 0) else { return "Antes" };
    match (today - d.with_timezone(&Local).date_naive()).num_days() {
        ..=0 => "Hoy",
        1 => "Ayer",
        2..=6 => "Esta semana",
        7..=30 => "Este mes",
        _ => "Antes",
    }
}

/// Barra izquierda: conversaciones y servidor.
pub fn draw_side(ui: &mut Ui, area: Area, chats: &mut Chats, env: &Env, hover: Option<Hit>) {
    ui.fill(area, SIDE_BG);
    // La lista va primero: lo que sobresale por arriba y por abajo se tapa.
    let (list_top, foot) = (area.y + ui.s(150.0), area.bottom() - ui.s(166.0));
    let view = Area::new(area.x, list_top, area.w - 1.0, (foot - list_top).max(0.0));
    let row = ui.s(48.0);
    let mut y = list_top - chats.side_scroll;
    let today = Local::now().date_naive();
    let mut group = "";
    ui.clip = Some(view);
    for i in chats.recent() {
        let c = &chats.list[i];
        let g = day_group(c.updated, today);
        if g != group {
            y += if group.is_empty() { 0.0 } else { ui.s(10.0) };
            ui.text(g, area.x + ui.s(20.0), y, area.w - ui.s(40.0), 10.5, FAINT, true, Align::Left);
            y += ui.s(22.0);
            group = g;
        }
        if y + row < view.y || y > view.bottom() {
            y += row + ui.s(2.0);
            continue;
        }
        let a = Area::new(area.x + ui.s(8.0), y, area.w - ui.s(16.0), row);
        let del = env.hover == Some(ChatHit::Delete(c.id));
        let hov = del || env.hover == Some(ChatHit::Open(c.id));
        if i == chats.cur {
            ui.round(a, ui::ACTIVE_BG, 1.0, 9.0);
        } else if hov {
            ui.round(a, ui::HOVER_BG, 1.0, 9.0);
        }
        let (dot, color) = if c.asking().is_some() {
            ("●", HOT)
        } else if c.busy() {
            (SPIN[(env.tick % 4) as usize], EMBER)
        } else if c.unseen {
            ("●", ui::GREEN)
        } else {
            ("", DIM)
        };
        let tx = a.x + ui.s(12.0);
        let tw = a.w - ui.s(44.0);
        ui.text(&c.title, tx, a.y + ui.s(6.0), tw, 13.0, TEXT, i == chats.cur || c.unseen, Align::Left);
        let sub = match &c.worktree {
            Some(w) => format!("{} · ⎇ {}", c.project(), w.name()),
            None => c.project(),
        };
        ui.text(sub, tx, a.y + ui.s(26.0), tw, 11.0, FAINT, false, Align::Left);
        let x_box = Area::new(a.right() - ui.s(30.0), a.y + ui.s(12.0), ui.s(24.0), ui.s(24.0));
        if chats.confirm == Some(c.id) {
            let b = Area::new(a.right() - ui.s(76.0), a.y + ui.s(12.0), ui.s(70.0), ui.s(24.0));
            ui.round(b, ui::RED, 0.9, 7.0);
            ui.text("¿Borrar?", b.x, b.y + ui.s(4.0), b.w, 11.0, [0x20, 0x0c, 0x0a], true, Align::Center);
            ui.hit(b, Hit::Chat(ChatHit::Delete(c.id)));
        } else if hov {
            if del {
                ui.round(x_box, LINE, 1.0, 7.0);
            }
            ui.text("×", x_box.x, x_box.y + ui.s(1.0), x_box.w, 15.0, DIM, false, Align::Center);
            ui.hit(x_box, Hit::Chat(ChatHit::Delete(c.id)));
        } else {
            ui.text(dot, x_box.x, x_box.y + ui.s(4.0), x_box.w, 11.0, color, true, Align::Center);
        }
        ui.hit(a, Hit::Chat(ChatHit::Open(c.id)));
        y += row + ui.s(2.0);
    }
    ui.clip = None;
    chats.side_area = area;
    chats.side_max = (y + chats.side_scroll - list_top - view.h).max(0.0);
    chats.side_scroll = chats.side_scroll.min(chats.side_max);
    ui.fill(Area::new(area.x, area.y, area.w, list_top - area.y), SIDE_BG);
    ui.fill(Area::new(area.x, foot, area.w, area.bottom() - foot), SIDE_BG);
    ui.fill(Area::new(area.right() - 1.0, area.y, 1.0, area.h), LINE);
    ui.gradient(Area::new(area.x, area.y, area.w - 1.0, ui.s(140.0)), (EMBER, 0.022), (EMBER, 0.0), 0.0);
    let pad = ui.s(14.0);
    let (x, w) = (area.x + pad, area.w - 2.0 * pad);
    let mut y = area.y + ui.s(16.0);
    let logo = Area::new(x + ui.s(2.0), y, ui.s(22.0), ui.s(22.0));
    ui.glow(logo, EMBER, 0.32, 6.0, 9.0);
    ui.gradient(logo, (HOT, 1.0), (EMBER_DEEP, 1.0), 6.0);
    ui.text("❯", logo.x, logo.y + ui.s(2.5), logo.w, 12.0, ON_EMBER, true, Align::Center);
    ui.text("Karbyx", logo.right() + ui.s(9.0), y + ui.s(0.5), w, 15.0, TEXT, true, Align::Left);
    y += ui.s(38.0);
    draw_nav(ui, Area::new(x, y, w, ui.s(32.0)), 0, hover);
    y += ui.s(44.0);

    let new = Area::new(x, y, w, ui.s(34.0));
    let h = env.hover == Some(ChatHit::New);
    ui.panel(new, if h { ui::ACTIVE_BG } else { CARD }, if h { EMBER } else { LINE }, 9.0, 1.0);
    ui.text(
        "＋  Nueva conversación",
        new.x + ui.s(12.0),
        new.y + ui.s(8.0),
        new.w,
        12.5,
        TEXT,
        true,
        Align::Left,
    );
    ui.text("⌘N", new.x, new.y + ui.s(9.0), new.w - ui.s(12.0), 11.0, FAINT, false, Align::Right);
    ui.hit(new, Hit::Chat(ChatHit::New));

    // Memoria, habilidades y servidores MCP.
    let top = area.bottom() - ui.s(160.0);
    ui.fill(Area::new(area.x, top - ui.s(6.0), area.w - 1.0, 1.0), LINE);
    let links = [
        ("✦", "Memoria y habilidades", ChatHit::Memory),
        ("⧉", "Servidores MCP", ChatHit::Mcp),
        ("⌕", "Buscador", ChatHit::SearchSetup),
    ];
    for (i, (icon, text, hit)) in links.into_iter().enumerate() {
        let m = Area::new(area.x + ui.s(8.0), top + ui.s(30.0) * i as f32, area.w - ui.s(16.0), ui.s(29.0));
        let hov = env.hover == Some(hit);
        if hov {
            ui.round(m, ui::HOVER_BG, 1.0, 9.0);
        }
        ui.text(icon, m.x + ui.s(11.0), m.y + ui.s(6.5), ui.s(16.0), 11.5, EMBER, true, Align::Left);
        let color = if hov { TEXT } else { DIM };
        ui.text(text, m.x + ui.s(30.0), m.y + ui.s(6.0), m.w, 12.0, color, false, Align::Left);
        // Sin buscador no se puede buscar: que se vea.
        if hit == ChatHit::SearchSetup {
            let (t, c) = match super::engine::chosen(&chats.saved.search) {
                Some(name) => (name, FAINT),
                None => ("sin elegir", HOT),
            };
            ui.text(t, m.x, m.y + ui.s(7.0), m.w - ui.s(10.0), 11.0, c, false, Align::Right);
        }
        ui.hit(m, Hit::Chat(hit));
    }

    // Servidor.
    let a = Area::new(area.x + ui.s(8.0), area.bottom() - ui.s(62.0), area.w - ui.s(16.0), ui.s(52.0));
    ui.fill(Area::new(area.x, a.y - ui.s(8.0), area.w - 1.0, 1.0), LINE);
    if env.hover == Some(ChatHit::ModelMenu) {
        ui.round(a, ui::HOVER_BG, 1.0, 9.0);
    }
    let (color, title, sub) = match (chats.model(), chats.servers.first()) {
        (Some((s, m)), _) => (ui::GREEN, s.name.clone(), m.name.clone()),
        (None, None) => (FAINT, "Sin servidor".into(), "Conecta tu modelo".to_string()),
        (None, Some(_)) => {
            let url = chats.saved.model.as_ref().map(|m| m.0.as_str());
            let s = chats
                .servers
                .iter()
                .find(|s| Some(s.server.url.as_str()) == url)
                .unwrap_or(&chats.servers[0]);
            match s.status {
                Status::Checking => (HOT, s.server.name.clone(), "Conectando…".into()),
                _ => (ui::RED, s.server.name.clone(), "No responde".into()),
            }
        }
    };
    let dot = Area::new(a.x + ui.s(13.0), a.y + ui.s(13.0), ui.s(8.0), ui.s(8.0));
    ui.glow(dot, color, 0.5, 4.0, 5.0);
    ui.round(dot, color, 1.0, 4.0);
    ui.text(title, a.x + ui.s(30.0), a.y + ui.s(7.0), a.w - ui.s(40.0), 12.5, TEXT, true, Align::Left);
    ui.text(sub, a.x + ui.s(30.0), a.y + ui.s(27.0), a.w - ui.s(40.0), 11.0, FAINT, false, Align::Left);
    ui.hit(a, Hit::Chat(ChatHit::ModelMenu));
}

/// Pinta la conversación actual y la caja de escritura.
pub fn draw_chat(
    ui: &mut Ui,
    over: &mut Ui,
    gfx: &mut Gfx,
    out: &mut Vec<RichDraw>,
    area: Area,
    chats: &mut Chats,
    env: &Env,
) {
    // La pone la barra lateral, si se ve.
    chats.side_area = Area::default();
    ui.fill(area, BG);
    let s = ui.s(100.0) / 100.0;
    let z = s * env.zoom;
    let col_w = (area.w - ui.s(64.0)).min(ui.s(COLUMN)).max(ui.s(200.0));
    let x = (area.x + (area.w - col_w) / 2.0).round();
    let has_model = chats.model().is_some();
    let model = chats.model().map(|(_, m)| (m.name.clone(), m.ctx, m.tools));
    let no_servers = chats.servers.is_empty();
    let Some(c) = chats.list.get_mut(chats.cur) else { return };

    // Caja de escritura (primero: decide cuánto queda para la conversación).
    let before = ui.rects.len();
    let top = composer(ui, gfx, out, Area::new(x, area.y, col_w, area.h), c, &model, z, env);
    // La caja se pinta por encima de la conversación.
    let box_rects = ui.rects.split_off(before);

    let head = ui.s(48.0);
    let view = Area::new(area.x, area.y + head, area.w, (top - ui.s(14.0) - area.y - head).max(1.0));
    c.view = view;
    c.placed.clear();

    if c.blank() {
        welcome(ui, view, x, col_w, c, has_model, no_servers, env);
    } else {
        messages(ui, gfx, out, view, x, col_w, c, s, z, env);
        // Lo que sobresale por arriba y por abajo se tapa.
        ui.fill(Area::new(area.x, area.y, area.w, head), BG);
        ui.fill(Area::new(area.x, view.bottom(), area.w, area.bottom() - view.bottom()), BG);
        ui.fill(Area::new(area.x, area.y + head - 1.0, area.w, 1.0), [0x1e, 0x1a, 0x17]);
        ui.text(&c.title, x, area.y + ui.s(15.0), col_w - ui.s(230.0), 13.0, DIM, true, Align::Left);
    }
    if c.worktree.is_some() {
        let b = Area::new(x + col_w - ui.s(150.0), area.y + ui.s(10.0), ui.s(150.0), ui.s(28.0));
        let hov = env.hover == Some(ChatHit::Review);
        ui.round(b, ui::GREEN, if hov { 0.26 } else { 0.13 }, 8.0);
        ui.text("Revisar cambios ⌘R", b.x, b.y + ui.s(6.0), b.w, 11.5, ui::GREEN, true, Align::Center);
        ui.hits.insert(0, (b, Hit::Chat(ChatHit::Review)));
    }
    ui.rects.extend(box_rects);
    if let Some((t, _)) = &chats.toast {
        let w = over.s(14.0) * 0.5 * t.chars().count() as f32 + over.s(36.0);
        let a = Area::new(area.x + (area.w - w) / 2.0, area.y + over.s(14.0), w, over.s(32.0));
        over.panel(a, ui::ACTIVE_BG, EMBER, 16.0, 1.0);
        over.text(t, a.x, a.y + over.s(7.0), a.w, 12.5, TEXT, true, Align::Center);
    }
    slash_menu(over, area, x, top, chats, env);
    let mut grew = None;
    match &chats.popup {
        Popup::None => {}
        Popup::Models => models_menu(over, area, x, top, chats, env),
        Popup::Projects => projects_menu(over, area, x, top, chats, env),
        Popup::Modes(sure) => modes_menu(over, area, x, top, chats, sure.is_some(), env),
        Popup::Memory(v) => memory_panel(over, area, v, env),
        Popup::Mcp(p) => grew = Some(panels::mcp_panel(over, area, chats, p, env)),
        Popup::McpForm(f) => panels::mcp_form(over, area, chats, f, env),
        Popup::McpSearch(f) => grew = Some(panels::mcp_search(over, area, f, env)),
        Popup::Skill(f) => {
            if let Some(c) = chats.current() {
                grew = Some(panels::skill_form(over, area, f, &c.know(), env));
            }
        }
        Popup::Form(f) => form(over, area, chats, f, env),
        Popup::Search(f) => super::engine::search_form(over, area, &chats.saved.search, f, env),
    }
    // Cuánto se puede desplazar el panel.
    match (&mut chats.popup, grew) {
        (Popup::Mcp(p), Some(m)) => (p.max, p.scroll) = (m, p.scroll.min(m)),
        (Popup::McpSearch(f), Some(m)) => (f.max, f.scroll) = (m, f.scroll.min(m)),
        (Popup::Skill(f), Some(m)) => (f.max, f.scroll) = (m, f.scroll.min(m)),
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn messages(
    ui: &mut Ui,
    gfx: &mut Gfx,
    out: &mut Vec<RichDraw>,
    view: Area,
    x: f32,
    w: f32,
    c: &mut Chat,
    s: f32,
    z: f32,
    env: &Env,
) {
    // Debajo de tus mensajes y de cada respuesta, Copiar y la fecha.
    let foot = footers(&c.items, c.busy());
    // Alturas (solo se maqueta lo que cambió).
    let mut ys = Vec::with_capacity(c.items.len());
    let mut y = ui.s(22.0);
    for i in 0..c.items.len() {
        let prev = i.checked_sub(1).map(|p| &c.items[p].body);
        y += ui.s(gap(prev, &c.items[i].body));
        let it = &c.items[i];
        let fresh =
            it.cache.as_ref().is_some_and(|k| k.w == w && k.scale == s * 1000.0 + z && k.rev == it.rev);
        if !fresh {
            let cached = build(gfx, s, z, w, i, it);
            c.items[i].cache = Some(cached);
        }
        ys.push(y);
        y += c.items[i].cache.as_ref().map_or(0.0, |k| k.h);
        if foot[i] {
            y += ui.s(FOOT);
        }
    }
    // Estado vivo al final.
    let live_y = y + ui.s(16.0);
    let live = live_text(c);
    if live.is_some() {
        y = live_y + ui.s(24.0);
    }
    y += ui.s(26.0);
    c.content_h = y;
    let max = (y - view.h).max(0.0);
    if c.stick {
        c.scroll = max;
    }
    c.scroll = c.scroll.clamp(0.0, max).round();
    let oy = view.y - c.scroll;
    ui.clip = Some(view);
    let sel = c.sel.map(|(a, b)| if a <= b { (a, b) } else { (b, a) });
    for (i, it) in c.items.iter().enumerate() {
        let Some(k) = &it.cache else { continue };
        let iy = oy + ys[i];
        if iy > view.bottom() || iy + k.h + ui.s(FOOT + 4.0) < view.y {
            continue;
        }
        for (j, p) in k.pieces.iter().enumerate() {
            let mut a = Area::new(x + p.a.x, iy + p.a.y, p.a.w, p.a.h);
            if let What::Copy { end, .. } = p.what {
                a.y = stick(a.y, view.y + ui.s(6.0), iy + end - ui.s(4.0) - a.h);
            }
            if a.y > view.bottom() || a.bottom() < view.y {
                continue;
            }
            paint(ui, gfx, out, view, i, j, &p.what, a, sel, &mut c.placed, env);
        }
        if foot[i] {
            let user = matches!(it.body, Body::User(_));
            let bw = ui.s(64.0);
            let bx = if user { x + w - bw + ui.s(6.0) } else { x - ui.s(6.0) };
            let b = Area::new(bx, iy + k.h + ui.s(5.0), bw, ui.s(22.0));
            let hit = ChatHit::CopyAnswer(i as u16);
            let hov = env.hover == Some(hit);
            if hov {
                clipped(ui, view, b, |ui, b| ui.round(b, ui::HOVER_BG, 1.0, 6.0));
            }
            ui.text(
                "Copiar",
                b.x,
                b.y + ui.s(3.0),
                b.w,
                11.0,
                if hov { TEXT } else { FAINT },
                false,
                Align::Center,
            );
            ui.hit(b, Hit::Chat(hit));
            let info = foot_text(&c.items, i);
            if !info.is_empty() {
                let (ty, gap) = (b.y + ui.s(3.0), ui.s(2.0));
                if user {
                    ui.text(info, x, ty, bx - x - gap, 11.0, FAINT, false, Align::Right);
                } else {
                    ui.text(info, bx + bw + gap, ty, w - bw, 11.0, FAINT, false, Align::Left);
                }
            }
        }
    }
    if let Some((text, since)) = live {
        let ly = oy + live_y;
        let col = [0xf0, 0x8a, 0x4b].map(|v: u8| (v as f32 * (0.55 + 0.45 * pulse(env.tick))) as u8);
        ui.text(SPIN[(env.tick % 4) as usize], x, ly, ui.s(18.0), 13.0, EMBER, false, Align::Left);
        let clock = since
            .filter(|t| t.elapsed().as_secs() >= 2)
            .map(|t| format!("  {}", ui::fmt_duration(t.elapsed())))
            .unwrap_or_default();
        ui.text(format!("{text}{clock}"), x + ui.s(22.0), ly + ui.s(1.0), w, 12.5, col, false, Align::Left);
    }
    ui.clip = None;
    // Barra de desplazamiento y botón para volver al final.
    if max > 0.0 {
        let h = (view.h * view.h / y).max(ui.s(30.0));
        let sy = view.y + (view.h - h) * (c.scroll / max);
        ui.round(Area::new(view.right() - ui.s(8.0), sy, ui.s(4.0), h), [0x57, 0x4d, 0x44], 0.5, 2.0);
    }
    if !c.stick && max - c.scroll > ui.s(120.0) {
        let b = Area::new(x + w / 2.0 - ui.s(16.0), view.bottom() - ui.s(40.0), ui.s(32.0), ui.s(32.0));
        ui.panel(b, ui::ACTIVE_BG, ui::BORDER, 16.0, 1.0);
        ui.text("↓", b.x, b.y + ui.s(6.0), b.w, 14.0, TEXT, true, Align::Center);
        ui.hits.insert(0, (b, Hit::Chat(ChatHit::Bottom)));
    }
}

/// Alto de la línea de Copiar y la fecha.
const FOOT: f32 = 26.0;

/// Qué elementos llevan Copiar y la fecha debajo: tus mensajes y la última
/// respuesta de cada turno (la del turno en curso, cuando acaba).
fn footers(items: &[Item], busy: bool) -> Vec<bool> {
    let mut out = vec![false; items.len()];
    // Recorriendo hacia atrás: ¿ya hay una respuesta más tarde en este turno?
    // ¿Este turno ya ha terminado (le sigue un mensaje tuyo)?
    let (mut answered, mut closed) = (false, false);
    for (i, it) in items.iter().enumerate().rev() {
        match it.body {
            Body::User(_) => {
                out[i] = true;
                (answered, closed) = (false, true);
            }
            Body::Assistant(_) => {
                out[i] = !answered && (closed || !busy);
                answered = true;
            }
            _ => {}
        }
    }
    out
}

/// La fecha de un elemento y, en una respuesta, lo que tardó desde tu mensaje.
fn foot_text(items: &[Item], i: usize) -> String {
    let Some(at) = items[i].at else { return String::new() };
    let mut s = when(at);
    if matches!(items[i].body, Body::Assistant(_))
        && let Some(sent) =
            items[..i].iter().rev().find(|it| matches!(it.body, Body::User(_))).and_then(|it| it.at)
        && at > sent
    {
        s = format!("{s} · {}", ui::fmt_duration(std::time::Duration::from_millis((at - sent) as u64)));
    }
    s
}

/// «26 nov 2026, 14:32», en tu hora.
fn when(ms: i64) -> String {
    use chrono::{Datelike, TimeZone, Timelike};
    const MONTHS: [&str; 12] =
        ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic"];
    let Some(t) = chrono::Local.timestamp_millis_opt(ms).single() else { return String::new() };
    format!("{} {} {}, {:02}:{:02}", t.day(), MONTHS[t.month0() as usize], t.year(), t.hour(), t.minute())
}

/// Qué está haciendo, si no se ve ya en el último elemento.
fn live_text(c: &Chat) -> Option<(String, Option<std::time::Instant>)> {
    match &c.phase {
        Phase::Idle | Phase::Writing => None,
        Phase::Reading(t) => Some(("Leyendo la conversación".into(), Some(*t))),
        Phase::Preparing(name, n) => {
            let what = match name.as_str() {
                "edit" => "Preparando un cambio",
                "write" => "Escribiendo un archivo",
                "bash" => "Preparando un comando",
                _ => "Preparando el siguiente paso",
            };
            let size = if *n > 400 { format!(" · {:.1} kB", *n as f32 / 1000.0) } else { String::new() };
            Some((format!("{what}{size}"), None))
        }
        Phase::Starting(n, t) => Some((format!("Arrancando {n}"), Some(*t))),
        Phase::Compacting(t) => Some(("Resumiendo la conversación para hacer sitio".into(), Some(*t))),
        Phase::Tool => None,
        Phase::Stopping => Some(("Parando…".into(), None)),
    }
}

/// Dónde va un botón que acompaña al desplazar: en su sitio (`y`), o bajado
/// hasta `top` si se ha ido por arriba, sin pasar de `last`.
fn stick(y: f32, top: f32, last: f32) -> f32 {
    y.max(top.min(last))
}

/// Pinta un rectángulo recortado a la zona visible.
fn clipped(ui: &mut Ui, view: Area, a: Area, f: impl FnOnce(&mut Ui, Area)) {
    let c = a.intersect(view);
    if c.w > 0.0 && c.h > 0.0 {
        f(ui, c);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint(
    ui: &mut Ui,
    gfx: &mut Gfx,
    out: &mut Vec<RichDraw>,
    view: Area,
    item: usize,
    piece: usize,
    what: &What,
    a: Area,
    sel: Option<(super::DocPos, super::DocPos)>,
    placed: &mut Vec<Placed>,
    env: &Env,
) {
    match what {
        What::Text { rich, select, codes } => {
            let (key, _) = gfx.rich(rich, a.w);
            for (p, q) in codes {
                for r in gfx.rich_boxes(key, *p, *q) {
                    let pad = ui.s(2.0);
                    let h = (rich.px * 1.32).round();
                    let b = Area::new(a.x + r.x - pad, a.y + r.y + (r.h - h) / 2.0, r.w + 2.0 * pad, h);
                    clipped(ui, view, b, |ui, b| ui.round(b, [0x29, 0x22, 0x1c], 1.0, 4.0));
                }
            }
            if *select {
                if let Some((from, to)) = sel {
                    let here = (item, piece);
                    if here >= (from.0, from.1) && here <= (to.0, to.1) {
                        let p = if here == (from.0, from.1) { from.2 } else { (0, 0) };
                        let q = if here == (to.0, to.1) { to.2 } else { (usize::MAX, usize::MAX) };
                        for r in gfx.rich_boxes(key, p, q) {
                            let b = Area::new(a.x + r.x, a.y + r.y, r.w, r.h);
                            clipped(ui, view, b, |ui, b| ui.tint(b, SELECT, 0.16));
                        }
                    }
                }
                placed.push(Placed { item, piece, key, area: a, line: rich.line });
            }
            out.push(RichDraw { key, x: a.x, y: a.y, clip: view });
        }
        What::Panel { bg, border, radius } => {
            // Recortada: las esquinas redondeadas solo se ven si están dentro.
            let full = a.y >= view.y && a.bottom() <= view.bottom();
            let m = ui.s(20.0);
            let grown = Area::new(view.x, view.y - m, view.w, view.h + 2.0 * m);
            let b = if full { a } else { a.intersect(grown) };
            ui.panel(b, *bg, *border, *radius, 1.0);
        }
        What::Round { color, alpha, radius } => {
            if *alpha > 0.0 {
                let m = ui.s(20.0);
                let grown = Area::new(view.x, view.y - m, view.w, view.h + 2.0 * m);
                let b = if *radius > 0.0 { a.intersect(grown) } else { a.intersect(view) };
                if b.h > 0.0 {
                    ui.round(b, *color, *alpha, *radius);
                }
            }
        }
        What::Mark(_) => {}
        What::Label { text, pt, color, bold, align, mono } => {
            ui.text(text, a.x, a.y, a.w, *pt, *color, *bold, *align);
            if *mono && let Some(l) = ui.labels.last_mut() {
                l.font = Font::Mono;
            }
        }
        What::Button { text, hit, style } => {
            let hov = env.hover == Some(*hit);
            let r = 8.0;
            match style {
                Btn::Primary => {
                    if hov {
                        ui.glow(a, EMBER, 0.3, r, 8.0);
                    }
                    ui.gradient(a, (if hov { HOT } else { [0xf6, 0x9a, 0x55] }, 1.0), (EMBER_DEEP, 1.0), r);
                    ui.text(text, a.x, a.y + ui.s(6.0), a.w, 12.5, ON_EMBER, true, Align::Center);
                }
                Btn::Plain => {
                    ui.panel(a, if hov { ui::ACTIVE_BG } else { ui::HOVER_BG }, ui::BORDER, r, 1.0);
                    ui.text(text, a.x, a.y + ui.s(6.0), a.w, 12.5, TEXT, true, Align::Center);
                }
            }
            ui.hit(a, Hit::Chat(*hit));
        }
        What::Copy { hit, text, .. } => {
            let hov = env.hover == Some(*hit);
            // Pegado arriba, tapa el código que pasa por debajo.
            clipped(ui, view, a, |ui, b| ui.round(b, CODE_BG, 1.0, 6.0));
            if hov {
                clipped(ui, view, a, |ui, b| ui.round(b, ui::ACTIVE_BG, 1.0, 6.0));
            }
            ui.text(
                *text,
                a.x,
                a.y + ui.s(3.0),
                a.w,
                11.0,
                if hov { TEXT } else { FAINT },
                false,
                Align::Center,
            );
            ui.hit(a, Hit::Chat(*hit));
        }
        What::Spin => {
            ui.text(SPIN[(env.tick % 4) as usize], a.x, a.y, a.w, 12.0, EMBER, false, Align::Left);
        }
        What::Clock(t, label) => {
            let d = t.elapsed();
            if !label.is_empty() {
                let c = [0xf0, 0x8a, 0x4b].map(|v: u8| (v as f32 * (0.55 + 0.45 * pulse(env.tick))) as u8);
                let secs =
                    if d.as_secs() >= 2 { format!("  {}", ui::fmt_duration(d)) } else { String::new() };
                ui.text(
                    format!("{} {label}{secs}", SPIN[(env.tick % 4) as usize]),
                    a.x,
                    a.y,
                    a.w,
                    12.5,
                    c,
                    false,
                    Align::Left,
                );
            } else if d.as_secs() >= 1 {
                ui.text(ui::fmt_duration(d), a.x, a.y, a.w, 11.5, FAINT, false, Align::Right);
            }
        }
        What::Hit(h) => ui.hit(a, Hit::Chat(*h)),
    }
}

/// Caja de escritura. Devuelve su borde superior.
#[allow(clippy::too_many_arguments)]
fn composer(
    ui: &mut Ui,
    gfx: &mut Gfx,
    out: &mut Vec<RichDraw>,
    col: Area,
    c: &mut Chat,
    model: &Option<(String, Option<u32>, Option<bool>)>,
    z: f32,
    env: &Env,
) -> f32 {
    let (px, py) = (ui.s(18.0), ui.s(14.0));
    let bar = ui.s(44.0);
    let tw = col.w - 2.0 * px;
    let text = c.input.text().to_string();
    let rich = Rich { spans: vec![span(text.clone(), TEXT)], px: 15.0 * z, line: (15.0 * z * 1.5).round() };
    let (key, th) = gfx.rich(&rich, tw);
    let max_h = rich.line * 9.0;
    let box_h = th.min(max_h);
    let h = box_h + 2.0 * py + bar - ui.s(6.0);
    let a = Area::new(col.x, col.bottom() - h - ui.s(22.0), col.w, h);
    let asking = c.asking().is_some();
    if env.focused {
        ui.glow(a, EMBER, if text.is_empty() { 0.022 } else { 0.05 }, 18.0, 22.0);
    }
    let border = if !text.is_empty() { [0x8a, 0x55, 0x34] } else { [0x3a, 0x32, 0x2b] };
    ui.panel(a, [0x1d, 0x19, 0x16], border, 18.0, 1.0);

    // Texto, con desplazamiento si no cabe.
    let caret = gfx.rich_caret(key, c.input.pos(c.input.caret())).unwrap_or((0.0, 0.0));
    let shift = if th > max_h { (caret.1 + rich.line - max_h).clamp(0.0, th - max_h) } else { 0.0 };
    let t = Area::new(a.x + px, a.y + py, tw, box_h);
    let placed = Area::new(t.x, t.y - shift, tw, th.max(rich.line));
    if let Some((p, q)) = c.input.selection() {
        for r in gfx.rich_boxes(key, c.input.pos(p), c.input.pos(q)) {
            let b = Area::new(placed.x + r.x, placed.y + r.y, r.w, r.h);
            clipped(ui, t, b, |ui, b| ui.tint(b, SELECT, 0.18));
        }
    }
    if text.is_empty() {
        let hint = match (asking, c.busy()) {
            (true, _) => "Dile qué prefieres, o contesta con los botones…",
            (_, true) => "↵ lo para y le dice esto · ⌘↵ se lo dice al acabar…",
            _ if c.root.is_some() => "¿Qué hacemos?",
            _ => "Pregunta lo que quieras…",
        };
        ui.text(
            hint,
            t.x + ui.s(5.0),
            t.y + (rich.line - ui.s(15.0) * 1.5) / 2.0,
            t.w,
            15.0,
            FAINT,
            false,
            Align::Left,
        );
    }
    out.push(RichDraw { key, x: placed.x, y: placed.y, clip: Area::new(t.x - 2.0, t.y, t.w + 4.0, t.h) });
    if env.focused {
        let ch = (15.0 * z * 1.25).round();
        let b = Area::new(
            placed.x + caret.0,
            placed.y + caret.1 + (rich.line - ch) / 2.0,
            ui.s(2.0).max(2.0),
            ch,
        );
        clipped(ui, Area::new(t.x - 2.0, t.y, t.w + 6.0, t.h), b, |ui, b| ui.round(b, EMBER, 1.0, 1.0));
    }
    c.input_at = Some(Placed { item: 0, piece: 0, key, area: placed, line: rich.line });

    // Barra inferior: carpeta, modelo, modo y enviar.
    let by = a.bottom() - bar + ui.s(2.0);
    let bh = ui.s(30.0);
    let mut x = a.x + ui.s(10.0);
    let mut pill =
        |ui: &mut Ui, gfx: &mut Gfx, icon: &str, ic: Rgb, text: &str, hit: ChatHit, color: Rgb, max: f32| {
            let r = Rich { spans: vec![span(text, TEXT)], px: ui.s(12.0), line: ui.s(18.0) };
            let (k, _) = gfx.rich(&r, 4000.0);
            let w = (gfx.rich_width(k).ceil() + ui.s(36.0)).min(max);
            let b = Area::new(x, by, w, bh);
            if env.hover == Some(hit) {
                ui.round(b, ui::ACTIVE_BG, 1.0, 9.0);
            }
            ui.text(icon, b.x + ui.s(9.0), b.y + ui.s(6.5), ui.s(16.0), 11.5, ic, true, Align::Left);
            ui.text(text, b.x + ui.s(26.0), b.y + ui.s(6.5), w - ui.s(32.0), 12.0, color, false, Align::Left);
            ui.hit(b, Hit::Chat(hit));
            x += w + ui.s(4.0);
        };
    let folder = match (&c.worktree, &c.root) {
        (Some(w), _) => format!("{} ⎇ {}", c.project(), w.name()),
        (None, Some(p)) => folder_name(p),
        (None, None) => "Sin carpeta".into(),
    };
    pill(ui, gfx, "⌂", EMBER, &folder, ChatHit::ProjectMenu, DIM, ui.s(200.0));
    let (name, nc) = match model {
        Some((n, ..)) => (n.as_str(), DIM),
        None => ("Elige un modelo", HOT),
    };
    pill(ui, gfx, "◆", if model.is_some() { EMBER } else { HOT }, name, ChatHit::ModelMenu, nc, ui.s(250.0));
    // Sin carpeta solo hay algo que permitir si hay servidores MCP.
    if c.root.is_some() || env.mcp {
        let (icon, t, ic, color) = match c.mode() {
            Mode::Ask => ("?", "Pregunta antes", EMBER, DIM),
            Mode::Auto => ("»", "Automático", HOT, HOT),
            Mode::Full => ("⚠", "Total", ui::RED, ui::RED),
        };
        pill(ui, gfx, icon, ic, t, ChatHit::ModeMenu, color, ui.s(150.0));
    }

    // Enviar / parar.
    let b = Area::new(a.right() - ui.s(42.0), by - ui.s(1.0), ui.s(32.0), ui.s(32.0));
    let stop = c.busy() && text.is_empty();
    let hit = if stop { ChatHit::Stop } else { ChatHit::Send };
    let hov = env.hover == Some(hit);
    if stop {
        ui.panel(b, if hov { ui::ACTIVE_BG } else { ui::HOVER_BG }, EMBER, 16.0, 1.0);
        ui.round(b.inset(ui.s(11.0)), EMBER, 1.0, 2.0);
    } else if text.trim().is_empty() {
        ui.round(b, ui::ACTIVE_BG, 1.0, 16.0);
        ui.text("↑", b.x, b.y + ui.s(5.0), b.w, 15.0, FAINT, true, Align::Center);
    } else {
        ui.gradient(b, (if hov { HOT } else { [0xf6, 0x9a, 0x55] }, 1.0), (EMBER_DEEP, 1.0), 16.0);
        ui.text("↑", b.x, b.y + ui.s(5.0), b.w, 15.0, ON_EMBER, true, Align::Center);
    }
    ui.hit(b, Hit::Chat(hit));

    // Contexto usado y velocidad.
    if let Some(st) = c.stats {
        let used = st.prompt + st.output;
        let mut t = match model.as_ref().and_then(|m| m.1) {
            Some(ctx) => format!("{} de {}", ui::tok(used as u64), ui::tok(ctx as u64)),
            None => format!("{} tok", ui::tok(used as u64)),
        };
        if let Some(v) = st.speed {
            t = format!("{v:.0} tok/s  ·  {t}");
        }
        let w = b.x - ui.s(40.0) - x;
        if w > ui.s(110.0) {
            // Al pasar el ratón ofrece resumir.
            let hov = env.hover == Some(ChatHit::Compact) && !c.busy();
            let (t, color) = if hov { ("Resumir ahora".to_string(), EMBER) } else { (t, FAINT) };
            ui.text(t, x, by + ui.s(7.0), w, 11.0, color, false, Align::Right);
            let zone =
                Area::new(b.x - ui.s(190.0).min(w + ui.s(40.0)), by, ui.s(186.0).min(w + ui.s(36.0)), bh);
            if !c.busy() {
                ui.hit(zone, Hit::Chat(ChatHit::Compact));
            }
            if let Some(ctx) = model.as_ref().and_then(|m| m.1) {
                let ring = Area::new(b.x - ui.s(34.0), by + ui.s(13.0), ui.s(24.0), ui.s(4.0));
                let f = (used as f32 / ctx as f32).clamp(0.03, 1.0);
                ui.round(ring, LINE, 1.0, 2.0);
                let col = if f > 0.85 {
                    ui::RED
                } else if f > 0.65 {
                    HOT
                } else {
                    EMBER
                };
                ui.round(Area::new(ring.x, ring.y, (ring.w * f).round(), ring.h), col, 1.0, 2.0);
            }
        }
    }
    let mut top = a.y;
    if let Some(q) = &c.queued {
        let b = Area::new(a.x + ui.s(14.0), a.y - ui.s(32.0), a.w - ui.s(28.0), ui.s(26.0));
        ui.round(b, ui::HOVER_BG, 1.0, 8.0);
        let one = crate::agents::one_line(q, 160);
        ui.text(
            format!("{}: {one}", if c.phase == Phase::Stopping { "En cuanto pare" } else { "Después" }),
            b.x + ui.s(12.0),
            b.y + ui.s(5.0),
            b.w - ui.s(24.0),
            12.0,
            DIM,
            false,
            Align::Left,
        );
        top = b.y;
    }
    top
}

#[allow(clippy::too_many_arguments)]
fn welcome(ui: &mut Ui, view: Area, x: f32, w: f32, c: &Chat, has_model: bool, no_servers: bool, env: &Env) {
    let mut y = view.y + (view.h * 0.30).min(ui.s(190.0));
    let logo = Area::new(x + w / 2.0 - ui.s(26.0), y - ui.s(72.0), ui.s(52.0), ui.s(52.0));
    ui.glow(logo, EMBER, 0.16, 14.0, 40.0);
    ui.gradient(logo, (HOT, 1.0), (EMBER_DEEP, 1.0), 14.0);
    ui.text("❯", logo.x, logo.y + ui.s(8.0), logo.w, 25.0, ON_EMBER, true, Align::Center);
    let (title, sub) = if no_servers {
        (
            "Conecta tu modelo".to_string(),
            "El modelo corre en este Mac o en un PC de tu red, con llama.cpp u otro servidor. Karbyx trabaja aquí, en tu Mac.",
        )
    } else if !has_model {
        (
            "Tu servidor no responde".to_string(),
            "Arranca el modelo (y enciende el PC, si está ahí); Karbyx lo reintenta solo.",
        )
    } else if let Some(p) = &c.root {
        (
            format!("¿Qué hacemos en {}?", folder_name(p)),
            match c.mode() {
                Mode::Ask => {
                    "Puedo leer, editar y ejecutar en esta carpeta. Te pido permiso antes de cambiar nada."
                }
                Mode::Auto => {
                    "Edito y ejecuto en esta carpeta sin preguntar. Para salir de ella, te pido permiso."
                }
                Mode::Full => "Lo hago todo sin preguntar, también fuera de esta carpeta y del sandbox.",
            },
        )
    } else {
        (
            "¿Qué hacemos?".to_string(),
            "Elige una carpeta para trabajar en un proyecto, o pregunta lo que quieras.",
        )
    };
    ui.text(title, x, y, w, 26.0, TEXT, true, Align::Center);
    y += ui.s(46.0);
    ui.text(sub, x, y, w, 14.0, DIM, false, Align::Center);
    y += ui.s(48.0);
    if no_servers {
        let b = Area::new(x + w / 2.0 - ui.s(110.0), y, ui.s(220.0), ui.s(40.0));
        let hov = env.hover == Some(ChatHit::AddServer);
        ui.glow(b, EMBER, if hov { 0.3 } else { 0.1 }, 11.0, 14.0);
        ui.gradient(b, (if hov { HOT } else { [0xf6, 0x9a, 0x55] }, 1.0), (EMBER_DEEP, 1.0), 11.0);
        ui.text("Conectar mi modelo", b.x, b.y + ui.s(9.0), b.w, 14.0, ON_EMBER, true, Align::Center);
        ui.hit(b, Hit::Chat(ChatHit::AddServer));
    } else if has_model && c.root.is_some() {
        let g = ui.s(10.0);
        let cw = ((w - 2.0 * g) / 3.0).min(ui.s(230.0));
        let mut cx = x + (w - 3.0 * cw - 2.0 * g) / 2.0;
        for (i, (t, _)) in SUGGESTIONS.iter().enumerate() {
            let b = Area::new(cx, y, cw, ui.s(42.0));
            let hit = ChatHit::Suggest(i as u8);
            let hov = env.hover == Some(hit);
            ui.panel(b, if hov { ui::ACTIVE_BG } else { CARD }, if hov { EMBER } else { LINE }, 11.0, 1.0);
            ui.text(*t, b.x, b.y + ui.s(11.0), b.w, 13.0, if hov { TEXT } else { DIM }, false, Align::Center);
            ui.hit(b, Hit::Chat(hit));
            cx += cw + g;
        }
    } else {
        let (hit, t) = if has_model {
            (ChatHit::ProjectMenu, "Elegir carpeta")
        } else {
            (ChatHit::ModelMenu, "Ver servidores")
        };
        let b = Area::new(x + w / 2.0 - ui.s(80.0), y, ui.s(160.0), ui.s(38.0));
        let hov = env.hover == Some(hit);
        ui.panel(b, if hov { ui::ACTIVE_BG } else { CARD }, if hov { EMBER } else { ui::BORDER }, 11.0, 1.0);
        ui.text(t, b.x, b.y + ui.s(9.0), b.w, 13.0, TEXT, true, Align::Center);
        ui.hit(b, Hit::Chat(hit));
    }
}

/// Caja flotante encima de la caja de escritura.
fn popup_box(ui: &mut Ui, x: f32, bottom: f32, w: f32, h: f32) -> Area {
    let a = Area::new(x, bottom - h - ui.s(8.0), w, h);
    ui.glow(a, [0, 0, 0], 0.6, 13.0, 26.0);
    ui.panel(a, [0x20, 0x1b, 0x18], ui::BORDER, 13.0, 1.0);
    a
}

/// Menú de habilidades al escribir «/».
fn slash_menu(ui: &mut Ui, screen: Area, x: f32, bottom: f32, chats: &Chats, env: &Env) {
    let list = chats.slash();
    if list.is_empty() {
        return;
    }
    let rh = ui.s(34.0);
    let w = ui.s(620.0).min(screen.w - ui.s(40.0));
    let a = popup_box(ui, x, bottom, w, rh * list.len() as f32 + ui.s(12.0));
    for (i, s) in list.iter().enumerate() {
        let b = Area::new(a.x + ui.s(6.0), a.y + ui.s(6.0) + rh * i as f32, a.w - ui.s(12.0), rh);
        let hit = ChatHit::PickSkill(i as u16);
        if i == chats.slash_sel || env.hover == Some(hit) {
            ui.round(b, ui::ACTIVE_BG, 1.0, 8.0);
        }
        let name = format!("/{}", s.name);
        ui.text(name, b.x + ui.s(12.0), b.y + ui.s(8.0), ui.s(170.0), 13.0, EMBER, true, Align::Left);
        let dx = b.x + ui.s(186.0);
        ui.text(
            &s.description,
            dx,
            b.y + ui.s(8.5),
            b.right() - dx - ui.s(12.0),
            12.0,
            DIM,
            false,
            Align::Left,
        );
        ui.hit(b, Hit::Chat(hit));
    }
    ui.hit(a, Hit::Chat(ChatHit::Inside));
}

/// Ventana centrada con título. Devuelve (x, ancho, y del contenido, caja).
pub(super) fn window(
    ui: &mut Ui,
    screen: Area,
    w: f32,
    h: f32,
    title: &str,
    about: &str,
) -> (f32, f32, f32, Area) {
    ui.tint(screen, [0, 0, 0], 0.55);
    let w = ui.s(w).min(screen.w - ui.s(40.0));
    let h = ui.s(h).min(screen.h - ui.s(40.0));
    let a = Area::new(screen.x + (screen.w - w) / 2.0, screen.y + (screen.h - h) / 2.0, w, h);
    ui.glow(a, [0, 0, 0], 0.7, 16.0, 40.0);
    ui.panel(a, [0x1d, 0x19, 0x16], ui::BORDER, 16.0, 1.0);
    let pad = ui.s(26.0);
    let (x, w) = (a.x + pad, a.w - 2.0 * pad);
    ui.text(title, x, a.y + ui.s(22.0), w, 19.0, TEXT, true, Align::Left);
    ui.text(about, x, a.y + ui.s(54.0), w, 12.5, DIM, false, Align::Left);
    (x, w, a.y + ui.s(88.0), a)
}

/// Botones de ventana; devuelven su borde izquierdo.
pub(super) fn primary(ui: &mut Ui, t: &str, right: f32, y: f32, w: f32, hit: ChatHit, env: &Env) -> f32 {
    let b = Area::new(right - w, y, w, ui.s(36.0));
    let hov = env.hover == Some(hit);
    ui.gradient(b, (if hov { HOT } else { [0xf6, 0x9a, 0x55] }, 1.0), (EMBER_DEEP, 1.0), 10.0);
    ui.text(t, b.x, b.y + ui.s(8.0), b.w, 13.0, ON_EMBER, true, Align::Center);
    ui.hit(b, Hit::Chat(hit));
    b.x
}

pub(super) fn secondary(ui: &mut Ui, t: &str, right: f32, y: f32, w: f32, hit: ChatHit, env: &Env) -> f32 {
    let b = Area::new(right - w, y, w, ui.s(36.0));
    let hov = env.hover == Some(hit);
    ui.panel(b, if hov { ui::ACTIVE_BG } else { ui::HOVER_BG }, ui::BORDER, 10.0, 1.0);
    ui.text(t, b.x, b.y + ui.s(8.0), b.w, 13.0, TEXT, true, Align::Center);
    ui.hit(b, Hit::Chat(hit));
    b.x
}

/// Panel de memoria y habilidades.
fn memory_panel(ui: &mut Ui, screen: Area, v: &MemoryView, env: &Env) {
    ui.tint(screen, [0, 0, 0], 0.55);
    let w = ui.s(680.0).min(screen.w - ui.s(40.0));
    // Crece con lo que hay que enseñar.
    let notes = (v.user.len().max(1) + v.project.len().max(1)).min(16) + v.skills.len().min(12);
    let h = ui.s(250.0 + 27.0 * notes as f32).min(screen.h - ui.s(40.0));
    let a = Area::new(screen.x + (screen.w - w) / 2.0, screen.y + (screen.h - h) / 2.0, w, h);
    ui.glow(a, [0, 0, 0], 0.7, 16.0, 40.0);
    ui.panel(a, [0x1d, 0x19, 0x16], ui::BORDER, 16.0, 1.0);
    let pad = ui.s(26.0);
    let (x, w) = (a.x + pad, a.w - 2.0 * pad);
    let mut y = a.y + ui.s(22.0);
    ui.text("Memoria y habilidades", x, y, w, 19.0, TEXT, true, Align::Left);
    y += ui.s(32.0);
    let about = "Lo que tu modelo tiene en cuenta en cada conversación. Nada se guarda sin que lo apruebes, salvo en modo Total.";
    ui.text(about, x, y, w, 12.5, DIM, false, Align::Left);
    y += ui.s(34.0);
    // Se reparte el alto: las notas mandan, pero siempre se ven habilidades.
    let bottom = a.bottom() - ui.s(58.0);
    let row = ui.s(27.0);
    let link = |ui: &mut Ui, text: &str, y: f32, hit: ChatHit| {
        let b = Area::new(x + w - ui.s(150.0), y - ui.s(3.0), ui.s(150.0), ui.s(22.0));
        let c = if env.hover == Some(hit) { EMBER } else { FAINT };
        ui.text(text, b.x, b.y + ui.s(4.0), b.w, 11.0, c, false, Align::Right);
        ui.hit(b, Hit::Chat(hit));
    };
    let project = v.name.clone().unwrap_or_default();
    let blocks = [(false, "SOBRE TI".to_string(), &v.user), (true, project.to_uppercase(), &v.project)];
    for (is_project, title, notes) in blocks {
        if is_project && v.name.is_none() {
            continue;
        }
        ui.text(title, x, y, w * 0.6, 10.5, EMBER, true, Align::Left);
        link(ui, "Editar el archivo", y, ChatHit::MemoryFile(is_project));
        y += ui.s(24.0);
        if notes.is_empty() {
            let t = if is_project {
                "Nada todavía. Dile, por ejemplo: «recuerda que aquí los tests se lanzan con make test»."
            } else {
                "Nada todavía. Dile, por ejemplo: «recuerda que prefiero respuestas cortas»."
            };
            ui.text(t, x, y, w, 12.0, FAINT, false, Align::Left);
            y += row;
        }
        let fit = (((bottom - y) * 0.42 / row) as usize).max(2);
        // Las últimas son las que más cuentan.
        let skip = notes.len().saturating_sub(fit);
        for (i, n) in notes.iter().enumerate().skip(skip) {
            ui.text("•", x, y, ui.s(12.0), 12.5, EMBER, false, Align::Left);
            ui.text(n, x + ui.s(14.0), y, w - ui.s(44.0), 12.5, TEXT, false, Align::Left);
            let hit = ChatHit::ForgetNote(is_project, i as u16);
            let b = Area::new(x + w - ui.s(24.0), y - ui.s(3.0), ui.s(24.0), ui.s(24.0));
            let hov = env.hover == Some(hit);
            if hov {
                ui.round(b, LINE, 1.0, 7.0);
            }
            ui.text(
                "×",
                b.x,
                b.y + ui.s(1.0),
                b.w,
                15.0,
                if hov { ui::RED } else { FAINT },
                false,
                Align::Center,
            );
            ui.hit(b, Hit::Chat(hit));
            y += row;
        }
        if skip > 0 {
            let t = format!("y {skip} más antiguas, en el archivo");
            ui.text(t, x, y, w, 11.0, FAINT, false, Align::Left);
            y += row;
        }
        y += ui.s(16.0);
    }
    ui.text("HABILIDADES", x, y, w * 0.6, 10.5, EMBER, true, Align::Left);
    link(ui, "Abrir la carpeta", y, ChatHit::SkillsFolder);
    let b = Area::new(x + w - ui.s(270.0), y - ui.s(3.0), ui.s(130.0), ui.s(22.0));
    let c = if env.hover == Some(ChatHit::SkillAdd) { HOT } else { EMBER };
    ui.text("Buscar e instalar…", b.x, b.y + ui.s(4.0), b.w, 11.0, c, true, Align::Right);
    ui.hit(b, Hit::Chat(ChatHit::SkillAdd));
    y += ui.s(24.0);
    let fit = ((bottom - y) / row).max(0.0) as usize;
    for (i, s) in v.skills.iter().enumerate() {
        if i + 1 == fit && v.skills.len() > fit {
            ui.text(format!("y {} más", v.skills.len() - i), x, y, w, 11.0, FAINT, false, Align::Left);
            break;
        }
        ui.text(format!("/{}", s.name), x, y, ui.s(150.0), 12.5, TEXT, true, Align::Left);
        let from = match s.source {
            Source::User => "tuya",
            Source::Project => "del proyecto",
            Source::Builtin => "de serie",
        };
        ui.text(&s.description, x + ui.s(156.0), y, w - ui.s(250.0), 12.0, DIM, false, Align::Left);
        ui.text(from, x, y, w, 11.0, FAINT, false, Align::Right);
        y += row;
    }
    let hint = "Escribe / en el chat para usar una, o pide «guarda esto como habilidad».";
    ui.text(hint, x, a.bottom() - ui.s(42.0), w - ui.s(120.0), 12.0, FAINT, false, Align::Left);
    let b = Area::new(x + w - ui.s(100.0), a.bottom() - ui.s(50.0), ui.s(100.0), ui.s(34.0));
    let hov = env.hover == Some(ChatHit::FormCancel);
    ui.panel(b, if hov { ui::ACTIVE_BG } else { ui::HOVER_BG }, ui::BORDER, 10.0, 1.0);
    ui.text("Cerrar", b.x, b.y + ui.s(7.5), b.w, 12.5, TEXT, true, Align::Center);
    ui.hit(b, Hit::Chat(ChatHit::FormCancel));
    ui.hit(a, Hit::Chat(ChatHit::Inside));
}

struct Row<'a> {
    icon: &'a str,
    color: Rgb,
    title: String,
    sub: String,
    on: bool,
    hit: Option<ChatHit>,
}

fn menu_row(ui: &mut Ui, a: Area, r: &Row, env: &Env) {
    let hov = r.hit.is_some() && env.hover == r.hit;
    if hov {
        ui.round(a, ui::ACTIVE_BG, 1.0, 8.0);
    }
    ui.text(r.icon, a.x + ui.s(12.0), a.y + ui.s(8.0), ui.s(18.0), 12.0, r.color, true, Align::Left);
    let tc = if r.hit.is_some() { TEXT } else { DIM };
    ui.text(
        &r.title,
        a.x + ui.s(34.0),
        a.y + ui.s(7.0),
        if r.sub.is_empty() { a.w - ui.s(46.0) } else { a.w * 0.62 },
        13.0,
        tc,
        r.on,
        Align::Left,
    );
    ui.text(
        &r.sub,
        a.x + a.w * 0.5,
        a.y + ui.s(8.0),
        a.w * 0.5 - ui.s(14.0),
        11.5,
        FAINT,
        false,
        Align::Right,
    );
    if let Some(h) = r.hit {
        ui.hit(a, Hit::Chat(h));
    }
}

fn models_menu(ui: &mut Ui, screen: Area, x: f32, bottom: f32, chats: &Chats, env: &Env) {
    let cur = chats.model().map(|(s, m)| (s.url.clone(), m.id.clone()));
    let (rh, hh) = (ui.s(34.0), ui.s(30.0));
    let mut rows: Vec<(Option<usize>, Row)> = Vec::new();
    for (i, s) in chats.servers.iter().enumerate() {
        rows.push((
            Some(i),
            Row {
                icon: "",
                color: DIM,
                title: s.server.name.clone(),
                sub: String::new(),
                on: true,
                hit: None,
            },
        ));
        match &s.status {
            Status::Checking => rows.push((
                None,
                Row {
                    icon: "◐",
                    color: HOT,
                    title: "Conectando…".into(),
                    sub: String::new(),
                    on: false,
                    hit: None,
                },
            )),
            Status::Down(e) => rows.push((
                None,
                Row {
                    icon: "✕", color: ui::RED, title: e.clone(), sub: String::new(), on: false, hit: None
                },
            )),
            Status::Ready(ms) => {
                for (j, m) in ms.iter().enumerate().take(12) {
                    let on = cur.as_ref().is_some_and(|(u, id)| *u == s.server.url && *id == m.id);
                    let mut sub =
                        m.ctx.map(|c| format!("{} de contexto", ui::tok(c as u64))).unwrap_or_default();
                    if m.tools == Some(false) {
                        sub = "sin herramientas".into();
                    }
                    let hit = Some(ChatHit::PickModel(i as u16, j as u16));
                    let icon = if on { "✓" } else { "◆" };
                    rows.push((
                        None,
                        Row {
                            icon,
                            color: if on { ui::GREEN } else { EMBER },
                            title: m.name.clone(),
                            sub,
                            on,
                            hit,
                        },
                    ));
                }
            }
        }
    }
    rows.push((
        None,
        Row {
            icon: "＋",
            color: EMBER,
            title: "Añadir un servidor…".into(),
            sub: "llama.cpp, Strata…".into(),
            on: false,
            hit: Some(ChatHit::AddServer),
        },
    ));
    if env.claude {
        rows.push((
            None,
            Row {
                icon: "✦",
                color: EMBER,
                title: "Abrir Claude Code".into(),
                sub: "en una terminal de Karbyx".into(),
                on: false,
                hit: Some(ChatHit::PickClaude),
            },
        ));
    }
    let h: f32 = rows.iter().map(|(s, _)| if s.is_some() { hh } else { rh }).sum::<f32>() + ui.s(12.0);
    let w = ui.s(460.0).min(screen.w - ui.s(40.0));
    let a = popup_box(ui, x, bottom, w, h);
    let mut y = a.y + ui.s(6.0);
    for (server, r) in &rows {
        let b = Area::new(a.x + ui.s(6.0), y, a.w - ui.s(12.0), if server.is_some() { hh } else { rh });
        match server {
            Some(i) => {
                ui.text(
                    r.title.to_uppercase(),
                    b.x + ui.s(12.0),
                    b.y + ui.s(9.0),
                    b.w * 0.5,
                    10.5,
                    DIM,
                    true,
                    Align::Left,
                );
                let e = Area::new(b.right() - ui.s(66.0), b.y + ui.s(4.0), ui.s(60.0), ui.s(22.0));
                let hit = ChatHit::EditServer(*i as u16);
                let hov = env.hover == Some(hit);
                ui.text(
                    "Editar",
                    e.x,
                    e.y + ui.s(4.0),
                    e.w - ui.s(8.0),
                    11.0,
                    if hov { EMBER } else { FAINT },
                    false,
                    Align::Right,
                );
                ui.hit(e, Hit::Chat(hit));
            }
            None => menu_row(ui, b, r, env),
        }
        y += b.h;
    }
    ui.hit(a, Hit::Chat(ChatHit::Inside));
}

fn modes_menu(ui: &mut Ui, screen: Area, x: f32, bottom: f32, chats: &Chats, sure: bool, env: &Env) {
    let cur = chats.current().map_or(Mode::Ask, |c| c.mode());
    let full = if sure {
        "Pulsa otra vez para activarlo."
    } else {
        "No pregunta nada: fuera del sandbox, fuera de la carpeta, MCP, claves y memoria."
    };
    let rows = [
        (Mode::Ask, "?", EMBER, "Pregunta antes", "Lee la carpeta. Para editar o ejecutar, te pregunta."),
        (
            Mode::Auto,
            "»",
            HOT,
            "Automático",
            "Edita en la carpeta y ejecuta en el sandbox. Lo demás, lo pregunta.",
        ),
        (Mode::Full, "⚠", ui::RED, if sure { "¿Seguro?" } else { "Total" }, full),
    ];
    let rh = ui.s(52.0);
    let foot = ui.s(if sure { 62.0 } else { 34.0 });
    let w = ui.s(520.0).min(screen.w - ui.s(40.0));
    let a = popup_box(ui, x, bottom, w, rh * rows.len() as f32 + foot + ui.s(12.0));
    for (i, (m, icon, color, title, sub)) in rows.into_iter().enumerate() {
        let b = Area::new(a.x + ui.s(6.0), a.y + ui.s(6.0) + rh * i as f32, a.w - ui.s(12.0), rh);
        let hit = ChatHit::PickMode(m);
        let warn = sure && m == Mode::Full;
        if warn {
            ui.panel(b, [0x33, 0x1a, 0x17], ui::RED, 8.0, 1.0);
        } else if env.hover == Some(hit) {
            ui.round(b, ui::ACTIVE_BG, 1.0, 8.0);
        }
        let on = cur == m;
        let (icon, color) = if on { ("✓", ui::GREEN) } else { (icon, color) };
        ui.text(icon, b.x + ui.s(12.0), b.y + ui.s(9.0), ui.s(18.0), 12.0, color, true, Align::Left);
        let tc = if m == Mode::Full { ui::RED } else { TEXT };
        let tx = b.x + ui.s(34.0);
        ui.text(title, tx, b.y + ui.s(8.0), b.w - ui.s(46.0), 13.0, tc, on || warn, Align::Left);
        ui.text(sub, tx, b.y + ui.s(28.0), b.w - ui.s(46.0), 11.5, DIM, false, Align::Left);
        ui.hit(b, Hit::Chat(hit));
    }
    let y = a.bottom() - foot + ui.s(4.0);
    let lines: &[(&str, Rgb)] = if sure {
        &[
            ("Puede borrar archivos, leer tus claves y enviar lo que lea fuera de tu Mac.", HOT),
            ("Una página, un archivo o una herramienta pueden engañar al modelo.", HOT),
        ]
    } else {
        &[(
            "Vale para esta conversación, hasta que cierres Karbyx.  Mayús+Tab alterna los dos primeros.",
            FAINT,
        )]
    };
    for (i, (t, c)) in lines.iter().enumerate() {
        let y = y + ui.s(20.0) * i as f32 + ui.s(2.0);
        ui.text(*t, a.x + ui.s(18.0), y, a.w - ui.s(36.0), 11.5, *c, false, Align::Left);
    }
    ui.hit(a, Hit::Chat(ChatHit::Inside));
}

fn projects_menu(ui: &mut Ui, screen: Area, x: f32, bottom: f32, chats: &Chats, env: &Env) {
    let cur = chats.current().and_then(|c| c.root.clone());
    let mut rows: Vec<Row> = env
        .projects
        .iter()
        .enumerate()
        .map(|(i, p)| Row {
            icon: if cur.as_ref() == Some(p) { "✓" } else { "⌂" },
            color: if cur.as_ref() == Some(p) { ui::GREEN } else { EMBER },
            title: folder_name(p),
            sub: karbyx_term::proc::tilde(p.parent().unwrap_or(p)),
            on: cur.as_ref() == Some(p),
            hit: Some(ChatHit::PickProject(i as u16)),
        })
        .collect();
    rows.push(Row {
        icon: "…",
        color: EMBER,
        title: "Elegir otra carpeta…".into(),
        sub: String::new(),
        on: false,
        hit: Some(ChatHit::BrowseProject),
    });
    rows.push(Row {
        icon: "○",
        color: DIM,
        title: "Sin carpeta (solo hablar)".into(),
        sub: String::new(),
        on: cur.is_none(),
        hit: Some(ChatHit::PickNoProject),
    });
    let rh = ui.s(34.0);
    let w = ui.s(440.0).min(screen.w - ui.s(40.0));
    let a = popup_box(ui, x, bottom, w, rh * rows.len() as f32 + ui.s(12.0));
    for (i, r) in rows.iter().enumerate() {
        let b = Area::new(a.x + ui.s(6.0), a.y + ui.s(6.0) + rh * i as f32, a.w - ui.s(12.0), rh);
        menu_row(ui, b, r, env);
    }
    ui.hit(a, Hit::Chat(ChatHit::Inside));
}

fn form(ui: &mut Ui, screen: Area, chats: &Chats, f: &Form, env: &Env) {
    ui.tint(screen, [0, 0, 0], 0.55);
    let editing = f.edit.is_some();
    let w = ui.s(560.0).min(screen.w - ui.s(40.0));
    let h = if editing { ui.s(250.0) } else { ui.s(466.0) };
    let a =
        Area::new(screen.x + (screen.w - w) / 2.0, screen.y + ((screen.h - h) / 2.0).max(ui.s(10.0)), w, h);
    ui.glow(a, [0, 0, 0], 0.7, 16.0, 40.0);
    ui.panel(a, [0x1d, 0x19, 0x16], ui::BORDER, 16.0, 1.0);
    let pad = ui.s(26.0);
    let (x, w) = (a.x + pad, a.w - 2.0 * pad);
    let mut y = a.y + ui.s(22.0);
    let title = if editing { "Editar servidor" } else { "Conectar mi modelo" };
    ui.text(title, x, y, w, 19.0, TEXT, true, Align::Left);
    y += ui.s(42.0);
    if !editing {
        ui.text(
            "1 · Arranca el modelo, en este Mac o en un PC de tu red, con una de estas órdenes:",
            x,
            y,
            w,
            12.5,
            DIM,
            false,
            Align::Left,
        );
        y += ui.s(26.0);
        let key = if f.key.is_empty() { f.suggested.as_str() } else { f.key.text() };
        for (i, name) in ["llama.cpp", "Strata"].iter().enumerate() {
            let b = Area::new(x, y, w, ui.s(36.0));
            let hit = ChatHit::CopyCommand(i as u8);
            let hov = env.hover == Some(hit);
            ui.panel(b, if hov { ui::ACTIVE_BG } else { CODE_BG }, if hov { EMBER } else { LINE }, 9.0, 1.0);
            ui.text(*name, b.x + ui.s(12.0), b.y + ui.s(9.0), ui.s(70.0), 12.0, EMBER, true, Align::Left);
            ui.text(
                super::server_command(i as u8, key),
                b.x + ui.s(84.0),
                b.y + ui.s(10.0),
                b.w - ui.s(160.0),
                11.5,
                DIM,
                false,
                Align::Left,
            );
            if let Some(l) = ui.labels.last_mut() {
                l.font = Font::Mono;
            }
            ui.text(
                "Copiar",
                b.x,
                b.y + ui.s(9.0),
                b.w - ui.s(14.0),
                11.5,
                if hov { TEXT } else { FAINT },
                true,
                Align::Right,
            );
            ui.hit(b, Hit::Chat(hit));
            y += b.h + ui.s(8.0);
        }
        ui.text(
            "Si Windows pregunta por el cortafuegos, permite solo las redes privadas.",
            x,
            y,
            w,
            11.5,
            FAINT,
            false,
            Align::Left,
        );
        y += ui.s(32.0);
        ui.text("2 · Dime dónde está:", x, y, w, 12.5, DIM, false, Align::Left);
        y += ui.s(26.0);
    }
    let fields = [
        ("Dirección", &f.url, "127.0.0.1:8080 en este Mac, o la IP del PC", false),
        ("Clave", &f.key, "la de --api-key (si pusiste una)", true),
    ];
    for (i, (name, ed, hint, secret)) in fields.into_iter().enumerate() {
        let b = Area::new(x, y, w, ui.s(40.0));
        let on = f.focus == i as u8;
        if on {
            ui.glow(b, EMBER, 0.06, 10.0, 10.0);
        }
        ui.panel(b, [0x13, 0x11, 0x0f], if on { EMBER } else { ui::BORDER }, 10.0, 1.0);
        ui.text(name, b.x + ui.s(14.0), b.y + ui.s(10.5), ui.s(80.0), 12.5, DIM, true, Align::Left);
        let tx = b.x + ui.s(96.0);
        let tw = b.w - ui.s(110.0);
        if ed.is_empty() {
            let t = if on { format!("▏{hint}") } else { hint.to_string() };
            ui.text(t, tx, b.y + ui.s(9.5), tw, 13.5, FAINT, false, Align::Left);
        } else {
            let mask = |s: &str| if secret { "•".repeat(s.chars().count()) } else { s.to_string() };
            let (l, r) = ed.text().split_at(ed.caret());
            let l = mask(l);
            // Si no cabe, se ve lo que se está escribiendo.
            let fit = (tw / ui.s(7.6)) as usize;
            let n = l.chars().count();
            let l: String = if n > fit { l.chars().skip(n - fit).collect() } else { l };
            let t = if on { format!("{l}▏{}", mask(r)) } else { format!("{l}{}", mask(r)) };
            if ed.selection().is_some() && on {
                ui.round(Area::new(tx - ui.s(3.0), b.y + ui.s(8.0), tw, b.h - ui.s(16.0)), SELECT, 0.2, 4.0);
            }
            ui.text(t, tx, b.y + ui.s(9.5), tw, 13.5, TEXT, false, Align::Left);
        }
        ui.hit(b, Hit::Chat(ChatHit::Field(i as u8)));
        y += b.h + ui.s(10.0);
    }
    let (msg, color) = match &f.status {
        None => ("", DIM),
        Some(Ok(())) => ("Conectando…", HOT),
        Some(Err(e)) => (e.as_str(), ui::RED),
    };
    // El mensaje puede ser largo: dos líneas.
    for (i, l) in wrap(msg, 78).iter().take(2).enumerate() {
        ui.text(l, x, y + ui.s(17.0) * i as f32, w, 12.0, color, false, Align::Left);
    }
    let by = a.bottom() - ui.s(58.0);
    let button = |ui: &mut Ui, t: &str, right: f32, bw: f32, hit: ChatHit, primary: bool| {
        let b = Area::new(right - bw, by, bw, ui.s(36.0));
        let hov = env.hover == Some(hit);
        if primary {
            ui.gradient(b, (if hov { HOT } else { [0xf6, 0x9a, 0x55] }, 1.0), (EMBER_DEEP, 1.0), 10.0);
        } else {
            ui.panel(b, if hov { ui::ACTIVE_BG } else { ui::HOVER_BG }, ui::BORDER, 10.0, 1.0);
        }
        let c = if primary { ON_EMBER } else { TEXT };
        ui.text(t, b.x, b.y + ui.s(8.0), b.w, 13.0, c, true, Align::Center);
        ui.hit(b, Hit::Chat(hit));
        b.x
    };
    let r = button(ui, "Conectar  ↵", x + w, ui.s(130.0), ChatHit::FormOk, true);
    let r = button(ui, "Cancelar", r - ui.s(8.0), ui.s(100.0), ChatHit::FormCancel, false);
    if let Some(i) = f.edit.filter(|i| *i < chats.servers.len()) {
        button(ui, "Quitar", r - ui.s(8.0), ui.s(90.0), ChatHit::RemoveServer(i as u16), false);
    }
    ui.hit(a, Hit::Chat(ChatHit::Inside));
}

pub(super) fn wrap(s: &str, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in s.split_whitespace() {
        match out.last_mut() {
            Some(l) if l.chars().count() + w.chars().count() < max => {
                l.push(' ');
                l.push_str(w);
            }
            _ => out.push(w.to_string()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::md::INK;
    use super::*;

    #[test]
    fn date_and_copy_go_under_messages_and_answers() {
        let it = |b: Body, at: Option<i64>| Item { at, ..Item::new(b) };
        let user = |at| it(Body::User("hola".into()), Some(at));
        let said = |at| it(Body::Assistant("vale".into()), Some(at));
        let note = || it(Body::Notice("·".into()), None);
        let items = vec![user(1_000), said(2_000), note(), said(13_500), user(20_000), said(21_000)];
        assert_eq!(footers(&items, false), [true, false, false, true, true, true]);
        // Mientras trabaja, la respuesta en curso aún no lo lleva.
        assert_eq!(footers(&items, true), [true, false, false, true, true, false]);
        assert!(foot_text(&items, 3).ends_with(" · 12,5 s"), "{}", foot_text(&items, 3));
        assert!(!foot_text(&items, 0).contains('·'));
        assert_eq!(foot_text(&[note()], 0), "");
        let d = when(1_764_158_400_000);
        assert!(d.contains(" nov 2025, "), "{d}");
    }

    #[test]
    fn buttons_follow_the_scroll() {
        // A la vista: en su sitio.
        assert_eq!(stick(300.0, 100.0, 900.0), 300.0);
        // Se ha ido por arriba: pegado arriba.
        assert_eq!(stick(-200.0, 100.0, 900.0), 100.0);
        // Al acabarse el bloque, se va con él.
        assert_eq!(stick(-200.0, 100.0, 40.0), 40.0);
    }

    #[test]
    fn inline_code_ranges() {
        let s = vec![span("a\nñ ", INK), mono("xy", INK), span(" z", INK)];
        assert_eq!(code_ranges(&s), [((1, 3), (1, 5))]);
    }

    #[test]
    fn groups_by_day() {
        let now = Local::now();
        let at = |days: i64| (now - chrono::Duration::days(days)).timestamp();
        let g: Vec<_> = [0, 1, 3, 20, 90].iter().map(|d| day_group(at(*d), now.date_naive())).collect();
        assert_eq!(g, ["Hoy", "Ayer", "Esta semana", "Este mes", "Antes"]);
    }

    #[test]
    fn wraps_messages() {
        assert_eq!(wrap("uno dos tres", 8), ["uno dos", "tres"]);
        assert!(wrap("", 8).is_empty());
    }
}

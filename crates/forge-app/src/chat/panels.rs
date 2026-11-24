// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Cómo se ven los paneles de servidores MCP y de instalar habilidades.

use forge_agent::know::Know;
use forge_agent::oauth::{LOGIN, signed_in};
use forge_agent::registry::Entry;

use super::draw::{Env, primary, secondary, window, wrap};
use super::edit::Editor;
use super::extend::{McpForm, McpPanel, McpSearch, McpStatus, SkillForm, mcp_label, skill_exists};
use super::{ChatHit, Chats};
use crate::ui::{self, Align, Area, DIM, EMBER, Font, HOT, Hit, Rgb, TEXT, Ui};

const CARD: Rgb = [0x1a, 0x17, 0x14];
const LINE: Rgb = [0x2a, 0x25, 0x21];
const FAINT: Rgb = [0x6a, 0x60, 0x57];
const FIELD: Rgb = [0x13, 0x11, 0x0f];

fn mono(ui: &mut Ui, t: impl Into<String>, x: f32, y: f32, w: f32, pt: f32, color: Rgb) {
    ui.text(t, x, y, w, pt, color, false, Align::Left);
    if let Some(l) = ui.labels.last_mut() {
        l.font = Font::Mono;
    }
}

/// Botón pequeño; devuelve su borde izquierdo.
fn chip(ui: &mut Ui, text: &str, right: f32, y: f32, color: Rgb, hit: ChatHit, env: &Env) -> f32 {
    let w = ui.s(20.0) + text.chars().count() as f32 * ui.s(6.6);
    let b = Area::new(right - w, y, w, ui.s(24.0));
    ui.round(b, color, if env.hover == Some(hit) { 0.28 } else { 0.13 }, 7.0);
    ui.text(text, b.x, b.y + ui.s(5.0), b.w, 11.0, color, true, Align::Center);
    ui.hit(b, Hit::Chat(hit));
    b.x
}

/// Campo de una línea.
fn field(ui: &mut Ui, b: Area, ed: &Editor, hint: &str, on: bool, secret: bool) {
    if on {
        ui.glow(b, EMBER, 0.06, 10.0, 10.0);
    }
    ui.panel(b, FIELD, if on { EMBER } else { ui::BORDER }, 10.0, 1.0);
    let bar = if on { "▏" } else { "" };
    let mask = |s: &str| if secret { "•".repeat(s.chars().count()) } else { s.to_string() };
    let (tx, tw) = (b.x + ui.s(14.0), b.w - ui.s(28.0));
    if ed.is_empty() {
        ui.text(format!("{bar}{hint}"), tx, b.y + ui.s(9.5), tw, 13.0, FAINT, false, Align::Left);
        return;
    }
    let (l, r) = ed.text().split_at(ed.caret());
    let (l, r) = (mask(l), mask(r));
    let fit = (tw / ui.s(7.4)) as usize;
    let n = l.chars().count();
    let l: String = if n > fit { l.chars().skip(n - fit).collect() } else { l };
    if on && ed.selection().is_some() {
        ui.round(Area::new(tx - ui.s(3.0), b.y + ui.s(8.0), tw, b.h - ui.s(16.0)), EMBER, 0.2, 4.0);
    }
    ui.text(format!("{l}{bar}{r}"), tx, b.y + ui.s(9.5), tw, 13.0, TEXT, false, Align::Left);
}

/// Servidores MCP.
pub fn mcp_panel(ui: &mut Ui, screen: Area, chats: &Chats, p: &McpPanel, env: &Env) -> f32 {
    let project = chats.current().and_then(|c| c.know().project);
    let about =
        "Herramientas de otros programas para tu modelo: en tu Mac (fuera del sandbox) o en internet.";
    let (x, w, top, a) = window(ui, screen, 760.0, 600.0, "Servidores MCP", about);
    let bottom = a.bottom() - ui.s(84.0);
    let view = Area::new(x - ui.s(4.0), top, w + ui.s(8.0), bottom - top);
    let mut y = top - p.scroll;
    ui.clip = Some(view);
    if let Some(e) = &chats.mcp_error {
        ui.text(e, x, y, w, 12.5, ui::RED, false, Align::Left);
        y += ui.s(30.0);
    } else if chats.mcp.is_empty() {
        for t in [
            "Todavía no hay ninguno.",
            "Busca uno por su nombre, o añádelo a mano con la configuración que den en su web,",
            "la orden con la que se lanza o su dirección.",
        ] {
            ui.text(t, x, y, w, 12.5, FAINT, false, Align::Left);
            y += ui.s(24.0);
        }
    }
    for (i, v) in chats.mcp.iter().enumerate() {
        let n = i as u16;
        let state = chats.saved.mcp.get(&v.name);
        let approved = state.is_some_and(|s| s.approved == v.config.fingerprint());
        let tools = match &v.status {
            McpStatus::Ready(t) => t.as_slice(),
            _ => &[],
        };
        let open = p.open == Some(i) && approved;
        let row = ui.s(28.0);
        let h = ui.s(66.0) + if open { row * tools.len() as f32 + ui.s(12.0) } else { 0.0 };
        let b = Area::new(x, y, w, h);
        // Solo se pinta lo que se ve: los rectángulos no se recortan solos.
        let c = b.intersect(view);
        if c.h > 0.0 {
            ui.panel(c, CARD, LINE, 11.0, 1.0);
        }
        let on = approved && state.is_some_and(|s| s.on_in(project.as_deref()));
        let (dot, sub, sc) = match &v.status {
            _ if !approved => (HOT, "Nuevo o cambiado: revisa la orden antes de aprobarlo".to_string(), HOT),
            McpStatus::Starting => (HOT, "Arrancando…".into(), HOT),
            McpStatus::SigningIn => (HOT, "Inicia sesión en el navegador…".into(), HOT),
            McpStatus::Failed(e) if e == LOGIN => (HOT, "Necesita que inicies sesión".into(), HOT),
            McpStatus::Failed(e) if v.config.url.is_some() => (ui::RED, format!("No conecta: {e}"), ui::RED),
            McpStatus::Failed(e) => (ui::RED, format!("No arranca: {e}"), ui::RED),
            McpStatus::Ready(t) => {
                let used = t.iter().filter(|t| !state.is_some_and(|s| s.off.contains(&t.name)));
                let (mut k, mut cost) = (0, 0);
                for t in used {
                    k += 1;
                    cost += t.cost(&v.name);
                }
                let many = cost > 4_000;
                let what = if k == 1 { "herramienta" } else { "herramientas" };
                let s = format!("{k} {what} · ≈{} tokens de contexto", ui::tok(cost as u64));
                (if on { ui::GREEN } else { FAINT }, s, if many { HOT } else { FAINT })
            }
            McpStatus::Unknown => (FAINT, if on { "Activo".into() } else { String::new() }, FAINT),
        };
        let inside = |ui: &mut Ui, r: Area, color: Rgb| {
            if r.y >= view.y && r.bottom() <= view.bottom() {
                ui.round(r, color, 1.0, 4.0);
            }
        };
        inside(ui, Area::new(x + ui.s(15.0), y + ui.s(15.0), ui.s(8.0), ui.s(8.0)), dot);
        ui.text(&v.name, x + ui.s(34.0), y + ui.s(9.0), ui.s(200.0), 13.5, TEXT, true, Align::Left);
        let mut right = x + w - ui.s(12.0);
        let by = y + ui.s(20.0);
        if by >= view.y && by + ui.s(24.0) <= view.bottom() {
            let sure = p.confirm == Some(i);
            let t = if sure { "¿Quitar?" } else { "×" };
            right = chip(ui, t, right, by, if sure { ui::RED } else { DIM }, ChatHit::McpRemove(n), env)
                - ui.s(6.0);
            if !approved {
                right = chip(ui, "Revisar y aprobar", right, by, HOT, ChatHit::McpApprove(n), env);
            } else {
                let label = mcp_label(state, project.as_deref());
                let color = if on { ui::GREEN } else { DIM };
                right = chip(ui, &label, right, by, color, ChatHit::McpState(n), env) - ui.s(6.0);
                let login = v.config.url.is_some()
                    && matches!(&v.status, McpStatus::SigningIn | McpStatus::Failed(_))
                    && (v.status == McpStatus::SigningIn || v.status == McpStatus::Failed(LOGIN.into()));
                let inside = v.config.url.as_deref().is_some_and(signed_in);
                if login {
                    let t = if v.status == McpStatus::SigningIn { "Otra vez" } else { "Iniciar sesión" };
                    right = chip(ui, t, right, by, HOT, ChatHit::McpSignIn(n), env) - ui.s(6.0);
                    right = chip(ui, "Poner clave", right, by, EMBER, ChatHit::McpKey(n), env);
                } else if v.config.url.is_some() && matches!(v.status, McpStatus::Failed(_)) {
                    // Un servidor remoto que falla suele necesitar (otra) clave.
                    right = chip(ui, "Poner clave", right, by, EMBER, ChatHit::McpKey(n), env) - ui.s(6.0);
                    right = chip(ui, "Reintentar", right, by, DIM, ChatHit::McpOpen(n), env);
                } else {
                    let t = if open { "Plegar" } else { "Herramientas" };
                    right = chip(ui, t, right, by, EMBER, ChatHit::McpOpen(n), env);
                    if inside && open {
                        right = chip(
                            ui,
                            "Cerrar sesión",
                            right - ui.s(6.0),
                            by,
                            DIM,
                            ChatHit::McpSignOut(n),
                            env,
                        );
                    }
                }
            }
        }
        // Lo que dejen libre los botones.
        let room = right - x - ui.s(44.0);
        mono(ui, v.config.line(), x + ui.s(34.0), y + ui.s(29.0), room, 10.5, DIM);
        ui.text(sub, x + ui.s(34.0), y + ui.s(45.0), room, 11.0, sc, false, Align::Left);
        if open && let Some(s) = state {
            let mut ty = y + ui.s(66.0);
            for (j, t) in tools.iter().enumerate() {
                let off = s.off.contains(&t.name);
                if ty >= view.y - row && ty + row <= view.bottom() + row {
                    let hit = ChatHit::McpTool(n, j as u16);
                    let hov = env.hover == Some(hit);
                    let mark = if off { "○" } else { "●" };
                    let mc = if off { FAINT } else { ui::GREEN };
                    ui.text(mark, x + ui.s(34.0), ty + ui.s(5.0), ui.s(16.0), 11.0, mc, false, Align::Left);
                    let nc = if hov {
                        EMBER
                    } else if off {
                        FAINT
                    } else {
                        TEXT
                    };
                    mono(ui, &t.name, x + ui.s(52.0), ty + ui.s(5.0), ui.s(190.0), 11.5, nc);
                    let d = t.description.split_whitespace().collect::<Vec<_>>().join(" ");
                    let dx = x + ui.s(250.0);
                    ui.text(d, dx, ty + ui.s(5.0), x + w - ui.s(130.0) - dx, 11.5, FAINT, false, Align::Left);
                    ui.hit(Area::new(x + ui.s(28.0), ty, w - ui.s(160.0), row), Hit::Chat(hit));
                    if !off && ty >= view.y && ty + row <= view.bottom() {
                        let trusted = s.trusted.contains(&t.name);
                        let (l, c) = if trusted { ("Sin preguntar", HOT) } else { ("Pregunta", DIM) };
                        chip(
                            ui,
                            l,
                            x + w - ui.s(12.0),
                            ty + ui.s(2.0),
                            c,
                            ChatHit::McpTrust(n, j as u16),
                            env,
                        );
                    }
                }
                ty += row;
            }
        }
        y += h + ui.s(10.0);
    }
    ui.clip = None;
    let max = (y + p.scroll - top - view.h).max(0.0);
    let by = a.bottom() - ui.s(52.0);
    let r = primary(ui, "Buscar…", x + w, by, ui.s(110.0), ChatHit::McpSearch, env) - ui.s(8.0);
    let r = secondary(ui, "Añadir a mano…", r, by, ui.s(136.0), ChatHit::McpAdd, env) - ui.s(8.0);
    let r = secondary(ui, "Traer del proyecto", r, by, ui.s(150.0), ChatHit::McpImport, env) - ui.s(8.0);
    secondary(ui, "Cerrar", r, by, ui.s(90.0), ChatHit::FormCancel, env);
    let hint = "Cuantas más herramientas, menos contexto para tu tarea.";
    ui.text(hint, x, by - ui.s(24.0), w, 11.5, FAINT, false, Align::Left);
    ui.hit(a, Hit::Chat(ChatHit::Inside));
    max
}

/// Añadir servidores.
pub fn mcp_form(ui: &mut Ui, screen: Area, chats: &Chats, f: &McpForm, env: &Env) {
    let about = if f.fixed {
        "Esto es lo que se va a usar. Revísalo antes de aceptar."
    } else {
        "Pega el JSON de «mcpServers», la orden con la que se lanza o su dirección. Revísalo antes de aceptar."
    };
    let (x, w, mut y, a) = window(ui, screen, 760.0, 600.0, "Añadir servidor MCP", about);
    let wants = f.wants_key();
    if !f.fixed {
        let b = Area::new(x, y, w, ui.s(40.0));
        let hint = "⌘V para pegar, npx -y paquete, o https://…";
        field(ui, b, &f.text, hint, !(f.on_key && wants), false);
        ui.hit(b, Hit::Chat(ChatHit::McpField(false)));
        y += ui.s(50.0);
    }
    if wants {
        let b = Area::new(x, y, w, ui.s(40.0));
        field(ui, b, &f.key, "Clave, si la pide (opcional)", f.on_key, true);
        ui.hit(b, Hit::Chat(ChatHit::McpField(true)));
        y += ui.s(50.0);
        let t = "Si el servicio no deja iniciar sesión desde aquí, créale una clave en su web y pégala.";
        ui.text(t, x, y - ui.s(2.0), w, 11.5, FAINT, false, Align::Left);
        y += ui.s(26.0);
    }
    if let Some(e) = &f.error {
        ui.text(e, x, y, w, 12.5, ui::RED, false, Align::Left);
        y += ui.s(26.0);
    }
    // La orden, entera: es lo que se aprueba.
    let cols = (((w - ui.s(28.0)) / ui.s(6.95)) as usize).max(20);
    for (name, c) in &f.parsed {
        let lines = wrap_mono(&c.line(), cols);
        let env_lines = wrap_mono(&extra_text(c), cols);
        let h = ui.s(46.0) + ui.s(17.0) * (lines.len() + env_lines.len()) as f32;
        ui.panel(Area::new(x, y, w, h), CARD, LINE, 10.0, 1.0);
        ui.text(name, x + ui.s(14.0), y + ui.s(8.0), w * 0.5, 13.5, TEXT, true, Align::Left);
        if chats.mcp.iter().any(|v| v.name == *name) {
            let t = "sustituye al que ya tienes";
            ui.text(t, x, y + ui.s(9.0), w - ui.s(14.0), 11.0, HOT, false, Align::Right);
        }
        let mut ly = y + ui.s(31.0);
        for l in lines {
            mono(ui, l, x + ui.s(14.0), ly, w - ui.s(28.0), 11.0, EMBER);
            ly += ui.s(17.0);
        }
        for l in env_lines {
            mono(ui, l, x + ui.s(14.0), ly, w - ui.s(28.0), 11.0, DIM);
            ly += ui.s(17.0);
        }
        y += h + ui.s(10.0);
    }
    let by = a.bottom() - ui.s(52.0);
    let local = f.parsed.iter().any(|(_, c)| c.url.is_none());
    let hosts: Vec<String> = f.parsed.iter().filter_map(|(_, c)| c.host()).collect();
    let plain = f.parsed.iter().any(|(_, c)| c.url.as_deref().is_some_and(|u| u.starts_with("http://")));
    let mut warn = Vec::new();
    if local && !f.parsed.is_empty() {
        warn.push(
            "⚠ Es un programa: se ejecutará en tu Mac, fuera del sandbox y con tus permisos.".to_string(),
        );
    }
    if !hosts.is_empty() {
        warn.push(format!("⚠ Lo que el modelo le mande saldrá de tu Mac hacia {}.", hosts.join(", ")));
    }
    if plain {
        warn.push("⚠ La conexión va sin cifrar (http): úsala solo en tu red.".into());
    }
    let top = by - ui.s(12.0) - ui.s(18.0) * warn.len() as f32;
    for (i, t) in warn.iter().enumerate() {
        ui.text(t, x, top + ui.s(18.0) * i as f32, w, 12.0, HOT, false, Align::Left);
    }
    let r = primary(ui, "Añadir  ↵", x + w, by, ui.s(176.0), ChatHit::McpAddOk, env) - ui.s(8.0);
    secondary(ui, "Volver", r, by, ui.s(90.0), ChatHit::ExtendBack, env);
    ui.hit(a, Hit::Chat(ChatHit::Inside));
}

/// Buscador de servidores. Devuelve cuánto se puede desplazar.
pub fn mcp_search(ui: &mut Ui, screen: Area, f: &McpSearch, env: &Env) -> f32 {
    if let Some(e) = f.entry() {
        return mcp_detail(ui, screen, f, e, env);
    }
    let about = "En el registro oficial de MCP. Lo que escribas se envía a su web al pulsar ↵.";
    let (x, w, top, a) = window(ui, screen, 780.0, 640.0, "Buscar servidores MCP", about);
    let go = ui.s(100.0);
    let b = Area::new(x, top, w - go - ui.s(8.0), ui.s(40.0));
    field(ui, b, &f.query, "github, notion, postgres…", true, false);
    primary(ui, "Buscar ↵", x + w, top + ui.s(2.0), go, ChatHit::McpSearchGo, env);
    let list = top + ui.s(54.0);
    let by = a.bottom() - ui.s(52.0);
    let view = Area::new(x, list, w, by - ui.s(34.0) - list);
    let warn = "⚠ El registro comprueba quién publica, no que sea de fiar. Fíjate en el autor.";
    ui.text(warn, x, by - ui.s(24.0), w, 11.5, HOT, false, Align::Left);
    let msg = match (&f.error, &f.results, f.busy) {
        (_, _, true) => Some(("Buscando…".to_string(), HOT)),
        (Some(e), ..) => Some((e.clone(), ui::RED)),
        (_, Some(r), _) if r.is_empty() => Some(("No hay ninguno con ese nombre.".into(), DIM)),
        _ => None,
    };
    if let Some((t, c)) = msg {
        ui.text(t, x, list + ui.s(6.0), w, 12.5, c, false, Align::Left);
    }
    let row = ui.s(58.0);
    let mut y = list - f.scroll;
    ui.clip = Some(view);
    for (i, e) in f.results.iter().flatten().enumerate() {
        let r = Area::new(x, y, w, row);
        y += row;
        if r.bottom() < view.y || r.y > view.bottom() {
            continue;
        }
        let hit = ChatHit::McpFound(i as u16);
        if env.hover == Some(hit) {
            ui.round(r.intersect(view), ui::ACTIVE_BG, 1.0, 8.0);
        }
        let (px, ty) = (x + ui.s(12.0), r.y);
        ui.text(&e.title, px, ty + ui.s(6.0), w * 0.55, 13.5, TEXT, true, Align::Left);
        ui.text(&e.publisher, px, ty + ui.s(7.0), w - ui.s(24.0), 11.5, EMBER, false, Align::Right);
        ui.text(&e.about, px, ty + ui.s(26.0), w - ui.s(200.0), 11.5, DIM, false, Align::Left);
        let kinds: Vec<&str> = e.choices.iter().map(|c| c.label).collect();
        let t = if kinds.is_empty() { "forge no puede usarlo".into() } else { kinds.join(" · ") };
        ui.text(t, px, ty + ui.s(26.0), w - ui.s(24.0), 11.0, FAINT, false, Align::Right);
        ui.hit(r, Hit::Chat(hit));
    }
    ui.clip = None;
    secondary(ui, "Volver", x + w, by, ui.s(90.0), ChatHit::ExtendBack, env);
    ui.hit(a, Hit::Chat(ChatHit::Inside));
    (y + f.scroll - list - view.h).max(0.0)
}

/// Ficha de un servidor: quién es, cómo se conecta y qué pide.
fn mcp_detail(ui: &mut Ui, screen: Area, f: &McpSearch, e: &Entry, env: &Env) -> f32 {
    let about = format!("{} · publicado por {}", e.id, e.publisher);
    let (x, w, mut y, a) = window(ui, screen, 780.0, 640.0, &e.title, &about);
    for l in wrap(&e.about, 108).iter().take(3) {
        ui.text(l, x, y, w, 12.5, TEXT, false, Align::Left);
        y += ui.s(19.0);
    }
    if e.repo.is_some() {
        chip(ui, "Ver su código", x + w, a.y + ui.s(22.0), EMBER, ChatHit::McpRepo, env);
    }
    y += ui.s(12.0);
    let by = a.bottom() - ui.s(52.0);
    if e.choices.is_empty() {
        let t = "Se publica de una forma que forge todavía no admite.";
        ui.text(t, x, y, w, 12.5, HOT, false, Align::Left);
    }
    // Formas de conectarlo.
    if e.choices.len() > 1 {
        ui.text("Cómo:", x, y + ui.s(5.0), ui.s(60.0), 12.0, DIM, false, Align::Left);
        let mut cx = x + ui.s(56.0);
        for (i, c) in e.choices.iter().enumerate() {
            let cw = ui.s(20.0) + c.label.chars().count() as f32 * ui.s(6.6);
            let color = if i == f.choice { EMBER } else { DIM };
            chip(ui, c.label, cx + cw, y, color, ChatHit::McpChoice(i as u16), env);
            cx += cw + ui.s(8.0);
        }
        y += ui.s(36.0);
    }
    let view = Area::new(x, y, w, by - ui.s(32.0) - y);
    let mut max = 0.0;
    if let Some(c) = f.current() {
        let mut iy = y - f.scroll;
        ui.clip = Some(view);
        if c.inputs.is_empty() {
            ui.text("No pide ningún dato.", x, iy, w, 12.5, DIM, false, Align::Left);
            iy += ui.s(24.0);
        }
        for (i, inp) in c.inputs.iter().enumerate() {
            let star = if inp.required { " · obligatorio" } else { " · opcional" };
            ui.text(format!("{}{star}", inp.label), x, iy, w, 12.0, TEXT, true, Align::Left);
            iy += ui.s(19.0);
            for l in wrap(&inp.about, 112).iter().take(2) {
                ui.text(l, x, iy, w, 11.0, DIM, false, Align::Left);
                iy += ui.s(16.0);
            }
            let n = i as u16;
            if inp.choices.is_empty() {
                let b = Area::new(x, iy + ui.s(2.0), w, ui.s(36.0));
                // Los campos a medias no se pintan: los rectángulos no se recortan.
                if b.y >= view.y
                    && b.bottom() <= view.bottom()
                    && let Some(ed) = f.values.get(i)
                {
                    field(ui, b, ed, "", f.focus == i, inp.secret);
                    ui.hit(b, Hit::Chat(ChatHit::McpInput(n)));
                }
                iy += ui.s(50.0);
            } else {
                let cur = f.values.get(i).map(|e| e.text()).unwrap_or_default();
                let mut cx = x;
                for (j, o) in inp.choices.iter().enumerate() {
                    let cw = ui.s(20.0) + o.chars().count() as f32 * ui.s(6.6);
                    if cx + cw > x + w {
                        cx = x;
                        iy += ui.s(30.0);
                    }
                    if iy >= view.y && iy + ui.s(26.0) <= view.bottom() {
                        let color = if o == cur { EMBER } else { DIM };
                        chip(ui, o, cx + cw, iy + ui.s(2.0), color, ChatHit::McpOption(n, j as u16), env);
                    }
                    cx += cw + ui.s(6.0);
                }
                iy += ui.s(40.0);
            }
        }
        ui.clip = None;
        max = (iy + f.scroll - y - view.h).max(0.0);
        let (t, color) = match (&f.error, c.needs) {
            (Some(e), _) => (e.clone(), ui::RED),
            (None, Some(p)) if !f.installed(p) => {
                (format!("⚠ Necesita «{p}», que no está instalado en este Mac."), HOT)
            }
            (None, Some(p)) => {
                (format!("Se lanza con «{p}», que lo descarga y lo ejecuta en tu Mac."), FAINT)
            }
            (None, None) => ("Está en internet: lo que el modelo le mande sale de tu Mac.".into(), FAINT),
        };
        ui.text(t, x, by - ui.s(24.0), w, 12.0, color, false, Align::Left);
        let r = primary(ui, "Revisar  ↵", x + w, by, ui.s(120.0), ChatHit::McpReview, env) - ui.s(8.0);
        secondary(ui, "Volver", r, by, ui.s(90.0), ChatHit::ExtendBack, env);
    } else {
        secondary(ui, "Volver", x + w, by, ui.s(90.0), ChatHit::ExtendBack, env);
    }
    ui.hit(a, Hit::Chat(ChatHit::Inside));
    max
}

/// Instalar habilidades. Devuelve cuánto se puede desplazar.
pub fn skill_form(ui: &mut Ui, screen: Area, f: &SkillForm, know: &Know, env: &Env) -> f32 {
    let Some(b) = &f.bundle else {
        let about = "De un repositorio público de GitHub (dueño/nombre o su dirección) o de una carpeta.";
        let (x, w, mut y, a) = window(ui, screen, 640.0, 270.0, "Instalar habilidades", about);
        field(ui, Area::new(x, y, w, ui.s(40.0)), &f.url, "dueño/repositorio", true, false);
        y += ui.s(54.0);
        let (t, c) = match (&f.error, f.busy) {
            (_, true) => ("Buscando…".to_string(), HOT),
            (Some(e), _) => (e.clone(), ui::RED),
            _ => ("Primero las verás; nada se instala ni se ejecuta al buscar.".into(), FAINT),
        };
        ui.text(t, x, y, w, 12.5, c, false, Align::Left);
        let by = a.bottom() - ui.s(52.0);
        let r = primary(ui, "Buscar  ↵", x + w, by, ui.s(110.0), ChatHit::SkillFetch, env) - ui.s(8.0);
        secondary(ui, "Volver", r, by, ui.s(90.0), ChatHit::ExtendBack, env);
        ui.hit(a, Hit::Chat(ChatHit::Inside));
        return 0.0;
    };
    match f.sel.and_then(|i| b.found.get(i)) {
        None => {
            let (x, w, top, a) = window(ui, screen, 760.0, 600.0, "Elige una habilidad", &b.from);
            let bottom = a.bottom() - ui.s(66.0);
            let view = Area::new(x, top, w, bottom - top);
            let row = ui.s(44.0);
            ui.clip = Some(view);
            let mut y = top - f.scroll;
            for (i, s) in b.found.iter().enumerate() {
                let r = Area::new(x, y, w, row);
                let hit = ChatHit::SkillPick(i as u16);
                if env.hover == Some(hit) {
                    ui.round(r.intersect(view), ui::ACTIVE_BG, 1.0, 8.0);
                }
                let name = format!("/{}", s.name);
                ui.text(name, x + ui.s(12.0), y + ui.s(5.0), w * 0.6, 13.0, EMBER, true, Align::Left);
                let have = if skill_exists(know, &s.name) { "ya la tienes" } else { "" };
                ui.text(have, x, y + ui.s(6.0), w - ui.s(12.0), 11.0, FAINT, false, Align::Right);
                ui.text(
                    &s.description,
                    x + ui.s(12.0),
                    y + ui.s(24.0),
                    w - ui.s(24.0),
                    11.5,
                    DIM,
                    false,
                    Align::Left,
                );
                ui.hit(r, Hit::Chat(hit));
                y += row;
            }
            ui.clip = None;
            let by = a.bottom() - ui.s(52.0);
            secondary(ui, "Volver", x + w, by, ui.s(90.0), ChatHit::ExtendBack, env);
            let t = format!("{} habilidades", b.found.len());
            ui.text(t, x, by + ui.s(9.0), w * 0.5, 11.5, FAINT, false, Align::Left);
            ui.hit(a, Hit::Chat(ChatHit::Inside));
            (y + f.scroll - top - view.h).max(0.0)
        }
        Some(s) => {
            let title = format!("/{}", s.name);
            let (x, w, mut y, a) = window(ui, screen, 820.0, 660.0, &title, &b.from);
            let count = |n: usize, one: &str, many: &str| {
                if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
            };
            let mut facts = vec![count(s.files.len(), "archivo más", "archivos más")];
            if s.scripts > 0 {
                facts.push(count(s.scripts, "programa", "programas"));
            }
            if skill_exists(know, &s.name) {
                facts.push("sustituye a la que tienes".into());
            }
            let names: Vec<String> = s.files.iter().take(6).map(|f| f.0.display().to_string()).collect();
            let more = if s.files.len() > 6 { "…" } else { "" };
            ui.text(
                facts.join(" · "),
                x,
                y,
                w,
                12.5,
                if s.scripts > 0 { HOT } else { DIM },
                true,
                Align::Left,
            );
            y += ui.s(20.0);
            if !names.is_empty() {
                mono(ui, format!("{}{more}", names.join("  ")), x, y, w, 10.5, FAINT);
                y += ui.s(20.0);
            }
            let bottom = a.bottom() - ui.s(96.0);
            let view = Area::new(x, y + ui.s(4.0), w, bottom - y - ui.s(4.0));
            ui.panel(view, FIELD, LINE, 10.0, 1.0);
            let inner = view.inset(ui.s(12.0));
            ui.clip = Some(inner);
            let lh = ui.s(18.0);
            let cols = ((inner.w / ui.s(6.9)) as usize).max(20);
            let mut ly = inner.y - f.scroll;
            for l in s.text.lines() {
                for part in wrap_mono(l, cols) {
                    if ly + lh >= inner.y && ly <= inner.bottom() {
                        mono(ui, part, inner.x, ly, inner.w, 11.0, TEXT);
                    }
                    ly += lh;
                }
            }
            ui.clip = None;
            let by = a.bottom() - ui.s(52.0);
            let (t, c) = match (&f.error, &s.problem) {
                (Some(e), _) => (e.clone(), ui::RED),
                (_, Some(p)) => (format!("No se puede instalar: {p}"), ui::RED),
                _ => (
                    "⚠ Son instrucciones de otra persona para tu modelo. Léelas antes de instalar.".into(),
                    HOT,
                ),
            };
            for (i, l) in wrap(&t, 110).iter().take(1).enumerate() {
                ui.text(l, x, by - ui.s(30.0) + ui.s(16.0) * i as f32, w, 12.0, c, false, Align::Left);
            }
            let mut r = x + w;
            if s.problem.is_none() {
                r = primary(ui, "Instalar", r, by, ui.s(110.0), ChatHit::SkillInstall, env) - ui.s(8.0);
            }
            secondary(ui, "Volver", r, by, ui.s(90.0), ChatHit::ExtendBack, env);
            ui.hit(a, Hit::Chat(ChatHit::Inside));
            (ly + f.scroll - inner.bottom()).max(0.0)
        }
    }
}

/// Variables o cabeceras de un servidor, sin enseñar lo que parece una clave.
fn extra_text(c: &forge_agent::mcp::Config) -> String {
    let (what, map) = if c.url.is_some() { ("Cabeceras", &c.headers) } else { ("Variables", &c.env) };
    if map.is_empty() {
        return format!("Sin {}", what.to_lowercase());
    }
    let secret = |k: &str| {
        ["KEY", "TOKEN", "SECRET", "PASS", "CLAVE", "AUTH", "COOKIE"]
            .iter()
            .any(|s| k.to_uppercase().contains(s))
    };
    // También una dirección con su contraseña dentro.
    let login = |v: &str| v.contains("://") && v.contains('@');
    let one = |(k, v): (&String, &String)| {
        if secret(k) || login(v) { format!("{k}=•••") } else { format!("{k}={v}") }
    };
    format!("{what}: {}", map.iter().map(one).collect::<Vec<_>>().join(" "))
}

/// Parte una línea en trozos de `cols` caracteres (al menos uno).
fn wrap_mono(l: &str, cols: usize) -> Vec<String> {
    let l: Vec<char> = l.replace('\t', "    ").chars().filter(|c| !c.is_control()).collect();
    if l.is_empty() {
        return vec![String::new()];
    }
    l.chunks(cols).map(|c| c.iter().collect()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_long_lines() {
        assert_eq!(wrap_mono("abcdef", 4), ["abcd", "ef"]);
        assert_eq!(wrap_mono("", 4), [""]);
        // Los caracteres invisibles no esconden instrucciones.
        assert_eq!(wrap_mono("a\u{1b}[8mb\u{7}", 10), ["a[8mb"]);
    }
}

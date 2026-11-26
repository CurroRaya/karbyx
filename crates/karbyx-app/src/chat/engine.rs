// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! El buscador del chat. Ningún buscador deja que un programa busque sin una
//! cuenta, así que cada uno pone la suya: aquí se elige, se explica cómo
//! conseguir la clave y se prueba antes de guardarla.

use karbyx_agent::browse::{self, Search};

use super::draw::{Env, primary, secondary, window, wrap};
use super::edit::Editor;
use super::{App, ChatHit, Popup};
use crate::UserEvent;
use crate::ui::{self, Align, Area, DIM, EMBER, Hit, TEXT, Ui};

/// Un buscador que se puede elegir.
pub struct Engine {
    pub name: &'static str,
    /// Su web, para crear la cuenta.
    pub site: &'static str,
    /// Cómo conseguir lo que pide, en pasos cortos.
    pub steps: [&'static str; 2],
    /// Qué se escribe: la clave o la dirección.
    pub field: &'static str,
    pub hint: &'static str,
    pub secret: bool,
}

pub const ENGINES: [Engine; 3] = [
    Engine {
        name: "Tavily",
        site: "https://app.tavily.com",
        steps: [
            "1 · Crea una cuenta en tavily.com. Tiene un plan gratuito, sin tarjeta.",
            "2 · Copia tu clave de API (empieza por «tvly-») y pégala aquí.",
        ],
        field: "Clave",
        hint: "tvly-…",
        secret: true,
    },
    Engine {
        name: "Brave",
        site: "https://api-dashboard.search.brave.com",
        steps: [
            "1 · Crea una cuenta en la API de Brave Search. Pide tarjeta y trae crédito cada mes.",
            "2 · Crea una clave de API y pégala aquí.",
        ],
        field: "Clave",
        hint: "la clave de tu cuenta",
        secret: true,
    },
    Engine {
        name: "SearXNG",
        site: "https://docs.searxng.org",
        steps: [
            "1 · Necesitas un servidor SearXNG (tuyo o de confianza) con el formato JSON activado.",
            "2 · Escribe su dirección.",
        ],
        field: "Dirección",
        hint: "https://searx.ejemplo.es",
        secret: false,
    },
];

/// Formulario del buscador.
#[derive(Default)]
pub struct SearchForm {
    /// Cuál de [`ENGINES`].
    pub engine: usize,
    pub value: Editor,
    /// `Ok`: probándolo. `Err`: por qué no vale.
    pub status: Option<Result<(), String>>,
}

impl SearchForm {
    fn search(&self) -> Result<Search, String> {
        let v = self.value.text().trim().to_string();
        if v.is_empty() {
            return Err(format!("Falta {}.", ENGINES[self.engine].field.to_lowercase()));
        }
        Ok(match self.engine {
            0 => Search::Tavily(v),
            1 => Search::Brave(v),
            _ if v.starts_with("https://") || v.starts_with("http://") => Search::SearXng(v),
            _ => return Err("La dirección empieza por https://".into()),
        })
    }
}

/// Nombre del buscador elegido, o `None`.
pub fn chosen(s: &Search) -> Option<&'static str> {
    match s {
        Search::Unset => None,
        Search::Tavily(_) => Some(ENGINES[0].name),
        Search::Brave(_) => Some(ENGINES[1].name),
        Search::SearXng(_) => Some(ENGINES[2].name),
    }
}

impl App {
    pub(super) fn open_search(&mut self) {
        let mut f = SearchForm::default();
        match &self.chats.saved.search {
            Search::Unset => {}
            Search::Tavily(k) => f.value.set(k.clone()),
            Search::Brave(k) => {
                f.engine = 1;
                f.value.set(k.clone());
            }
            Search::SearXng(u) => {
                f.engine = 2;
                f.value.set(u.clone());
            }
        }
        self.chats.popup = Popup::Search(Box::new(f));
    }

    /// Prueba lo escrito y, si funciona, lo guarda.
    pub(super) fn search_submit(&mut self) {
        let Popup::Search(f) = &mut self.chats.popup else { return };
        if f.status == Some(Ok(())) {
            return;
        }
        let s = match f.search() {
            Ok(s) => s,
            Err(e) => return f.status = Some(Err(e)),
        };
        f.status = Some(Ok(()));
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let r = browse::check(&s);
            let _ = proxy.send_event(UserEvent::SearchChecked(Box::new(s), r));
        });
    }

    pub(crate) fn on_search_checked(&mut self, s: Search, r: Result<(), String>) {
        let Popup::Search(f) = &mut self.chats.popup else { return };
        if f.status != Some(Ok(())) || f.search().as_ref() != Ok(&s) {
            return;
        }
        match r {
            Err(e) => f.status = Some(Err(format!("No funciona: {e}."))),
            Ok(()) => {
                let name = chosen(&s).unwrap_or_default();
                self.chats.saved.search = s;
                self.chats.saved.save();
                self.chats.popup = Popup::None;
                self.toast(format!("Buscador listo: {name}"));
            }
        }
        self.redraw();
    }

    pub(super) fn search_click(&mut self, hit: ChatHit) {
        match hit {
            ChatHit::SearchSetup => self.open_search(),
            ChatHit::SearchEngine(i) => {
                if let Popup::Search(f) = &mut self.chats.popup
                    && f.engine != i as usize
                {
                    **f = SearchForm { engine: (i as usize).min(ENGINES.len() - 1), ..SearchForm::default() };
                }
            }
            ChatHit::SearchSite => {
                if let Popup::Search(f) = &self.chats.popup
                    && std::process::Command::new("/usr/bin/open")
                        .arg(ENGINES[f.engine].site)
                        .spawn()
                        .is_err()
                {
                    self.toast("No se ha podido abrir el navegador");
                }
            }
            ChatHit::SearchRemove => {
                self.chats.saved.search = Search::Unset;
                self.chats.saved.save();
                self.chats.popup = Popup::None;
                self.toast("Buscador quitado: el modelo ya no puede buscar");
            }
            _ => {}
        }
    }
}

/// La ventana del buscador.
pub fn search_form(ui: &mut Ui, screen: Area, saved: &Search, f: &SearchForm, env: &Env) {
    let about = "Para buscar en internet, el modelo usa un buscador con tu propia cuenta.";
    let (x, w, mut y, a) = window(ui, screen, 620.0, 470.0, "Buscador", about);
    // Elegir buscador.
    let mut bx = x;
    for (i, e) in ENGINES.iter().enumerate() {
        let label = if i == 0 { format!("{} (recomendado)", e.name) } else { e.name.to_string() };
        let bw = ui.s(28.0) + label.chars().count() as f32 * ui.s(7.2);
        let b = Area::new(bx, y, bw, ui.s(32.0));
        let hit = ChatHit::SearchEngine(i as u8);
        let (on, hov) = (f.engine == i, env.hover == Some(hit));
        let bg = if on {
            ui::ACTIVE_BG
        } else if hov {
            ui::HOVER_BG
        } else {
            [0x13, 0x11, 0x0f]
        };
        ui.panel(b, bg, if on { EMBER } else { ui::BORDER }, 9.0, 1.0);
        ui.text(label, b.x, b.y + ui.s(7.5), b.w, 12.5, if on { TEXT } else { DIM }, on, Align::Center);
        ui.hit(b, Hit::Chat(hit));
        bx += bw + ui.s(8.0);
    }
    y += ui.s(52.0);
    let e = &ENGINES[f.engine];
    for s in e.steps {
        for l in wrap(s, 80) {
            ui.text(l, x, y, w, 12.5, TEXT, false, Align::Left);
            y += ui.s(20.0);
        }
        y += ui.s(4.0);
    }
    let b = Area::new(x, y, ui.s(150.0), ui.s(28.0));
    let hov = env.hover == Some(ChatHit::SearchSite);
    ui.panel(b, if hov { ui::ACTIVE_BG } else { ui::HOVER_BG }, ui::BORDER, 8.0, 1.0);
    ui.text("Abrir su web ↗", b.x, b.y + ui.s(6.0), b.w, 12.0, TEXT, false, Align::Center);
    ui.hit(b, Hit::Chat(ChatHit::SearchSite));
    y += ui.s(44.0);
    let b = Area::new(x, y, w, ui.s(40.0));
    ui.panel(b, [0x13, 0x11, 0x0f], EMBER, 10.0, 1.0);
    ui.text(e.field, b.x + ui.s(14.0), b.y + ui.s(10.5), ui.s(80.0), 12.5, DIM, true, Align::Left);
    let (tx, tw) = (b.x + ui.s(96.0), b.w - ui.s(110.0));
    if f.value.is_empty() {
        ui.text(
            format!("▏{}", e.hint),
            tx,
            b.y + ui.s(9.5),
            tw,
            13.5,
            super::draw::FAINT,
            false,
            Align::Left,
        );
    } else {
        let mask = |s: &str| if e.secret { "•".repeat(s.chars().count()) } else { s.to_string() };
        let (l, r) = f.value.text().split_at(f.value.caret());
        let l = mask(l);
        let fit = (tw / ui.s(7.6)) as usize;
        let n = l.chars().count();
        let l: String = if n > fit { l.chars().skip(n - fit).collect() } else { l };
        ui.text(format!("{l}▏{}", mask(r)), tx, b.y + ui.s(9.5), tw, 13.5, TEXT, false, Align::Left);
    }
    y += ui.s(52.0);
    let (msg, color) = match &f.status {
        None => ("La clave se guarda en tu Mac, solo para ti. Cada búsqueda te la enseña antes.", DIM),
        Some(Ok(())) => ("Probando con una búsqueda…", ui::HOT),
        Some(Err(e)) => (e.as_str(), ui::RED),
    };
    for l in wrap(msg, 82).iter().take(2) {
        ui.text(l, x, y, w, 12.0, color, false, Align::Left);
        y += ui.s(17.0);
    }
    let by = a.bottom() - ui.s(58.0);
    let r = primary(ui, "Probar y guardar  ↵", x + w, by, ui.s(170.0), ChatHit::FormOk, env);
    let r = secondary(ui, "Cancelar", r - ui.s(8.0), by, ui.s(100.0), ChatHit::FormCancel, env);
    if *saved != Search::Unset {
        secondary(ui, "Quitar", r - ui.s(8.0), by, ui.s(90.0), ChatHit::SearchRemove, env);
    }
    ui.hit(a, Hit::Chat(ChatHit::Inside));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_each_engine_needs() {
        let mut f = SearchForm::default();
        assert!(f.search().is_err(), "sin clave no vale");
        f.value.set(" tvly-abc ");
        assert_eq!(f.search(), Ok(Search::Tavily("tvly-abc".into())));
        f.engine = 1;
        assert_eq!(f.search(), Ok(Search::Brave("tvly-abc".into())));
        f.engine = 2;
        assert!(f.search().is_err(), "SearXNG pide una dirección");
        f.value.set("https://s.es");
        assert_eq!(f.search(), Ok(Search::SearXng("https://s.es".into())));
        assert_eq!(chosen(&Search::Unset), None);
        assert_eq!(chosen(&Search::Brave("k".into())), Some("Brave"));
    }
}

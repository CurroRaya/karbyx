// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Atajos de teclado: combinaciones legibles (`"cmd+shift+d"`) asociadas a
//! acciones, con valores por defecto que `[keys]` de `forge.toml` puede cambiar.

use std::collections::BTreeMap;

use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use crate::panes::Nav;

/// Acciones de la propia terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Paste,
    Copy,
    Quit,
    NewTab,
    NewAi,
    /// Cierra el panel activo (o la pestaña si es el último).
    Close,
    SelectTab(usize),
    NextTab,
    PrevTab,
    ToggleLeft,
    ToggleRight,
    FontBigger,
    FontSmaller,
    FontReset,
    ClearScreen,
    SplitRight,
    SplitDown,
    Focus(Nav),
    Find,
    FindNext,
    FindPrev,
    /// Seleccionar el bloque (comando) anterior / siguiente.
    BlockPrev,
    BlockNext,
    /// Preguntar a la IA por el bloque seleccionado (o el último fallido).
    AskAi,
}

/// Nombre de cada acción configurable en `[keys]`.
const NAMES: &[(&str, Action)] = &[
    ("paste", Action::Paste),
    ("copy", Action::Copy),
    ("quit", Action::Quit),
    ("new_tab", Action::NewTab),
    ("new_ai", Action::NewAi),
    ("close", Action::Close),
    ("next_tab", Action::NextTab),
    ("prev_tab", Action::PrevTab),
    ("toggle_sidebar", Action::ToggleLeft),
    ("toggle_usage", Action::ToggleRight),
    ("font_bigger", Action::FontBigger),
    ("font_smaller", Action::FontSmaller),
    ("font_reset", Action::FontReset),
    ("clear", Action::ClearScreen),
    ("split_right", Action::SplitRight),
    ("split_down", Action::SplitDown),
    ("focus_left", Action::Focus(Nav::Left)),
    ("focus_right", Action::Focus(Nav::Right)),
    ("focus_up", Action::Focus(Nav::Up)),
    ("focus_down", Action::Focus(Nav::Down)),
    ("find", Action::Find),
    ("find_next", Action::FindNext),
    ("find_prev", Action::FindPrev),
    ("block_prev", Action::BlockPrev),
    ("block_next", Action::BlockNext),
    ("ask_ai", Action::AskAi),
];

const DEFAULTS: &[(&str, &str)] = &[
    ("paste", "cmd+v"),
    ("copy", "cmd+c"),
    ("quit", "cmd+q"),
    ("new_tab", "cmd+t"),
    ("new_ai", "cmd+shift+c"),
    ("close", "cmd+w"),
    ("next_tab", "ctrl+tab"),
    ("prev_tab", "ctrl+shift+tab"),
    ("next_tab", "cmd+shift+down"),
    ("prev_tab", "cmd+shift+up"),
    ("toggle_sidebar", "cmd+b"),
    ("toggle_usage", "cmd+alt+b"),
    ("font_bigger", "cmd+="),
    ("font_bigger", "cmd++"),
    ("font_smaller", "cmd+-"),
    ("font_reset", "cmd+0"),
    ("clear", "cmd+k"),
    ("split_right", "cmd+d"),
    ("split_down", "cmd+shift+d"),
    ("focus_left", "cmd+alt+left"),
    ("focus_right", "cmd+alt+right"),
    ("focus_up", "cmd+alt+up"),
    ("focus_down", "cmd+alt+down"),
    ("find", "cmd+f"),
    ("find_next", "cmd+g"),
    ("find_prev", "cmd+shift+g"),
    ("block_prev", "cmd+up"),
    ("block_next", "cmd+down"),
    ("ask_ai", "cmd+i"),
];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Carácter en minúscula o nombre de tecla (`left`, `enter`, `tab`, `f5`...).
    pub key: String,
}

impl Chord {
    /// `"cmd+shift+d"`, `"ctrl+tab"`, `"cmd++"`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        let (mods, key) = if let Some(rest) = s.strip_suffix("++") {
            (rest, "+".to_string())
        } else {
            let (m, k) = s.rsplit_once('+').unwrap_or(("", s.as_str()));
            (m, k.to_string())
        };
        if key.is_empty() {
            return None;
        }
        let mut c = Chord { cmd: false, ctrl: false, alt: false, shift: false, key };
        for m in mods.split('+').filter(|m| !m.is_empty()) {
            match m {
                "cmd" | "super" | "command" => c.cmd = true,
                "ctrl" | "control" => c.ctrl = true,
                "alt" | "opt" | "option" => c.alt = true,
                "shift" => c.shift = true,
                _ => return None,
            }
        }
        Some(c)
    }

    pub fn from_event(event: &KeyEvent, mods: ModifiersState) -> Option<Self> {
        let key = match event.key_without_modifiers().as_ref() {
            Key::Character(s) => s.to_lowercase(),
            Key::Named(n) => named(n)?.to_string(),
            _ => return None,
        };
        Some(Chord {
            cmd: mods.super_key(),
            ctrl: mods.control_key(),
            alt: mods.alt_key(),
            shift: mods.shift_key(),
            key,
        })
    }
}

fn named(n: NamedKey) -> Option<&'static str> {
    Some(match n {
        NamedKey::ArrowLeft => "left",
        NamedKey::ArrowRight => "right",
        NamedKey::ArrowUp => "up",
        NamedKey::ArrowDown => "down",
        NamedKey::Enter => "enter",
        NamedKey::Tab => "tab",
        NamedKey::Backspace => "backspace",
        NamedKey::Escape => "escape",
        NamedKey::Space => "space",
        NamedKey::Delete => "delete",
        NamedKey::Home => "home",
        NamedKey::End => "end",
        NamedKey::PageUp => "pageup",
        NamedKey::PageDown => "pagedown",
        NamedKey::F1 => "f1",
        NamedKey::F2 => "f2",
        NamedKey::F3 => "f3",
        NamedKey::F4 => "f4",
        NamedKey::F5 => "f5",
        NamedKey::F6 => "f6",
        NamedKey::F7 => "f7",
        NamedKey::F8 => "f8",
        NamedKey::F9 => "f9",
        NamedKey::F10 => "f10",
        NamedKey::F11 => "f11",
        NamedKey::F12 => "f12",
        _ => return None,
    })
}

#[derive(Debug, Clone)]
pub struct Bindings {
    map: Vec<(Chord, Action)>,
}

impl Bindings {
    /// Atajos por defecto con los cambios de `[keys]`. Una acción redefinida
    /// pierde sus atajos por defecto; `"none"` la desactiva.
    pub fn new(overrides: &BTreeMap<String, String>) -> Self {
        let action = |name: &str| NAMES.iter().find(|(n, _)| *n == name).map(|(_, a)| *a);
        let mut map: Vec<(Chord, Action)> = DEFAULTS
            .iter()
            .filter(|(name, _)| !overrides.contains_key(*name))
            .filter_map(|(name, chord)| Some((Chord::parse(chord)?, action(name)?)))
            .collect();
        for (name, chord) in overrides {
            match (action(name), Chord::parse(chord)) {
                (Some(a), Some(c)) if chord.trim() != "none" => map.push((c, a)),
                (Some(_), _) => {}
                (None, _) => log::warn!("[keys]: acción desconocida '{name}'"),
            }
        }
        // ⌘1…⌘9 seleccionan pestaña (fijos).
        for i in 0..9 {
            map.push((
                Chord { cmd: true, ctrl: false, alt: false, shift: false, key: (i + 1).to_string() },
                Action::SelectTab(i),
            ));
        }
        Self { map }
    }

    pub fn lookup(&self, event: &KeyEvent, mods: ModifiersState) -> Option<Action> {
        let chord = Chord::from_event(event, mods)?;
        self.map.iter().find(|(c, _)| *c == chord).map(|(_, a)| *a)
    }

    #[cfg(test)]
    fn lookup_chord(&self, chord: &Chord) -> Option<Action> {
        self.map.iter().find(|(c, _)| c == chord).map(|(_, a)| *a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chords() {
        let c = Chord::parse("cmd+shift+D").unwrap();
        assert!(c.cmd && c.shift && !c.alt && !c.ctrl);
        assert_eq!(c.key, "d");
        assert_eq!(Chord::parse("cmd++").unwrap().key, "+");
        assert_eq!(Chord::parse("ctrl+tab").unwrap().key, "tab");
        assert!(Chord::parse("hyper+x").is_none());
        assert!(Chord::parse("cmd+").is_none());
    }

    #[test]
    fn defaults_and_overrides() {
        let b = Bindings::new(&BTreeMap::new());
        assert_eq!(b.lookup_chord(&Chord::parse("cmd+d").unwrap()), Some(Action::SplitRight));
        assert_eq!(b.lookup_chord(&Chord::parse("cmd+3").unwrap()), Some(Action::SelectTab(2)));

        let mut o = BTreeMap::new();
        o.insert("split_right".to_string(), "cmd+e".to_string());
        o.insert("find".to_string(), "none".to_string());
        let b = Bindings::new(&o);
        assert_eq!(b.lookup_chord(&Chord::parse("cmd+e").unwrap()), Some(Action::SplitRight));
        assert_eq!(b.lookup_chord(&Chord::parse("cmd+d").unwrap()), None);
        assert_eq!(b.lookup_chord(&Chord::parse("cmd+f").unwrap()), None);
    }
}

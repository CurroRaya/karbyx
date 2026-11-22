// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 CurroRaya y los colaboradores de forge

//! Traducción de teclas de winit a secuencias de bytes para la shell.

use forge_term::TermMode;
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use crate::keys::{Action, Bindings};

pub enum KeyOutcome {
    Bytes(Vec<u8>),
    Action(Action),
    None,
}

fn csi_mod(mods: ModifiersState) -> u8 {
    // Parámetro de modificador xterm: 1 + shift(1) + alt(2) + ctrl(4).
    1 + mods.shift_key() as u8 + 2 * mods.alt_key() as u8 + 4 * mods.control_key() as u8
}

/// Traduce una tecla: primero los atajos de la terminal, luego lo que se
/// envía a la shell. `option_as_alt`: ⌥ actúa como Meta (ESC + tecla).
pub fn translate(
    event: &KeyEvent,
    mods: ModifiersState,
    mode: TermMode,
    bindings: &Bindings,
    option_as_alt: bool,
) -> KeyOutcome {
    if let Some(a) = bindings.lookup(event, mods) {
        return KeyOutcome::Action(a);
    }

    if mods.super_key() {
        // Edición natural de texto (como iTerm2/Warp); el resto de ⌘ no llega a la shell.
        return match event.key_without_modifiers().as_ref() {
            Key::Named(NamedKey::ArrowLeft) => KeyOutcome::Bytes(vec![0x01]), // inicio de línea
            Key::Named(NamedKey::ArrowRight) => KeyOutcome::Bytes(vec![0x05]), // fin de línea
            Key::Named(NamedKey::Backspace) => KeyOutcome::Bytes(vec![0x15]), // borra la línea
            _ => KeyOutcome::None,
        };
    }

    // ⌥ como Meta: ESC + la tecla sin componer.
    if option_as_alt
        && mods.alt_key()
        && !mods.control_key()
        && let Key::Character(s) = event.key_without_modifiers().as_ref()
    {
        let ch = if mods.shift_key() { s.to_uppercase() } else { s.to_string() };
        let mut out = vec![0x1b];
        out.extend_from_slice(ch.as_bytes());
        return KeyOutcome::Bytes(out);
    }

    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let m = csi_mod(mods);
    let arrow = |c: char| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[1;{m}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if m > 1 { format!("\x1b[{n};{m}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };

    if let Key::Named(named) = &event.logical_key {
        let bytes = match named {
            NamedKey::Enter => Some(if mods.alt_key() { b"\x1b\r".to_vec() } else { b"\r".to_vec() }),
            NamedKey::Backspace => Some(if mods.alt_key() {
                b"\x1b\x7f".to_vec() // borra palabra
            } else if mods.control_key() {
                vec![0x08]
            } else {
                vec![0x7f]
            }),
            NamedKey::Tab => Some(if mods.shift_key() { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }),
            NamedKey::Escape => Some(b"\x1b".to_vec()),
            // Option+←/→ salta palabras.
            NamedKey::ArrowLeft if mods.alt_key() && !mods.control_key() => Some(b"\x1bb".to_vec()),
            NamedKey::ArrowRight if mods.alt_key() && !mods.control_key() => Some(b"\x1bf".to_vec()),
            NamedKey::ArrowUp => Some(arrow('A')),
            NamedKey::ArrowDown => Some(arrow('B')),
            NamedKey::ArrowRight => Some(arrow('C')),
            NamedKey::ArrowLeft => Some(arrow('D')),
            NamedKey::Home => Some(arrow('H')),
            NamedKey::End => Some(arrow('F')),
            NamedKey::Insert => Some(tilde(2)),
            NamedKey::Delete => Some(tilde(3)),
            NamedKey::PageUp => Some(tilde(5)),
            NamedKey::PageDown => Some(tilde(6)),
            NamedKey::F1 => Some(b"\x1bOP".to_vec()),
            NamedKey::F2 => Some(b"\x1bOQ".to_vec()),
            NamedKey::F3 => Some(b"\x1bOR".to_vec()),
            NamedKey::F4 => Some(b"\x1bOS".to_vec()),
            NamedKey::F5 => Some(tilde(15)),
            NamedKey::F6 => Some(tilde(17)),
            NamedKey::F7 => Some(tilde(18)),
            NamedKey::F8 => Some(tilde(19)),
            NamedKey::F9 => Some(tilde(20)),
            NamedKey::F10 => Some(tilde(21)),
            NamedKey::F11 => Some(tilde(23)),
            NamedKey::F12 => Some(tilde(24)),
            NamedKey::Space if mods.control_key() => Some(vec![0]),
            _ => None,
        };
        if let Some(b) = bytes {
            return KeyOutcome::Bytes(b);
        }
    }

    // Ctrl + tecla → carácter de control.
    if mods.control_key()
        && let Key::Character(s) = event.key_without_modifiers().as_ref()
        && let Some(ch) = s.chars().next()
    {
        let code = match ch.to_ascii_lowercase() {
            c @ 'a'..='z' => Some(c as u8 - b'a' + 1),
            '[' | '3' => Some(0x1b),
            '\\' | '4' => Some(0x1c),
            ']' | '5' => Some(0x1d),
            '6' => Some(0x1e),
            '/' | '-' | '7' => Some(0x1f),
            '2' | '@' | ' ' => Some(0),
            _ => None,
        };
        if let Some(c) = code {
            return if mods.alt_key() {
                KeyOutcome::Bytes(vec![0x1b, c])
            } else {
                KeyOutcome::Bytes(vec![c])
            };
        }
    }

    // Texto normal. En macOS Option compone caracteres (@, #, [, ñ...), así
    // que respetamos el texto que entrega el sistema.
    if let Some(text) = &event.text
        && !text.is_empty()
    {
        return KeyOutcome::Bytes(text.as_bytes().to_vec());
    }
    KeyOutcome::None
}

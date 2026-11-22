// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 CurroRaya y los colaboradores de forge

//! Codificación de eventos de ratón para aplicaciones que los piden (vim,
//! htop, tmux...): protocolos SGR (1006), UTF-8 (1005) y X10 clásico.

use alacritty_terminal::term::TermMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    /// Movimiento con `button` pulsado (o sin botón si es `None` en motion).
    Motion,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// ¿Quiere la aplicación eventos de ratón?
pub fn wants_mouse(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

/// Secuencia a enviar para un evento, o `None` si el modo actual no lo pide.
/// `col` y `row` son 0-based en el viewport.
pub fn encode(
    mode: TermMode,
    button: Option<MouseButton>,
    action: MouseAction,
    col: usize,
    row: usize,
    mods: MouseMods,
) -> Option<Vec<u8>> {
    if !wants_mouse(mode) {
        return None;
    }
    if action == MouseAction::Motion {
        let wanted = mode.contains(TermMode::MOUSE_MOTION)
            || (mode.contains(TermMode::MOUSE_DRAG) && button.is_some());
        if !wanted {
            return None;
        }
    }
    let sgr = mode.contains(TermMode::SGR_MOUSE);
    let mut code: u32 = match button {
        Some(MouseButton::Left) => 0,
        Some(MouseButton::Middle) => 1,
        Some(MouseButton::Right) => 2,
        Some(MouseButton::WheelUp) => 64,
        Some(MouseButton::WheelDown) => 65,
        None => 3, // movimiento sin botón
    };
    // En los protocolos antiguos la liberación no dice qué botón era.
    if action == MouseAction::Release && !sgr {
        code = 3;
    }
    if action == MouseAction::Motion {
        code += 32;
    }
    code += 4 * mods.shift as u32 + 8 * mods.alt as u32 + 16 * mods.ctrl as u32;

    let (x, y) = (col + 1, row + 1);
    if sgr {
        let end = if action == MouseAction::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }
    let mut out = b"\x1b[M".to_vec();
    out.push((32 + code) as u8);
    if mode.contains(TermMode::UTF8_MOUSE) {
        for v in [x, y] {
            let c = char::from_u32(32 + v as u32)?;
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    } else {
        // X10: coordenadas en un byte, máximo 223.
        if x > 223 || y > 223 {
            return None;
        }
        out.push((32 + x) as u8);
        out.push((32 + y) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: MouseMods = MouseMods { shift: false, alt: false, ctrl: false };

    #[test]
    fn no_mode_no_events() {
        assert!(encode(TermMode::empty(), Some(MouseButton::Left), MouseAction::Press, 0, 0, NONE).is_none());
    }

    #[test]
    fn sgr_press_release() {
        let m = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(
            encode(m, Some(MouseButton::Left), MouseAction::Press, 4, 9, NONE).unwrap(),
            b"\x1b[<0;5;10M"
        );
        assert_eq!(
            encode(m, Some(MouseButton::Left), MouseAction::Release, 4, 9, NONE).unwrap(),
            b"\x1b[<0;5;10m"
        );
        let ctrl = MouseMods { ctrl: true, ..NONE };
        assert_eq!(
            encode(m, Some(MouseButton::WheelUp), MouseAction::Press, 0, 0, ctrl).unwrap(),
            b"\x1b[<80;1;1M"
        );
        // Sin MOUSE_DRAG no se envía movimiento.
        assert!(encode(m, Some(MouseButton::Left), MouseAction::Motion, 1, 1, NONE).is_none());
        let drag = m | TermMode::MOUSE_DRAG;
        assert_eq!(
            encode(drag, Some(MouseButton::Left), MouseAction::Motion, 1, 1, NONE).unwrap(),
            b"\x1b[<32;2;2M"
        );
    }

    #[test]
    fn x10_encoding() {
        let m = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(
            encode(m, Some(MouseButton::Right), MouseAction::Press, 0, 0, NONE).unwrap(),
            vec![0x1b, b'[', b'M', 34, 33, 33]
        );
        assert_eq!(encode(m, Some(MouseButton::Right), MouseAction::Release, 0, 0, NONE).unwrap()[3], 35);
        assert!(encode(m, Some(MouseButton::Left), MouseAction::Press, 300, 0, NONE).is_none());
        let utf8 = m | TermMode::UTF8_MOUSE;
        assert!(encode(utf8, Some(MouseButton::Left), MouseAction::Press, 300, 0, NONE).is_some());
    }
}

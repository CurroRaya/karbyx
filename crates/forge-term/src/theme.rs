// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Paleta de colores y resolución de colores ANSI a RGB.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

pub type Rgb8 = [u8; 3];

#[derive(Debug, Clone)]
pub struct Theme {
    /// Colores 0..=15 (normales y brillantes).
    pub ansi: [Rgb8; 16],
    pub foreground: Rgb8,
    pub background: Rgb8,
    pub cursor: Rgb8,
    pub selection: Rgb8,
    /// Fondo de las coincidencias de búsqueda visibles.
    pub search_match: Rgb8,
    /// Fondo de la coincidencia actual.
    pub search_current: Rgb8,
}

impl Default for Theme {
    /// Tema oscuro por defecto, inspirado en paletas tipo "One Dark".
    fn default() -> Self {
        Self {
            ansi: [
                [0x28, 0x2c, 0x34], // black
                [0xe0, 0x6c, 0x75], // red
                [0x98, 0xc3, 0x79], // green
                [0xe5, 0xc0, 0x7b], // yellow
                [0x61, 0xaf, 0xef], // blue
                [0xc6, 0x78, 0xdd], // magenta
                [0x56, 0xb6, 0xc2], // cyan
                [0xab, 0xb2, 0xbf], // white
                [0x5c, 0x63, 0x70], // bright black
                [0xff, 0x7b, 0x86],
                [0xb1, 0xe1, 0x8b],
                [0xef, 0xcd, 0x8e],
                [0x80, 0xc4, 0xff],
                [0xde, 0x8e, 0xf5],
                [0x72, 0xd1, 0xdc],
                [0xff, 0xff, 0xff], // bright white
            ],
            foreground: [0xd7, 0xda, 0xe0],
            background: [0x1a, 0x1d, 0x23],
            cursor: [0xc6, 0x78, 0xdd],
            selection: [0x3e, 0x44, 0x51],
            search_match: [0x5c, 0x4a, 0x1e],
            search_current: [0xe5, 0xc0, 0x7b],
        }
    }
}

fn dim(c: Rgb8) -> Rgb8 {
    c.map(|v| (v as f32 * 0.66) as u8)
}

impl Theme {
    /// Color de la paleta de 256 colores de xterm.
    pub fn indexed(&self, i: u8) -> Rgb8 {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                let i = i - 16;
                let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                [step(i / 36), step((i / 6) % 6), step(i % 6)]
            }
            232..=255 => {
                let v = 8 + (i - 232) * 10;
                [v, v, v]
            }
        }
    }

    /// Resuelve un color de celda. `overrides` son los cambios de paleta que
    /// hace la aplicación con OSC 4/10/11.
    pub fn resolve(&self, color: Color, overrides: &Colors) -> Rgb8 {
        let rgb = |r: Rgb| [r.r, r.g, r.b];
        match color {
            Color::Spec(r) => rgb(r),
            Color::Indexed(i) => overrides[i as usize].map(rgb).unwrap_or_else(|| self.indexed(i)),
            Color::Named(n) => {
                if let Some(r) = overrides[n] {
                    return rgb(r);
                }
                let idx = n as usize;
                match n {
                    NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
                    NamedColor::Background => self.background,
                    NamedColor::Cursor => self.cursor,
                    NamedColor::DimForeground => dim(self.foreground),
                    _ if idx < 16 => self.ansi[idx],
                    // DimBlack..DimWhite van seguidos tras Cursor.
                    _ => {
                        let base = idx - NamedColor::DimBlack as usize;
                        dim(self.ansi[base.min(7)])
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_cube_and_gray() {
        let t = Theme::default();
        assert_eq!(t.indexed(16), [0, 0, 0]);
        assert_eq!(t.indexed(196), [255, 0, 0]);
        assert_eq!(t.indexed(231), [255, 255, 255]);
        assert_eq!(t.indexed(232), [8, 8, 8]);
        assert_eq!(t.indexed(255), [238, 238, 238]);
    }

    #[test]
    fn named_resolution() {
        let t = Theme::default();
        let o = Colors::default();
        assert_eq!(t.resolve(Color::Named(NamedColor::Red), &o), t.ansi[1]);
        assert_eq!(t.resolve(Color::Named(NamedColor::Background), &o), t.background);
        assert_eq!(t.resolve(Color::Named(NamedColor::DimRed), &o), dim(t.ansi[1]));
    }
}

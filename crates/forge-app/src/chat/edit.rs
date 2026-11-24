// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Editor de texto de la caja del chat: cursor, selección y movimientos de
//! macOS. No sabe nada de píxeles.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Editor {
    text: String,
    caret: usize,
    /// Otro extremo de la selección.
    anchor: Option<usize>,
    undo: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Move {
    Left,
    Right,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    Start,
    End,
}

impl Editor {
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn set(&mut self, t: impl Into<String>) {
        self.save();
        self.text = t.into();
        self.caret = self.text.len();
        self.anchor = None;
    }

    /// Deja la caja vacía y devuelve lo que había.
    pub fn take(&mut self) -> String {
        let t = std::mem::take(&mut self.text);
        *self = Self::default();
        t
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.caret).then(|| (a.min(self.caret), a.max(self.caret)))
    }

    pub fn selected(&self) -> Option<&str> {
        self.selection().map(|(a, b)| &self.text[a..b])
    }

    fn save(&mut self) {
        if self.undo.last().is_none_or(|(t, _)| *t != self.text) {
            self.undo.push((self.text.clone(), self.caret));
            if self.undo.len() > 100 {
                self.undo.remove(0);
            }
        }
    }

    pub fn undo(&mut self) {
        while let Some((t, c)) = self.undo.pop() {
            if t != self.text {
                self.caret = c.min(t.len());
                self.text = t;
                self.anchor = None;
                return;
            }
        }
    }

    pub fn insert(&mut self, s: &str) {
        // Un paso de deshacer por palabra.
        let word = s.chars().count() == 1 && !s.starts_with(char::is_whitespace);
        if !word || self.selection().is_some() || self.undo.is_empty() {
            self.save();
        }
        if let Some((a, b)) = self.selection() {
            self.text.replace_range(a..b, "");
            self.caret = a;
        }
        self.anchor = None;
        self.text.insert_str(self.caret, s);
        self.caret += s.len();
    }

    /// Borra la selección o, si no hay, hasta donde llevaría `m`.
    pub fn delete(&mut self, m: Move) {
        self.save();
        let (a, b) = self.selection().unwrap_or_else(|| {
            let t = self.target(m);
            (t.min(self.caret), t.max(self.caret))
        });
        self.text.replace_range(a..b, "");
        self.caret = a;
        self.anchor = None;
    }

    fn target(&self, m: Move) -> usize {
        let (t, c) = (&self.text, self.caret);
        let word = |c: char| c.is_alphanumeric() || c == '_';
        match m {
            Move::Left => t[..c].chars().next_back().map_or(0, |x| c - x.len_utf8()),
            Move::Right => t[c..].chars().next().map_or(c, |x| c + x.len_utf8()),
            Move::WordLeft => {
                let s = t[..c].trim_end_matches(|x| !word(x));
                s.trim_end_matches(word).len()
            }
            Move::WordRight => {
                let s = t[c..].trim_start_matches(|x| !word(x));
                t.len() - s.trim_start_matches(word).len()
            }
            Move::LineStart => t[..c].rfind('\n').map_or(0, |i| i + 1),
            Move::LineEnd => t[c..].find('\n').map_or(t.len(), |i| c + i),
            Move::Start => 0,
            Move::End => t.len(),
        }
    }

    pub fn go(&mut self, m: Move, select: bool) {
        // Con selección, ← y → van a su extremo.
        if !select && let Some((a, b)) = self.selection() {
            match m {
                Move::Left => return self.place(a, false),
                Move::Right => return self.place(b, false),
                _ => {}
            }
        }
        self.place(self.target(m), select);
    }

    pub fn place(&mut self, at: usize, select: bool) {
        match (select, self.anchor) {
            (true, None) => self.anchor = Some(self.caret),
            (false, _) => self.anchor = None,
            _ => {}
        }
        self.caret = at.min(self.text.len());
        while !self.text.is_char_boundary(self.caret) {
            self.caret -= 1;
        }
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.caret = self.text.len();
    }

    /// Byte → (línea, byte en la línea).
    pub fn pos(&self, at: usize) -> (usize, usize) {
        let before = &self.text[..at];
        (before.matches('\n').count(), at - before.rfind('\n').map_or(0, |i| i + 1))
    }

    /// (línea, byte en la línea) → byte.
    pub fn byte(&self, (line, index): (usize, usize)) -> usize {
        let start: usize = self.text.split_inclusive('\n').take(line).map(str::len).sum();
        let len = self.text[start..].find('\n').unwrap_or(self.text.len() - start);
        start + index.min(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(t: &str) -> Editor {
        let mut e = Editor::default();
        e.set(t);
        e
    }

    #[test]
    fn editing_and_moves() {
        let mut e = ed("hola año");
        e.delete(Move::Left);
        assert_eq!(e.text(), "hola añ");
        e.go(Move::WordLeft, false);
        assert_eq!(e.caret(), 5);
        e.go(Move::LineStart, true);
        assert_eq!(e.selected(), Some("hola "));
        e.insert("¡");
        assert_eq!(e.text(), "¡añ");
        e.undo();
        assert_eq!(e.text(), "hola añ");
        e.select_all();
        e.go(Move::Left, false);
        assert_eq!((e.caret(), e.selection()), (0, None));
        e.delete(Move::WordRight);
        assert_eq!(e.text(), " añ");
        assert_eq!(e.take(), " añ");
        e.delete(Move::Left);
        e.go(Move::Right, true);
        assert!(e.is_empty() && e.selection().is_none());
    }

    #[test]
    fn positions() {
        let e = ed("a\nñb\n\nc");
        for at in [0, 1, 2, 4, 5, 6, 7, 8] {
            assert_eq!(e.byte(e.pos(at)), at);
        }
        assert_eq!(e.pos(6), (2, 0));
        assert_eq!(e.byte((1, 99)), 5);
    }
}

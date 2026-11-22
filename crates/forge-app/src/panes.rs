// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Árbol de paneles de una pestaña: cada hoja es una sesión de terminal y
//! cada nodo interno divide su área en dos (lado a lado o arriba/abajo).

use crate::ui::Area;

pub type PaneId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    /// Lado a lado (divisor vertical).
    Horizontal,
    /// Uno encima de otro (divisor horizontal).
    Vertical,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Leaf(PaneId),
    Split { dir: SplitDir, ratio: f32, a: Box<Node>, b: Box<Node> },
}

/// Divisor entre dos paneles, para pintarlo y arrastrarlo.
#[derive(Debug, Clone, PartialEq)]
pub struct Divider {
    /// Línea a pintar.
    pub line: Area,
    /// Zona sensible al ratón (más ancha que la línea).
    pub grip: Area,
    pub dir: SplitDir,
    /// Área del nodo que divide (para convertir posición del ratón en ratio).
    pub parent: Area,
    /// Camino desde la raíz: `false` = hijo `a`, `true` = hijo `b`.
    pub path: Vec<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Left,
    Right,
    Up,
    Down,
}

const MIN_RATIO: f32 = 0.1;

impl Node {
    pub fn leaves(&self) -> Vec<PaneId> {
        match self {
            Node::Leaf(id) => vec![*id],
            Node::Split { a, b, .. } => {
                let mut v = a.leaves();
                v.extend(b.leaves());
                v
            }
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Node::Leaf(x) => *x == id,
            Node::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    /// Divide el panel `target` en dos; el nuevo queda en el lado `b`.
    pub fn split(&mut self, target: PaneId, new: PaneId, dir: SplitDir) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                *self = Node::Split {
                    dir,
                    ratio: 0.5,
                    a: Box::new(Node::Leaf(target)),
                    b: Box::new(Node::Leaf(new)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split(target, new, dir) || b.split(target, new, dir),
        }
    }

    /// Quita un panel; su hermano ocupa el sitio. No quita la última hoja.
    pub fn remove(&mut self, target: PaneId) -> bool {
        let Node::Split { a, b, .. } = self else { return false };
        if **a == Node::Leaf(target) {
            *self = (**b).clone();
            return true;
        }
        if **b == Node::Leaf(target) {
            *self = (**a).clone();
            return true;
        }
        a.remove(target) || b.remove(target)
    }

    /// Reparte `area` entre las hojas. `gap` es el grosor del divisor.
    pub fn layout(&self, area: Area, gap: f32, panes: &mut Vec<(PaneId, Area)>, dividers: &mut Vec<Divider>) {
        self.layout_at(area, gap, panes, dividers, &mut Vec::new());
    }

    fn layout_at(
        &self,
        area: Area,
        gap: f32,
        panes: &mut Vec<(PaneId, Area)>,
        dividers: &mut Vec<Divider>,
        path: &mut Vec<bool>,
    ) {
        match self {
            Node::Leaf(id) => panes.push((*id, area)),
            Node::Split { dir, ratio, a, b } => {
                let grip_w = (gap * 6.0).max(6.0);
                let (ra, rb, line) = match dir {
                    SplitDir::Horizontal => {
                        let wa = ((area.w - gap) * ratio).round();
                        let ra = Area::new(area.x, area.y, wa, area.h);
                        let line = Area::new(area.x + wa, area.y, gap, area.h);
                        let rb = Area::new(line.right(), area.y, area.w - wa - gap, area.h);
                        (ra, rb, line)
                    }
                    SplitDir::Vertical => {
                        let ha = ((area.h - gap) * ratio).round();
                        let ra = Area::new(area.x, area.y, area.w, ha);
                        let line = Area::new(area.x, area.y + ha, area.w, gap);
                        let rb = Area::new(area.x, line.bottom(), area.w, area.h - ha - gap);
                        (ra, rb, line)
                    }
                };
                let grip = match dir {
                    SplitDir::Horizontal => Area::new(line.x - (grip_w - gap) / 2.0, line.y, grip_w, line.h),
                    SplitDir::Vertical => Area::new(line.x, line.y - (grip_w - gap) / 2.0, line.w, grip_w),
                };
                dividers.push(Divider { line, grip, dir: *dir, parent: area, path: path.clone() });
                path.push(false);
                a.layout_at(ra, gap, panes, dividers, path);
                path.pop();
                path.push(true);
                b.layout_at(rb, gap, panes, dividers, path);
                path.pop();
            }
        }
    }

    /// Cambia el ratio del nodo indicado por `path`.
    pub fn set_ratio(&mut self, path: &[bool], ratio: f32) {
        match (self, path.split_first()) {
            (Node::Split { ratio: r, .. }, None) => *r = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO),
            (Node::Split { a, b, .. }, Some((first, rest))) => {
                if *first {
                    b.set_ratio(rest, ratio)
                } else {
                    a.set_ratio(rest, ratio)
                }
            }
            _ => {}
        }
    }
}

/// Panel vecino de `from` en una dirección, según la geometría.
pub fn neighbor(panes: &[(PaneId, Area)], from: PaneId, nav: Nav) -> Option<PaneId> {
    let (_, cur) = *panes.iter().find(|(id, _)| *id == from)?;
    let (cx, cy) = (cur.x + cur.w / 2.0, cur.y + cur.h / 2.0);
    panes
        .iter()
        .filter(|(id, _)| *id != from)
        .filter(|(_, a)| {
            let overlap_y = a.y < cur.bottom() && a.bottom() > cur.y;
            let overlap_x = a.x < cur.right() && a.right() > cur.x;
            match nav {
                Nav::Left => overlap_y && a.right() <= cur.x + 1.0,
                Nav::Right => overlap_y && a.x >= cur.right() - 1.0,
                Nav::Up => overlap_x && a.bottom() <= cur.y + 1.0,
                Nav::Down => overlap_x && a.y >= cur.bottom() - 1.0,
            }
        })
        .min_by(|(_, a), (_, b)| {
            let d = |r: &Area| {
                let (x, y) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                (x - cx).powi(2) + (y - cy).powi(2)
            };
            d(a).total_cmp(&d(b))
        })
        .map(|(id, _)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(n: &Node) -> (Vec<(PaneId, Area)>, Vec<Divider>) {
        let (mut p, mut d) = (Vec::new(), Vec::new());
        n.layout(Area::new(0.0, 0.0, 101.0, 51.0), 1.0, &mut p, &mut d);
        (p, d)
    }

    #[test]
    fn split_layout_and_remove() {
        let mut n = Node::Leaf(1);
        assert!(n.split(1, 2, SplitDir::Horizontal));
        assert!(n.split(2, 3, SplitDir::Vertical));
        assert_eq!(n.leaves(), vec![1, 2, 3]);

        let (p, d) = layout(&n);
        assert_eq!(d.len(), 2);
        assert_eq!(p[0].1, Area::new(0.0, 0.0, 50.0, 51.0));
        assert_eq!(p[1].1, Area::new(51.0, 0.0, 50.0, 25.0));
        assert_eq!(p[2].1, Area::new(51.0, 26.0, 50.0, 25.0));

        assert!(n.remove(2));
        assert_eq!(n.leaves(), vec![1, 3]);
        assert!(n.remove(1));
        assert_eq!(n, Node::Leaf(3));
        assert!(!n.remove(3), "la última hoja no se quita");
    }

    #[test]
    fn ratio_by_path() {
        let mut n = Node::Leaf(1);
        n.split(1, 2, SplitDir::Horizontal);
        n.split(2, 3, SplitDir::Vertical);
        let (_, d) = layout(&n);
        n.set_ratio(&d[1].path, 0.8);
        n.set_ratio(&d[0].path, 0.01); // se acota
        let Node::Split { ratio, b, .. } = &n else { panic!() };
        assert!((ratio - MIN_RATIO).abs() < 1e-6);
        let Node::Split { ratio: r2, .. } = &**b else { panic!() };
        assert!((r2 - 0.8).abs() < 1e-6);
    }

    #[test]
    fn geometric_navigation() {
        let mut n = Node::Leaf(1);
        n.split(1, 2, SplitDir::Horizontal);
        n.split(2, 3, SplitDir::Vertical);
        let (p, _) = layout(&n);
        assert_eq!(neighbor(&p, 1, Nav::Right), Some(2));
        assert_eq!(neighbor(&p, 3, Nav::Left), Some(1));
        assert_eq!(neighbor(&p, 2, Nav::Down), Some(3));
        assert_eq!(neighbor(&p, 3, Nav::Up), Some(2));
        assert_eq!(neighbor(&p, 1, Nav::Left), None);
    }
}

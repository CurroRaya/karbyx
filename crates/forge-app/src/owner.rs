// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Quién hace un trabajo de la mesa: un agente en una terminal (su panel) o
//! una conversación con un modelo local. Los dos comparten el mismo tipo de
//! número, así que la revisión, la verificación y los +/− valen para ambos.

use std::path::PathBuf;

use super::App;
use crate::panes::PaneId;
use crate::worktree::Worktree;

/// Marca de las conversaciones (los paneles se numeran desde 1).
const CHAT: PaneId = 1 << 62;

pub fn of_chat(chat: u64) -> PaneId {
    CHAT | chat
}

/// La conversación, si no es un panel.
pub fn as_chat(owner: PaneId) -> Option<u64> {
    (owner & CHAT != 0).then_some(owner & !CHAT)
}

impl App {
    pub(super) fn worktree_of(&self, owner: PaneId) -> Option<Worktree> {
        match as_chat(owner) {
            Some(id) => self.chats.by_id(id)?.worktree.clone(),
            None => self.panes.get(&owner)?.worktree.clone(),
        }
    }

    /// Carpeta donde trabaja.
    pub(super) fn dir_of(&self, owner: PaneId) -> Option<PathBuf> {
        match as_chat(owner) {
            Some(id) => self.chats.by_id(id)?.root.clone(),
            None => self.panes.get(&owner)?.info.cwd.clone(),
        }
    }

    /// Tras fusionar, el punto de partida es el commit nuevo.
    pub(super) fn set_base_of(&mut self, owner: PaneId, head: &str) {
        match as_chat(owner) {
            Some(id) => self.set_chat_base(id, head),
            None => {
                if let Some(w) = self.panes.get_mut(&owner).and_then(|p| p.worktree.as_mut()) {
                    w.base = head.to_string();
                }
            }
        }
    }

    /// La tarea.
    pub(super) fn title_of(&self, owner: PaneId) -> Option<String> {
        match as_chat(owner) {
            Some(id) => self.chats.by_id(id).map(|c| c.title.clone()).filter(|t| !t.is_empty()),
            None => {
                let p = self.panes.get(&owner)?;
                p.agent.as_ref().and_then(|a| a.task.clone()).or_else(|| p.label.clone())
            }
        }
    }

    pub(super) fn project_of_owner(&self, owner: PaneId) -> String {
        match as_chat(owner) {
            Some(id) => self.chats.by_id(id).map(|c| c.project()).unwrap_or_default(),
            None => self.panes.get(&owner).map(|p| p.project.clone()).unwrap_or_default(),
        }
    }

    pub(super) fn go_to_owner(&mut self, owner: PaneId) {
        match as_chat(owner) {
            Some(id) => self.go_to_chat(id),
            None => self.go_to_pane(owner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panes_and_chats_do_not_mix() {
        assert_eq!(as_chat(of_chat(7)), Some(7));
        assert_eq!(as_chat(7), None);
        assert_ne!(of_chat(7), 7);
    }
}

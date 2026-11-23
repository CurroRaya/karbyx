// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Respuestas de la ventana a las consultas de los agentes (vía `forge-app
//! mcp`). Solo lectura: un agente puede ver las terminales, nunca escribir en
//! ellas.

use serde_json::{Value, json};

use super::{App, PaneKind, state_text};
use crate::agents::QueryMsg;
use crate::panes::PaneId;
use forge_term::proc::tilde;

/// Límite de líneas que se devuelven de una terminal.
const MAX_LINES: usize = 2000;
/// Límite de caracteres de la salida de un comando.
const MAX_OUTPUT: usize = 60_000;

fn error(msg: impl Into<String>) -> Value {
    json!({ "error": msg.into() })
}

/// Recorta por el principio, conservando lo último (lo más relevante).
fn tail_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let rest: String = s.chars().skip(n - max).collect();
    format!("[… {} caracteres anteriores omitidos]\n{rest}", n - max)
}

impl App {
    pub(super) fn on_query(&self, msg: QueryMsg) {
        let answer = self.answer(msg.pane, &msg.query);
        let _ = msg.reply.send(answer);
    }

    fn answer(&self, from: PaneId, q: &Value) -> Value {
        let num = |k: &str| q.get(k).and_then(Value::as_u64);
        match q.get("tool").and_then(Value::as_str).unwrap_or_default() {
            "list_terminals" => self.list_terminals(from),
            "read_terminal" => match self.target(from, num("pane")) {
                Ok(id) => {
                    let lines = num("lines").unwrap_or(100).clamp(1, MAX_LINES as u64) as usize;
                    let p = &self.panes[&id];
                    json!({ "pane": id, "text": p.session.tail_text(lines) })
                }
                Err(e) => error(e),
            },
            "list_commands" => match self.target(from, num("pane")) {
                Ok(id) => {
                    let limit = num("limit").unwrap_or(20).clamp(1, 200) as usize;
                    let cmds: Vec<Value> = self.panes[&id]
                        .session
                        .recent_blocks(limit)
                        .into_iter()
                        .map(|(bid, i)| {
                            json!({
                                "id": bid,
                                "command": i.command,
                                "exit_code": i.exit,
                                "running": i.running(),
                                "duration_ms": i.duration().map(|d| d.as_millis() as u64),
                            })
                        })
                        .collect();
                    json!({ "pane": id, "commands": cmds })
                }
                Err(e) => error(e),
            },
            "read_command" => match self.target(from, num("pane")) {
                Ok(id) => {
                    self.read_command(id, num("id"), q.get("failed").and_then(Value::as_bool) == Some(true))
                }
                Err(e) => error(e),
            },
            other => error(format!("consulta desconocida: {other}")),
        }
    }

    /// Panel sobre el que actuar: el pedido o, si no se indica, la terminal
    /// que el usuario tiene delante (o la última usada), nunca la del agente.
    fn target(&self, from: PaneId, asked: Option<u64>) -> Result<PaneId, String> {
        if let Some(id) = asked {
            return if self.panes.contains_key(&id) {
                Ok(id)
            } else {
                Err(format!("no existe la terminal {id}; usa list_terminals"))
            };
        }
        let focused = self.tabs.get(self.active).map(|t| t.focus);
        if let Some(f) = focused
            && f != from
            && self.panes.get(&f).is_some_and(|p| p.title().1.is_none())
        {
            return Ok(f);
        }
        let others = || self.panes.iter().filter(|(id, _)| **id != from);
        others()
            .filter(|(_, p)| p.kind == PaneKind::Shell && p.title().1.is_none())
            .min_by_key(|(_, p)| p.session.idle_for())
            .or_else(|| others().min_by_key(|(_, p)| p.session.idle_for()))
            .map(|(id, _)| *id)
            .ok_or_else(|| "no hay otras terminales abiertas".to_string())
    }

    fn list_terminals(&self, from: PaneId) -> Value {
        let focused = self.tabs.get(self.active).map(|t| t.focus);
        let mut out = Vec::new();
        for (pos, ti) in self.display_order().into_iter().enumerate() {
            for id in self.tabs[ti].root.leaves() {
                let Some(p) = self.panes.get(&id) else { continue };
                let (title, ai) = p.title();
                let agent = p.agent.as_ref().map(|a| {
                    json!({
                        "state": state_text(a.state),
                        "task": a.task,
                        "detail": a.detail,
                    })
                });
                out.push(json!({
                    "pane": id,
                    "tab": pos + 1,
                    "title": title,
                    "project": p.project,
                    "cwd": p.info.cwd.as_deref().map(tilde),
                    "assistant": ai.map(|a| a.name()),
                    "agent": agent,
                    "worktree": p.worktree.as_ref().map(|w| json!({ "branch": w.branch, "path": tilde(&w.path) })),
                    "focused": focused == Some(id),
                    "is_you": id == from,
                }));
            }
        }
        json!({ "you": from, "terminals": out })
    }

    fn read_command(&self, pane: PaneId, id: Option<u64>, failed: bool) -> Value {
        let p = &self.panes[&pane];
        let recent = p.session.recent_blocks(200);
        let pick = match id {
            Some(id) => recent.iter().find(|(b, _)| *b == id),
            None if failed => recent.iter().rev().find(|(_, i)| i.failed()),
            // El último comando terminado (o el que está en marcha si no hay otro).
            None => recent.iter().rev().find(|(_, i)| !i.running()).or(recent.last()),
        };
        let Some((bid, info)) = pick else {
            let why = if failed { "no hay comandos fallidos" } else { "no hay comandos en esa terminal" };
            return error(format!("{why} (los bloques necesitan zsh con la integración de forge)"));
        };
        let Some(text) = p.session.block_text(*bid) else {
            return error("el comando ya no está en el historial");
        };
        json!({
            "pane": pane,
            "id": bid,
            "command": text.command,
            "exit_code": text.exit,
            "running": info.running(),
            "output": tail_chars(&text.output, MAX_OUTPUT),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail_chars("hola", 10), "hola");
        let t = tail_chars("abcdefghij", 3);
        assert!(t.ends_with("\nhij"));
        assert!(t.contains("7 caracteres"));
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! `karbyx-app mcp`: servidor MCP (Model Context Protocol) por stdio.
//!
//! El asistente (Claude Code, o cualquier cliente MCP) lo arranca como
//! subproceso dentro de un panel de Karbyx. Cada llamada a una herramienta se
//! traduce en una consulta a la ventana por el socket de Karbyx
//! (`KARBYX_SOCKET`), que responde con el estado real de las terminales.
//! Todas las herramientas son de solo lectura.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

use crate::agents::query_karbyx;

const PROTOCOL: &str = "2025-06-18";

const INSTRUCTIONS: &str = "Estás dentro de Karbyx, una terminal con varias pestañas y paneles. \
Con estas herramientas puedes ver las otras terminales del usuario: qué hay abierto, \
qué comandos ha ejecutado, su salida y su código de salida, y qué hacen los otros agentes. \
Úsalas cuando el usuario hable de «el error de la otra terminal», «lo que acabo de ejecutar», \
«el servidor que tengo abierto», etc., en vez de pedirle que copie la salida.";

/// Herramientas que expone el servidor.
pub fn tools() -> Value {
    let pane = json!({
        "type": "integer",
        "description": "Id de la terminal (de list_terminals). Si se omite: la terminal que el usuario tiene delante o la última que usó (nunca la tuya)."
    });
    json!([
        {
            "name": "list_terminals",
            "title": "Terminales abiertas",
            "description": "Lista las terminales abiertas en Karbyx: id, pestaña, proyecto, carpeta, asistente que corre en ella y estado del agente (trabajando, te espera, terminado) con su tarea. Marca cuál tiene el foco y cuál eres tú.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "read_terminal",
            "title": "Leer una terminal",
            "description": "Devuelve las últimas líneas de texto de una terminal (historial y pantalla), tal como las ve el usuario.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": pane,
                    "lines": { "type": "integer", "description": "Cuántas líneas (por defecto 100, máximo 2000)." }
                }
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "list_commands",
            "title": "Comandos ejecutados",
            "description": "Lista los últimos comandos ejecutados en una terminal con su código de salida, duración y si siguen en marcha.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": pane,
                    "limit": { "type": "integer", "description": "Cuántos comandos (por defecto 20)." }
                }
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "read_command",
            "title": "Salida de un comando",
            "description": "Devuelve un comando, su salida completa y su código de salida. Por defecto el último comando terminado; con failed=true, el último que falló.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pane": pane,
                    "id": { "type": "integer", "description": "Id del comando (de list_commands)." },
                    "failed": { "type": "boolean", "description": "El último comando con código de salida distinto de 0." }
                }
            },
            "annotations": { "readOnlyHint": true }
        }
    ])
}

/// Convierte la respuesta de Karbyx en texto para el modelo.
fn render(tool: &str, v: &Value) -> (String, bool) {
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        return (e.to_string(), true);
    }
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
    match tool {
        "read_terminal" => {
            let pane = v.get("pane").and_then(Value::as_u64).unwrap_or(0);
            (format!("[terminal {pane}]\n{}", s("text")), false)
        }
        "read_command" => {
            let exit =
                match (v.get("running").and_then(Value::as_bool), v.get("exit_code").and_then(Value::as_i64))
                {
                    (Some(true), _) => "en marcha".to_string(),
                    (_, Some(c)) => format!("código de salida {c}"),
                    _ => "código de salida desconocido".to_string(),
                };
            let pane = v.get("pane").and_then(Value::as_u64).unwrap_or(0);
            (format!("[terminal {pane}] $ {}\n({exit})\n\n{}", s("command"), s("output")), false)
        }
        _ => (serde_json::to_string_pretty(v).unwrap_or_default(), false),
    }
}

fn call_tool(params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let known = tools().as_array().is_some_and(|t| t.iter().any(|t| t["name"] == name));
    let (text, is_error) = if !known {
        (format!("herramienta desconocida: {name}"), true)
    } else {
        let mut q = params.get("arguments").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
        q["tool"] = json!(name);
        match query_karbyx(q) {
            Ok(v) => render(name, &v),
            Err(e) => (e, true),
        }
    };
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// Respuesta a un mensaje JSON-RPC, o `None` si es una notificación.
pub fn handle(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let version = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "karbyx", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => call_tool(&params),
        _ => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("método no soportado: {method}") }
            }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// Bucle principal: un mensaje JSON por línea en stdin, respuestas en stdout.
pub fn run() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg),
            Err(_) => Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": "JSON inválido" }
            })),
        };
        if let Some(r) = reply
            && (writeln!(out, "{r}").is_err() || out.flush().is_err())
        {
            break;
        }
    }
}

/// Configuración MCP para `claude --mcp-config`.
pub fn claude_config(exe: &std::path::Path) -> Value {
    json!({ "mcpServers": { "karbyx": { "type": "stdio", "command": exe, "args": ["mcp"] } } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_and_list() {
        let r = handle(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
        )
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], "karbyx");
        assert!(handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());
        let r = handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        let names: Vec<&str> =
            r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["list_terminals", "read_terminal", "list_commands", "read_command"]);
        let r = handle(&json!({"jsonrpc":"2.0","id":3,"method":"nope"})).unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }

    #[test]
    fn unknown_tool_is_an_error() {
        let r = call_tool(&json!({"name":"rm_rf"}));
        assert_eq!(r["isError"], true);
    }

    #[test]
    fn renders_command_output() {
        let v = json!({"pane":3,"command":"cargo test","exit_code":101,"running":false,"output":"boom"});
        let (t, err) = render("read_command", &v);
        assert!(!err);
        assert!(t.contains("$ cargo test") && t.contains("código de salida 101") && t.ends_with("boom"));
        let (t, err) = render("read_terminal", &json!({"error":"no existe"}));
        assert!(err && t == "no existe");
    }
}

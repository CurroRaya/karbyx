// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Estado de los agentes de IA que corren en cada panel.
//!
//! - **Claude Code** informa con precisión mediante *hooks*: forge lo lanza con
//!   `--settings <archivo>` y cada evento ejecuta `forge-app hook`, que manda
//!   el JSON del hook por un socket local a la ventana de forge.
//! - **Resto de agentes**: heurística por actividad de la salida.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Qué está haciendo el agente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Generando, usando herramientas...
    Working,
    /// Necesita algo de ti (permiso, una respuesta).
    Waiting,
    /// Ha terminado su turno.
    Done,
}

/// Estado de un agente en un panel.
#[derive(Debug, Clone)]
pub struct AgentStatus {
    pub state: AgentState,
    /// Resumen de la tarea (la última petición del usuario).
    pub task: Option<String>,
    /// Lo que está haciendo ahora o por qué espera.
    pub detail: Option<String>,
    pub since: Instant,
    /// Cambió a esperar/terminado sin que lo hayas visto.
    pub unseen: bool,
    /// El estado viene de hooks (preciso) y no de la heurística.
    pub from_hooks: bool,
    /// Id de la sesión del asistente (Claude lo manda en cada hook): enlaza
    /// el agente con su consumo de tokens en el historial.
    pub session: Option<String>,
}

/// Evento de un hook de Claude Code, ya interpretado.
#[derive(Debug, Clone, PartialEq)]
pub enum HookEvent {
    Prompt(String),
    Tool(String),
    Notification(String),
    Stop,
    SessionEnd,
    Other,
}

/// Recorta a `max` caracteres en una sola línea.
pub fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

/// Traduce los avisos habituales de Claude Code ("needs your permission to
/// use Bash", "is waiting for your input"); el resto se deja tal cual.
pub fn translate_notice(msg: &str) -> String {
    let m = msg.trim();
    if let Some(i) = m.find("needs your permission to use ") {
        let tool = m[i + "needs your permission to use ".len()..].trim_end_matches('.');
        return format!("Necesita permiso para usar {tool}");
    }
    if m.contains("needs your permission") || m.contains("needs your approval") {
        return "Necesita tu permiso".into();
    }
    if m.contains("waiting for your input") {
        return "Espera tu respuesta".into();
    }
    m.to_string()
}

/// Describe en español la herramienta que usa el agente.
pub fn describe_tool(name: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    match name {
        "Bash" => match s("description").or(s("command")) {
            Some(c) => format!("Ejecutando: {}", one_line(c, 60)),
            None => "Ejecutando un comando".into(),
        },
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => match s("file_path").or(s("notebook_path")) {
            Some(p) => format!("Editando {}", file_name(p)),
            None => "Editando archivos".into(),
        },
        "Read" => match s("file_path") {
            Some(p) => format!("Leyendo {}", file_name(p)),
            None => "Leyendo archivos".into(),
        },
        "Grep" | "Glob" => match s("pattern") {
            Some(p) => format!("Buscando {}", one_line(p, 40)),
            None => "Buscando en el código".into(),
        },
        "WebFetch" | "WebSearch" => "Consultando la web".into(),
        "Task" | "Agent" => "Lanzando un subagente".into(),
        "TodoWrite" => "Planificando".into(),
        other => format!("Usando {other}"),
    }
}

/// Interpreta el JSON que Claude Code pasa a un hook.
pub fn parse_hook(v: &Value) -> HookEvent {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match v.get("hook_event_name").and_then(Value::as_str).unwrap_or_default() {
        "UserPromptSubmit" => HookEvent::Prompt(s("prompt")),
        "PreToolUse" => {
            HookEvent::Tool(describe_tool(&s("tool_name"), v.get("tool_input").unwrap_or(&Value::Null)))
        }
        "Notification" => HookEvent::Notification(translate_notice(&s("message"))),
        "Stop" => HookEvent::Stop,
        "SessionEnd" => HookEvent::SessionEnd,
        _ => HookEvent::Other,
    }
}

impl AgentStatus {
    fn new(state: AgentState, from_hooks: bool) -> Self {
        Self {
            state,
            task: None,
            detail: None,
            since: Instant::now(),
            unseen: false,
            from_hooks,
            session: None,
        }
    }

    fn set_state(&mut self, state: AgentState, visible: bool) {
        if self.state != state {
            self.state = state;
            self.since = Instant::now();
            self.unseen = !visible && state != AgentState::Working;
        }
    }
}

/// Aplica un evento de hook. Devuelve el estado nuevo, o `None` si la
/// sesión de Claude terminó. `visible`: el panel está a la vista.
pub fn apply_hook(prev: Option<AgentStatus>, ev: HookEvent, visible: bool) -> Option<AgentStatus> {
    let mut st = prev.filter(|p| p.from_hooks).unwrap_or_else(|| AgentStatus::new(AgentState::Done, true));
    match ev {
        HookEvent::Prompt(p) => {
            let p = p.trim();
            if !p.is_empty() {
                st.task = Some(one_line(p, 80));
            }
            st.detail = None;
            st.set_state(AgentState::Working, visible);
        }
        HookEvent::Tool(d) => {
            st.detail = Some(d);
            st.set_state(AgentState::Working, visible);
        }
        HookEvent::Notification(m) => {
            st.detail = (!m.trim().is_empty()).then(|| one_line(&m, 90));
            st.set_state(AgentState::Waiting, visible);
        }
        HookEvent::Stop => {
            st.detail = None;
            st.set_state(AgentState::Done, visible);
        }
        HookEvent::SessionEnd => return None,
        HookEvent::Other => {}
    }
    Some(st)
}

/// Sin salida durante este tiempo, un agente sin hooks se considera parado.
pub const IDLE_AFTER: Duration = Duration::from_millis(2500);

/// Tiempo mínimo trabajando para que, al pararse, cuente como "te espera".
pub const MIN_WORK: Duration = Duration::from_secs(3);

/// Estado por heurística para agentes sin hooks (Codex, Gemini...).
pub fn heuristic(prev: Option<AgentStatus>, idle: Duration, visible: bool) -> AgentStatus {
    let working = idle < IDLE_AFTER;
    let mut st = prev.filter(|p| !p.from_hooks).unwrap_or_else(|| AgentStatus::new(AgentState::Done, false));
    if working {
        st.set_state(AgentState::Working, visible);
    } else if st.state == AgentState::Working {
        // Sin hooks no se distingue "te espera" de "terminado": si trabajó un
        // rato, queda a la espera; un parpadeo de salida no cuenta.
        let worked = st.since.elapsed() >= MIN_WORK;
        st.set_state(if worked { AgentState::Waiting } else { AgentState::Done }, visible);
        if !worked {
            st.unseen = false;
        }
    }
    st
}

// ---------- canal forge ↔ hooks ----------

/// Mensaje que `forge-app hook` envía a la ventana.
#[derive(Debug)]
pub struct HookMsg {
    pub pane: u64,
    pub event: HookEvent,
    pub session: Option<String>,
}

fn runtime_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|c| c.join("forge"))
}

/// Consulta de un cliente MCP (`forge-app mcp`) sobre el estado de forge.
#[derive(Debug)]
pub struct QueryMsg {
    /// Panel desde el que pregunta el agente.
    pub pane: u64,
    pub query: Value,
    pub reply: std::sync::mpsc::Sender<Value>,
}

/// Lo que llega por el socket.
#[derive(Debug)]
pub enum Incoming {
    Hook(HookMsg),
    Query(QueryMsg),
}

/// Tiempo máximo que un cliente espera la respuesta de la ventana.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

fn handle_conn(mut stream: UnixStream, on_msg: &impl Fn(Incoming)) {
    let mut buf = Vec::new();
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = (&stream).take(1 << 20).read_to_end(&mut buf);
    let Ok(v) = serde_json::from_slice::<Value>(&buf) else { return };
    let Some(pane) = v.get("pane").and_then(Value::as_u64) else { return };
    if let Some(query) = v.get("query") {
        let (tx, rx) = std::sync::mpsc::channel();
        on_msg(Incoming::Query(QueryMsg { pane, query: query.clone(), reply: tx }));
        let answer = rx
            .recv_timeout(QUERY_TIMEOUT)
            .unwrap_or_else(|_| serde_json::json!({ "error": "forge no responde" }));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let _ = stream.write_all(answer.to_string().as_bytes());
        return;
    }
    let ev = v.get("event").unwrap_or(&Value::Null);
    let session = ev.get("session_id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
    on_msg(Incoming::Hook(HookMsg { pane, event: parse_hook(ev), session }));
}

/// Arranca el servidor del socket; `on_msg` se llama por cada hook o consulta
/// recibida. Devuelve la ruta del socket. La carpeta es solo del usuario.
pub fn start_server(on_msg: impl Fn(Incoming) + Send + 'static) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = runtime_dir()?.join("sock");
    std::fs::create_dir_all(&dir).ok()?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let path = dir.join(format!("{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).ok()?;
    std::thread::Builder::new()
        .name("agent-hooks".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                handle_conn(stream, &on_msg);
            }
        })
        .ok()?;
    Some(path)
}

/// Envía una consulta a la ventana de forge y espera la respuesta.
pub fn query_forge(query: Value) -> Result<Value, String> {
    let sock =
        std::env::var("FORGE_SOCKET").map_err(|_| "no se está ejecutando dentro de forge".to_string())?;
    let pane = std::env::var("FORGE_PANE").ok().and_then(|p| p.parse::<u64>().ok()).unwrap_or(0);
    query_at(&sock, pane, query)
}

fn query_at(sock: &str, pane: u64, query: Value) -> Result<Value, String> {
    let mut s = UnixStream::connect(sock).map_err(|e| format!("no se puede conectar con forge: {e}"))?;
    let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
    let _ = s.set_read_timeout(Some(QUERY_TIMEOUT + Duration::from_secs(1)));
    let msg = serde_json::json!({ "pane": pane, "query": query });
    s.write_all(msg.to_string().as_bytes()).map_err(|e| e.to_string())?;
    s.shutdown(std::net::Shutdown::Write).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    s.take(64 << 20).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    serde_json::from_slice(&buf).map_err(|_| "respuesta de forge ilegible".to_string())
}

/// Borra el socket al salir.
pub fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// `forge-app hook`: lee el JSON del hook por stdin y lo manda a forge. No
/// imprime nada (Claude añadiría la salida a su contexto) y nunca falla.
pub fn run_hook_client() {
    let mut input = String::new();
    let _ = std::io::stdin().take(1 << 20).read_to_string(&mut input);
    let (Ok(sock), Ok(pane)) = (std::env::var("FORGE_SOCKET"), std::env::var("FORGE_PANE")) else { return };
    let Ok(pane) = pane.parse::<u64>() else { return };
    let event: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let msg = serde_json::json!({ "pane": pane, "event": event });
    if let Ok(mut s) = UnixStream::connect(sock) {
        let _ = s.set_write_timeout(Some(Duration::from_secs(1)));
        let _ = s.write_all(msg.to_string().as_bytes());
    }
}

fn write_if_changed(file: &Path, text: &str) -> Option<()> {
    if std::fs::read_to_string(file).ok().as_deref() != Some(text) {
        std::fs::write(file, text).ok()?;
    }
    Some(())
}

/// Escribe los archivos para Claude Code: ajustes con los hooks
/// (`--settings`) y el servidor MCP de forge (`--mcp-config`). Las
/// herramientas de forge son de solo lectura y se permiten sin preguntar.
pub fn write_claude_settings(exe: &Path) -> Option<(PathBuf, PathBuf)> {
    let dir = runtime_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let file = dir.join("claude-hooks.json");
    let command = format!("'{}' hook", exe.to_string_lossy().replace('\'', "'\\''"));
    let hook = serde_json::json!([{ "hooks": [{ "type": "command", "command": command, "timeout": 5 }] }]);
    let tool = serde_json::json!([{ "matcher": "*", "hooks": [{ "type": "command", "command": command, "timeout": 5 }] }]);
    let settings = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": hook,
            "PreToolUse": tool,
            "Notification": hook,
            "Stop": hook,
            "SessionEnd": hook,
        },
        "permissions": { "allow": ["mcp__forge"] }
    });
    write_if_changed(&file, &serde_json::to_string_pretty(&settings).ok()?)?;
    let mcp = dir.join("claude-mcp.json");
    write_if_changed(&mcp, &serde_json::to_string_pretty(&crate::mcp::claude_config(exe)).ok()?)?;
    Some((file, mcp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hook_parsing() {
        assert_eq!(
            parse_hook(&json!({"hook_event_name": "UserPromptSubmit", "prompt": "arregla el login"})),
            HookEvent::Prompt("arregla el login".into())
        );
        assert_eq!(
            parse_hook(
                &json!({"hook_event_name": "PreToolUse", "tool_name": "Edit", "tool_input": {"file_path": "/x/src/ui.rs"}})
            ),
            HookEvent::Tool("Editando ui.rs".into())
        );
        assert_eq!(
            parse_hook(
                &json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "cargo test"}})
            ),
            HookEvent::Tool("Ejecutando: cargo test".into())
        );
        assert_eq!(
            parse_hook(
                &json!({"hook_event_name": "Notification", "message": "Claude needs your permission"})
            ),
            HookEvent::Notification("Necesita tu permiso".into())
        );
        assert_eq!(parse_hook(&json!({"hook_event_name": "Stop"})), HookEvent::Stop);
        assert_eq!(parse_hook(&json!({})), HookEvent::Other);
    }

    #[test]
    fn hook_state_machine() {
        let s = apply_hook(None, HookEvent::Prompt("  arregla\n el   login ".into()), true).unwrap();
        assert_eq!(s.state, AgentState::Working);
        assert_eq!(s.task.as_deref(), Some("arregla el login"));
        let s = apply_hook(Some(s), HookEvent::Tool("Editando a.rs".into()), true).unwrap();
        assert_eq!(s.detail.as_deref(), Some("Editando a.rs"));
        // Espera sin que lo veas: queda marcado como no visto.
        let s = apply_hook(Some(s), HookEvent::Notification("necesita permiso".into()), false).unwrap();
        assert_eq!(s.state, AgentState::Waiting);
        assert!(s.unseen);
        let s = apply_hook(Some(s), HookEvent::Stop, true).unwrap();
        assert_eq!(s.state, AgentState::Done);
        assert!(!s.unseen, "visto: no se marca");
        assert_eq!(s.task.as_deref(), Some("arregla el login"), "la tarea se conserva");
        assert!(apply_hook(Some(s), HookEvent::SessionEnd, true).is_none());
    }

    #[test]
    fn heuristic_states() {
        let s = heuristic(None, Duration::from_secs(10), true);
        assert_eq!(s.state, AgentState::Done, "recién abierto y quieto");
        let s = heuristic(Some(s), Duration::from_millis(100), false);
        assert_eq!(s.state, AgentState::Working);
        // Un parpadeo de salida (menos de MIN_WORK trabajando) no es "te espera".
        let blip = heuristic(Some(s.clone()), Duration::from_secs(5), false);
        assert_eq!(blip.state, AgentState::Done);
        assert!(!blip.unseen);
        // Tras trabajar un rato, al pararse queda a la espera y sin ver.
        let mut long = s;
        long.since -= MIN_WORK;
        let w = heuristic(Some(long), Duration::from_secs(5), false);
        assert_eq!(w.state, AgentState::Waiting);
        assert!(w.unseen);
    }

    #[test]
    fn notices_in_spanish() {
        assert_eq!(
            translate_notice("Claude needs your permission to use Bash"),
            "Necesita permiso para usar Bash"
        );
        assert_eq!(translate_notice("Claude is waiting for your input"), "Espera tu respuesta");
        assert_eq!(translate_notice("Algo distinto"), "Algo distinto");
    }

    #[test]
    fn one_line_truncates() {
        assert_eq!(one_line("a\n  b", 10), "a b");
        assert_eq!(one_line("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn socket_roundtrip() {
        // El sandbox de la verificación no deja crear el socket de forge.
        if std::env::var_os("FORGE_VERIFY").is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let path = start_server(move |m| {
            let _ = tx.send(m);
        })
        .expect("socket");
        let mut s = UnixStream::connect(&path).unwrap();
        s.write_all(json!({"pane": 7, "event": {"hook_event_name": "Stop"}}).to_string().as_bytes()).unwrap();
        drop(s);
        match rx.recv_timeout(Duration::from_secs(2)).expect("mensaje") {
            Incoming::Hook(m) => {
                assert_eq!(m.pane, 7);
                assert_eq!(m.event, HookEvent::Stop);
            }
            other => panic!("esperaba un hook: {other:?}"),
        }
        // Consulta con respuesta (en el mismo test: el socket es uno por proceso).
        let answerer = std::thread::spawn(move || match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Incoming::Query(q)) => {
                let _ = q.reply.send(json!({ "pane": q.pane, "tool": q.query["tool"] }));
            }
            other => panic!("esperaba una consulta: {other:?}"),
        });
        let v = query_at(path.to_str().unwrap(), 9, json!({"tool": "list_terminals"})).expect("respuesta");
        assert_eq!(v, json!({ "pane": 9, "tool": "list_terminals" }));
        answerer.join().unwrap();
        cleanup(&path);
    }
}

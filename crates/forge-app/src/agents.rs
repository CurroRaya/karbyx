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
        Self { state, task: None, detail: None, since: Instant::now(), unseen: false, from_hooks }
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
}

fn runtime_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|c| c.join("forge"))
}

/// Arranca el servidor del socket; `on_msg` se llama por cada hook recibido.
/// Devuelve la ruta del socket.
pub fn start_server(on_msg: impl Fn(HookMsg) + Send + 'static) -> Option<PathBuf> {
    let dir = runtime_dir()?.join("sock");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).ok()?;
    std::thread::Builder::new()
        .name("agent-hooks".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut buf = Vec::new();
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = (&stream).take(1 << 20).read_to_end(&mut buf);
                let Ok(v) = serde_json::from_slice::<Value>(&buf) else { continue };
                let Some(pane) = v.get("pane").and_then(Value::as_u64) else { continue };
                let event = parse_hook(v.get("event").unwrap_or(&Value::Null));
                on_msg(HookMsg { pane, event });
            }
        })
        .ok()?;
    Some(path)
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

/// Escribe el archivo de ajustes con los hooks para `claude --settings`.
pub fn write_claude_settings(exe: &Path) -> Option<PathBuf> {
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
        }
    });
    let text = serde_json::to_string_pretty(&settings).ok()?;
    if std::fs::read_to_string(&file).ok().as_deref() != Some(text.as_str()) {
        std::fs::write(&file, text).ok()?;
    }
    Some(file)
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
        let (tx, rx) = std::sync::mpsc::channel();
        let path = start_server(move |m| {
            let _ = tx.send(m);
        })
        .expect("socket");
        let mut s = UnixStream::connect(&path).unwrap();
        s.write_all(json!({"pane": 7, "event": {"hook_event_name": "Stop"}}).to_string().as_bytes()).unwrap();
        drop(s);
        let m = rx.recv_timeout(Duration::from_secs(2)).expect("mensaje");
        assert_eq!(m.pane, 7);
        assert_eq!(m.event, HookEvent::Stop);
        cleanup(&path);
    }
}

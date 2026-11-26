// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! `karbyx`: un agente de programación con modelos locales (chat), una mesa
//! para dirigir a otros agentes y una terminal acelerada por GPU.

mod agents;
mod board;
mod capture;
mod chat;
mod config;
mod gfx;
mod input;
mod keys;
mod mcp;
mod owner;
mod panes;
mod preview;
mod query;
mod rects;
mod review;
mod ui;
mod usage;
mod verify;
mod worktree;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use karbyx_ai::Provider;
use karbyx_term::mouse::{self, MouseAction, MouseButton as TermButton, MouseMods};
use karbyx_term::proc::tilde;
use karbyx_term::{ProcessInfo, SelectionKind, Session, SessionOptions, TermEvent, TermMode, Theme};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::agents::{AgentState, AgentStatus};
use crate::config::Config;
use crate::gfx::{CursorStyle, FontSpec, Gfx, PaneText, RichDraw};
use crate::input::KeyOutcome;
use crate::keys::{Action, Bindings};
use crate::panes::{Divider, Node, PaneId, SplitDir};
use crate::rects::{Rect, linear};
use crate::ui::{Align, Area, Font, Hit, Label, TabView, Ui};
use crate::usage::{SharedUsage, UsageState};

/// Comando de una sesión de asistente: si el asistente termina (o no está
/// instalado), la sesión sigue viva con una shell normal.
fn ai_command(cmd: &str) -> String {
    let run = launch(cmd, "");
    format!(
        "if command -v {cmd} >/dev/null 2>&1; then {run}; \
         else printf '\\033[33mNo encuentro el comando {cmd} en el PATH.\\033[0m\\n'; fi; exec \"$SHELL\" -l"
    )
}

/// Cómo invocar el asistente: Claude lleva los hooks de Karbyx (estado real)
/// y su servidor MCP (para ver las otras terminales). `--mcp-config` admite
/// varios archivos, así que va antes de `--settings` para no tragarse la
/// pregunta.
/// `args` va tal cual al final (ya entrecomillado). Se usa un `if` explícito
/// porque zsh no parte en palabras `${VAR:+--settings "$VAR"}` y Claude
/// recibiría un único argumento "--settings /ruta".
fn launch(cmd: &str, args: &str) -> String {
    let args = if args.is_empty() { String::new() } else { format!(" {args}") };
    if cmd == "claude" {
        format!(
            r#"if [ -n "$KARBYX_CLAUDE_SETTINGS" ]; then claude --mcp-config "$KARBYX_CLAUDE_MCP" --settings "$KARBYX_CLAUDE_SETTINGS"{args}; else claude{args}; fi"#
        )
    } else {
        format!("{cmd}{args}")
    }
}

/// Variables que deja Claude Code (o su app de escritorio) en el entorno si
/// Karbyx se lanza desde ahí: no deben llegar a las sesiones de Karbyx, o el
/// Claude que abras dentro se creería un subproceso de otra sesión.
fn clean_inherited_env() {
    const PREFIXES: [&str; 3] = ["CLAUDECODE", "CLAUDE_CODE_", "CLAUDE_AGENT_SDK_"];
    const EXACT: [&str; 5] = [
        "CLAUDE_PID",
        "CLAUDE_EFFORT",
        "CLAUDE_PREVIEW_CLASSIFIER_FLOOR",
        "AI_AGENT",
        "__CFBundleIdentifier",
    ];
    let doomed: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| PREFIXES.iter().any(|p| k.starts_with(p)) || EXACT.contains(&k.as_str()))
        .collect();
    for k in doomed {
        // SAFETY: se llama al principio de `main`, antes de crear ningún hilo.
        unsafe { std::env::remove_var(k) };
    }
}

/// Lanzada desde el Finder o el Dock, la app no hereda el entorno de una
/// shell: sin `LANG` las terminales no verían la ñ ni los acentos. Se completa
/// con el idioma de macOS, como hace Terminal.
fn complete_gui_env() {
    let lang_set =
        ["LC_ALL", "LC_CTYPE", "LANG"].iter().any(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()));
    if !lang_set {
        let apple = std::process::Command::new("/usr/bin/defaults")
            .args(["read", "-g", "AppleLocale"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default();
        let lang = gui_lang(&apple, |l| std::path::Path::new("/usr/share/locale").join(l).exists());
        // SAFETY: se llama al principio de `main`, antes de crear ningún hilo.
        unsafe { std::env::set_var("LANG", lang) };
    }
    if std::env::var_os("SHELL").is_none_or(|s| s.is_empty()) {
        // SAFETY: igual que arriba.
        unsafe { std::env::set_var("SHELL", "/bin/zsh") };
    }
}

/// Mantener pulsada una tecla la repite, en vez de abrir el menú de acentos
/// de macOS (que aparece al aceptar texto compuesto).
fn disable_press_and_hold() {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSUserDefaults, ns_string};
        NSUserDefaults::standardUserDefaults().setBool_forKey(false, ns_string!("ApplePressAndHoldEnabled"));
    }
}

/// `LANG` a partir de `AppleLocale` («es_ES», «es_ES@rg=eszzzz»...).
fn gui_lang(apple: &str, exists: impl Fn(&str) -> bool) -> String {
    let base = apple.trim().split('@').next().unwrap_or_default();
    let want = format!("{base}.UTF-8");
    if !base.is_empty() && exists(&want) { want } else { "en_US.UTF-8".into() }
}

/// Comando de una sesión de asistente que arranca con una pregunta inicial
/// leída de `prompt` (un archivo, para no tener problemas de comillas).
fn ai_command_with_prompt(p: Provider, prompt: &std::path::Path) -> String {
    let file = prompt.to_string_lossy().replace('\'', "'\\''");
    let ask = match p {
        Provider::Gemini => format!("gemini -i \"$(cat '{file}')\""),
        _ => launch(p.command(), &format!("\"$(cat '{file}')\"")),
    };
    let cmd = p.command();
    format!(
        "if command -v {cmd} >/dev/null 2>&1; then {ask}; \
         else printf '\\033[33mNo encuentro el comando {cmd} en el PATH.\\033[0m\\n'; fi; exec \"$SHELL\" -l"
    )
}

/// Texto de la pregunta a la IA sobre un comando fallido.
fn ask_prompt(text: &karbyx_term::BlockText, cwd: Option<&std::path::Path>) -> String {
    let cmd = text.command.as_deref().unwrap_or("(comando desconocido)");
    let exit = text.exit.map(|c| format!(" con código {c}")).unwrap_or_default();
    let place = cwd.map(|p| format!(" en {}", p.display())).unwrap_or_default();
    let output = if text.output.is_empty() { "(sin salida)".to_string() } else { text.output.clone() };
    format!(
        "Este comando ha fallado{exit} en mi terminal{place}:\n\n$ {cmd}\n\nSalida:\n```\n{output}\n```\n\n\
         ¿Qué ha pasado y cómo lo arreglo?"
    )
}

/// Aviso del sistema (Centro de notificaciones de macOS).
fn notify(title: &str, body: &str) {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title));
    let _ = std::process::Command::new("osascript").arg("-e").arg(script).spawn();
}

/// Mide las fases de un frame y avisa si es lento (>= 25 ms).
struct Profiler {
    start: Instant,
    last: Instant,
    phases: Vec<(&'static str, Duration)>,
}

impl Profiler {
    fn new() -> Self {
        let now = Instant::now();
        Self { start: now, last: now, phases: Vec::with_capacity(8) }
    }

    fn mark(&mut self, name: &'static str) {
        let now = Instant::now();
        self.phases.push((name, now - self.last));
        self.last = now;
    }

    fn report(&self) {
        // `KARBYX_PROFILE=1` registra todos los frames, no solo los lentos.
        static ALL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let all = *ALL.get_or_init(|| std::env::var_os("KARBYX_PROFILE").is_some());
        let total = self.start.elapsed();
        if all || total >= Duration::from_millis(25) {
            let detail: Vec<String> =
                self.phases.iter().map(|(n, d)| format!("{n}={:.1}", d.as_secs_f64() * 1000.0)).collect();
            log::warn!("frame {:.1} ms: {}", total.as_secs_f64() * 1000.0, detail.join(" "));
        }
    }
}

#[derive(Debug)]
enum UserEvent {
    Wake,
    /// Modo de prueba: texto a escribir en el panel activo como si se tecleara.
    Type(String),
    /// `karbyx.toml` ha cambiado.
    Config(Box<Config>),
    /// Un hook de Claude Code informa del estado de un panel.
    Hook(agents::HookMsg),
    /// Consulta de un agente por MCP.
    Query(agents::QueryMsg),
    /// Resultado de git para el panel de revisión.
    Review(review::ReviewEvent),
    /// +/− de los worktrees para la mesa.
    BoardStats(Vec<(PaneId, board::Stats)>),
    /// Terminó la verificación de un agente (panel, generación, resultado).
    Verify(PaneId, u64, verify::Report),
    /// Un agente del chat informa (conversación, evento).
    Chat(u64, karbyx_agent::agent::Event),
    /// Qué modelos tiene un servidor.
    Probe(Box<karbyx_agent::api::Server>, Result<Vec<karbyx_agent::api::Model>, String>),
    /// Herramientas de un servidor MCP.
    McpProbe(String, Box<karbyx_agent::mcp::Config>, Result<Vec<karbyx_agent::mcp::Tool>, String>),
    /// Servidores MCP encontrados (búsqueda, programas instalados, fichas).
    McpFound(String, Vec<&'static str>, Result<Vec<karbyx_agent::registry::Entry>, String>),
    /// Habilidades encontradas para instalar.
    Skills(u64, Box<Result<karbyx_agent::install::Bundle, String>>),
    /// Habilidades encontradas en el catálogo.
    SkillHits(u64, Result<Vec<karbyx_agent::catalog::Hit>, String>),
    /// Carpeta elegida para el chat.
    Folder(PathBuf),
    /// Un aviso breve para el chat.
    Toast(String),
    /// Modo de prueba: mensaje para el chat.
    ChatSay(String),
    /// Bytes para un panel concreto (p. ej. el ↵ tras escribirle a un agente).
    WritePane(PaneId, Vec<u8>),
}

/// `KARBYX_SELFTEST=1`: teclea líneas largas (con salto de línea y scroll) en
/// el panel activo, carácter a carácter, para perfilar el render.
fn spawn_selftest(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        for i in 0..40 {
            let line = format!(
                "echo prueba {i} una línea bastante larga para que salte de línea en la terminal ✓ ◆ — fin"
            );
            for ch in line.chars() {
                let _ = proxy.send_event(UserEvent::Type(ch.to_string()));
                std::thread::sleep(Duration::from_millis(15));
            }
            let _ = proxy.send_event(UserEvent::Type("\r".into()));
            std::thread::sleep(Duration::from_millis(150));
        }
        let _ = proxy.send_event(UserEvent::Type("exit\r".into()));
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneKind {
    Shell,
    Ai(Provider),
}

struct Pane {
    session: Session,
    kind: PaneKind,
    info: ProcessInfo,
    /// Estado del agente de IA que corre en el panel, si hay uno.
    agent: Option<AgentStatus>,
    /// Proyecto (raíz git o carpeta) del directorio actual.
    project: String,
    /// Worktree propio del agente, si se lanzó con ⌘⇧A o desde la mesa.
    worktree: Option<worktree::Worktree>,
    /// Tarea con la que se lanzó desde la mesa (título hasta que el agente diga otro).
    label: Option<String>,
}

/// Nombre del proyecto de una carpeta: la raíz del repositorio git que la
/// contiene o, si no hay, la propia carpeta.
fn project_of(cwd: Option<&std::path::Path>) -> String {
    let Some(cwd) = cwd else { return "sin proyecto".into() };
    let main = project_root(cwd);
    let root = main.as_path();
    if std::env::var_os("HOME").is_some_and(|h| std::path::Path::new(&h) == root) {
        return "~".into();
    }
    root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into())
}

/// Raíz del proyecto de una carpeta: la del repositorio git que la contiene
/// (un worktree cuenta como su repositorio principal) o la propia carpeta.
/// No lanza procesos.
fn project_root(cwd: &std::path::Path) -> PathBuf {
    let root = cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(cwd);
    worktree::main_checkout(root)
}

/// "ahora", "3 min", "2 h", "1 d".
fn ago(since: Instant) -> String {
    let s = since.elapsed().as_secs();
    match s {
        0..=59 => "ahora".into(),
        60..=3599 => format!("{} min", s / 60),
        3600..=86_399 => format!("{} h", s / 3600),
        _ => format!("{} d", s / 86_400),
    }
}

impl Pane {
    fn title(&self) -> (String, Option<Provider>) {
        let name = self.info.name.as_deref().unwrap_or("shell").trim_start_matches('-').to_string();
        // Un asistente lanzado a mano en una terminal también se reconoce.
        let detected = Provider::ALL.into_iter().find(|p| name.contains(p.command()));
        match self.kind {
            PaneKind::Ai(p) => (p.name().to_string(), Some(p)),
            PaneKind::Shell => (name, detected),
        }
    }
}

/// Opciones extra al abrir un panel.
#[derive(Default)]
struct Spawn {
    /// Pregunta inicial para el asistente (archivo).
    prompt: Option<PathBuf>,
    /// Worktree donde arranca (y que queda asociado al panel).
    worktree: Option<worktree::Worktree>,
    /// Aviso que se imprime antes de lanzar el asistente.
    notice: Option<String>,
    /// Carpeta donde arranca (si no hay worktree).
    cwd: Option<PathBuf>,
    /// Título inicial (la tarea).
    label: Option<String>,
}

/// Una pestaña: un árbol de paneles y cuál tiene el foco.
struct Tab {
    root: Node,
    focus: PaneId,
}

/// Arrastre del ratón en curso.
#[derive(Debug, Clone)]
enum Drag {
    Select(PaneId),
    Divider(Divider),
    Report(PaneId),
    /// Selección en el chat (`true`: en la caja de escritura).
    Chat(bool),
}

/// Para detectar doble y triple clic.
#[derive(Debug, Clone, Copy)]
struct LastClick {
    at: Instant,
    pane: PaneId,
    row: usize,
    col: usize,
    count: u8,
}

/// Barra de búsqueda abierta sobre el panel activo.
#[derive(Debug, Clone, Default)]
struct SearchUi {
    query: String,
    found: bool,
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    wake_pending: Arc<AtomicBool>,
    gfx: Option<Gfx>,
    panes: HashMap<PaneId, Pane>,
    tabs: Vec<Tab>,
    active: usize,
    next_id: PaneId,
    usage: Option<SharedUsage>,
    config: Config,
    bindings: Bindings,
    theme: Theme,
    font_delta: f32,
    mods: ModifiersState,
    focused: bool,
    scroll_accum: f64,
    clipboard: Option<arboard::Clipboard>,
    show_left: bool,
    /// Panel de consumo: `None` = automático (visible si la pestaña activa
    /// tiene una sesión de IA); `Some` = forzado a mano con ⌘⌥B en esta pestaña.
    right_override: Option<bool>,
    /// Asistente de la pestaña activa en el último frame.
    last_tab_ai: Option<Provider>,
    /// Asistente seleccionado en el panel derecho.
    usage_sel: usize,
    /// Proveedor a seleccionar en cuanto lleguen los datos (`KARBYX_PROVIDER`).
    pending_provider: Option<Provider>,
    cursor: (f32, f32),
    cursor_icon: CursorIcon,
    hover: Option<Hit>,
    hits: Vec<(Area, Hit)>,
    /// Geometría del último frame: paneles visibles y divisores.
    pane_areas: Vec<(PaneId, Area)>,
    dividers: Vec<Divider>,
    drag: Option<Drag>,
    /// Último fotograma con un arrastre en marcha.
    drag_at: Option<Instant>,
    last_click: Option<LastClick>,
    search: Option<SearchUi>,
    /// Bloque seleccionado (panel, bloque) y bloque bajo el ratón.
    selected_block: Option<(PaneId, u64)>,
    hover_block: Option<(PaneId, u64)>,
    /// Bloques visibles por panel en el último frame: (id, primera fila, última).
    pane_blocks: HashMap<PaneId, Vec<(u64, usize, usize)>>,
    info_refreshed: Instant,
    /// Momento de arranque, para el modo captura.
    started: Instant,
    /// La ventana no se ve (minimizada, oculta o tapada): no se pinta.
    occluded: bool,
    /// Último título puesto, para no tocar la ventana en cada frame.
    window_title: String,
    /// Socket por el que llegan los hooks y ajustes para `claude --settings`.
    agent_socket: Option<PathBuf>,
    /// Archivos para `claude`: ajustes con los hooks y configuración MCP.
    claude_settings: Option<(PathBuf, PathBuf)>,
    /// Bandeja de agentes abierta (⌘J) y fila seleccionada.
    inbox: Option<usize>,
    /// Panel de mando abierto (con el agente seleccionado, si hay).
    dashboard: Option<usize>,
    /// Panel «Revisar cambios» abierto (⌘R).
    review: Option<review::Review>,
    /// La mesa de agentes y si se está viendo (en vez de las terminales).
    board: board::Board,
    board_visible: bool,
    /// Conversaciones con modelos locales y si se están viendo.
    chats: chat::Chats,
    chat_visible: bool,
    /// Último clic en el chat, para el doble clic.
    chat_click_at: Option<(Instant, (f32, f32))>,
    /// Hace falta repintar periódicamente (animaciones, tiempos, heurística).
    tick_needed: Arc<AtomicBool>,
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>, config: Config) -> Self {
        Self {
            proxy,
            wake_pending: Arc::new(AtomicBool::new(false)),
            gfx: None,
            panes: HashMap::new(),
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            usage: None,
            bindings: Bindings::new(&config.keys),
            theme: config.theme(),
            config,
            font_delta: 0.0,
            mods: ModifiersState::empty(),
            focused: true,
            scroll_accum: 0.0,
            clipboard: arboard::Clipboard::new().ok(),
            show_left: true,
            right_override: None,
            last_tab_ai: None,
            usage_sel: 0,
            pending_provider: std::env::var("KARBYX_PROVIDER").ok().and_then(|v| Provider::from_id(&v)),
            cursor: (-1.0, -1.0),
            cursor_icon: CursorIcon::Default,
            hover: None,
            hits: Vec::new(),
            pane_areas: Vec::new(),
            dividers: Vec::new(),
            drag: None,
            drag_at: None,
            last_click: None,
            search: None,
            selected_block: None,
            hover_block: None,
            pane_blocks: HashMap::new(),
            info_refreshed: Instant::now(),
            started: Instant::now(),
            occluded: false,
            window_title: String::new(),
            agent_socket: None,
            claude_settings: None,
            inbox: None,
            dashboard: None,
            review: None,
            board: board::Board::default(),
            board_visible: false,
            chats: chat::Chats::new(),
            chat_visible: false,
            chat_click_at: None,
            tick_needed: Arc::new(AtomicBool::new(false)),
        }
    }

    fn font_spec(&self) -> FontSpec {
        FontSpec {
            family: self.config.font.family.clone(),
            size_pt: (self.config.font_size() + self.font_delta).clamp(6.0, 72.0),
            line_height: self.config.line_height(),
            padding_pt: self.config.window.padding.clamp(0.0, 60.0),
        }
    }

    fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        let proxy = self.proxy.clone();
        let pending = self.wake_pending.clone();
        Arc::new(move || {
            // Agrupa despertares: como mucho uno en vuelo.
            if !pending.swap(true, Ordering::AcqRel) {
                let _ = proxy.send_event(UserEvent::Wake);
            }
        })
    }

    fn usage_state(&self) -> UsageState {
        self.usage.as_ref().and_then(|u| u.lock().ok().map(|g| g.clone())).unwrap_or_default()
    }

    /// Asistente activo: el elegido en el panel, o el primero detectado.
    fn selected_provider(&self) -> Option<Provider> {
        let st = self.usage_state();
        if st.loaded {
            return st.views.get(self.usage_sel.min(st.views.len().saturating_sub(1))).map(|v| v.provider);
        }
        Provider::detect_all().into_iter().next()
    }

    fn redraw(&self) {
        if self.occluded {
            return;
        }
        if let Some(g) = &self.gfx {
            g.window.request_redraw();
        }
    }

    fn layout(&self) -> Option<ui::Layout> {
        let g = self.gfx.as_ref()?;
        Some(ui::layout(
            g.config.width as f32,
            g.config.height as f32,
            g.scale(),
            self.show_left,
            self.right_visible(),
        ))
    }

    /// Asistente de IA que corre en la pestaña activa (el del panel con foco
    /// si lo tiene, si no el de cualquier otro panel de la pestaña).
    fn tab_ai(&self) -> Option<Provider> {
        let tab = self.tabs.get(self.active)?;
        let ai_of = |id: &PaneId| self.panes.get(id).and_then(|p| p.title().1);
        ai_of(&tab.focus).or_else(|| tab.root.leaves().iter().find_map(ai_of))
    }

    fn right_visible(&self) -> bool {
        right_panel_visible(self.right_override, self.tab_ai().is_some())
    }

    /// Cambia la pestaña activa; el ajuste manual del panel no se arrastra.
    fn set_active(&mut self, i: usize) {
        if i != self.active {
            self.right_override = None;
        }
        self.active = i;
    }

    fn focused_id(&self) -> Option<PaneId> {
        self.tabs.get(self.active).map(|t| t.focus)
    }

    fn focused_pane(&self) -> Option<&Pane> {
        self.focused_id().and_then(|id| self.panes.get(&id))
    }

    fn set_focus(&mut self, id: PaneId) {
        if self.focused_id() != Some(id) {
            self.close_search();
        }
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.focus = id;
        }
    }

    /// Directorio para una sesión nueva: el del panel activo, como iTerm2.
    fn new_session_cwd(&self) -> Option<PathBuf> {
        if let Some(cwd) = self.focused_pane().and_then(|p| p.info.cwd.clone()) {
            return Some(cwd);
        }
        let cwd = std::env::current_dir().ok();
        // Lanzada desde Finder el cwd es "/": mejor el home.
        match cwd {
            Some(p) if p != std::path::Path::new("/") => Some(p),
            _ => std::env::var_os("HOME").map(PathBuf::from),
        }
    }

    fn spawn_pane_with(&mut self, kind: PaneKind, area: Area, prompt: Option<PathBuf>) -> Option<PaneId> {
        self.spawn_pane_ext(kind, area, Spawn { prompt, ..Spawn::default() })
    }

    fn spawn_pane_ext(&mut self, kind: PaneKind, area: Area, extra: Spawn) -> Option<PaneId> {
        let Spawn { prompt, worktree, notice, cwd, label } = extra;
        let gfx = self.gfx.as_ref()?;
        let size = gfx.grid_size(area);
        // `KARBYX_CAPTURE_AI`: pestaña de IA de prueba con una shell normal dentro
        // (para capturas, sin lanzar el asistente de verdad).
        // Con `KARBYX_CAPTURE_AI=real` se lanza el asistente de verdad.
        let dry =
            capture::target().is_some() && std::env::var("KARBYX_CAPTURE_AI").is_ok_and(|v| v != "real");
        let command = match kind {
            PaneKind::Ai(_) if dry => None,
            PaneKind::Ai(p) => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
                let mut cmd = match &prompt {
                    Some(file) => ai_command_with_prompt(p, file),
                    None => ai_command(p.command()),
                };
                if let Some(n) = &notice {
                    cmd = format!("printf '\\033[33m%s\\033[0m\\n\\n' '{}'; {cmd}", n.replace('\'', "'\\''"));
                }
                Some((shell, vec!["-l".into(), "-i".into(), "-c".into(), cmd]))
            }
            PaneKind::Shell => None,
        };
        let mut env = std::collections::HashMap::new();
        env.insert("KARBYX_PANE".to_string(), self.next_id.to_string());
        if let Some(sock) = &self.agent_socket {
            env.insert("KARBYX_SOCKET".to_string(), sock.to_string_lossy().into_owned());
        }
        if let Some((settings, mcp)) = &self.claude_settings {
            env.insert("KARBYX_CLAUDE_SETTINGS".to_string(), settings.to_string_lossy().into_owned());
            env.insert("KARBYX_CLAUDE_MCP".to_string(), mcp.to_string_lossy().into_owned());
        }
        let opts = SessionOptions {
            cwd: worktree.as_ref().map(|w| w.path.clone()).or(cwd).or_else(|| self.new_session_cwd()),
            command,
            env,
            scrollback: Some(self.config.terminal.scrollback),
            shell_integration: self.config.terminal.shell_integration,
        };
        match Session::spawn(opts, size, self.theme.clone(), self.waker()) {
            Ok(session) => {
                let id = self.next_id;
                self.next_id += 1;
                let info = session.process_info();
                let project = project_of(info.cwd.as_deref());
                self.panes.insert(id, Pane { session, kind, info, agent: None, project, worktree, label });
                Some(id)
            }
            Err(e) => {
                log::error!("no se pudo abrir la sesión: {e}");
                None
            }
        }
    }

    fn open_tab(&mut self, kind: PaneKind) -> bool {
        self.open_tab_ext(kind, Spawn::default())
    }

    fn open_tab_ext(&mut self, kind: PaneKind, extra: Spawn) -> bool {
        let Some(layout) = self.layout() else { return false };
        let Some(id) = self.spawn_pane_ext(kind, layout.term, extra) else { return false };
        self.close_search();
        self.tabs.push(Tab { root: Node::Leaf(id), focus: id });
        self.set_active(self.tabs.len() - 1);
        self.redraw();
        true
    }

    /// Agente nuevo en su propio worktree de git (⌘⇧A). Fuera de un
    /// repositorio se abre normal, con un aviso.
    fn open_agent_worktree(&mut self) {
        let Some(p) = self.selected_provider() else { return };
        let cwd = self.new_session_cwd();
        let extra = match cwd.as_deref().map(worktree::create) {
            Some(Ok(wt)) => Spawn {
                notice: Some(format!(
                    "⎇ {} · trabajas en {} (⌘R para revisar y fusionar)",
                    wt.branch,
                    tilde(&wt.path)
                )),
                worktree: Some(wt),
                ..Spawn::default()
            },
            Some(Err(e)) => Spawn { notice: Some(format!("Sin worktree: {e}.")), ..Spawn::default() },
            None => Spawn::default(),
        };
        self.open_tab_ext(PaneKind::Ai(p), extra);
    }

    fn split(&mut self, dir: SplitDir) {
        self.split_with(dir, PaneKind::Shell, None);
    }

    fn split_with(&mut self, dir: SplitDir, kind: PaneKind, prompt: Option<PathBuf>) {
        let Some(focus) = self.focused_id() else { return };
        let area = self.pane_areas.iter().find(|(id, _)| *id == focus).map(|(_, a)| *a);
        let Some(area) = area.or_else(|| self.layout().map(|l| l.term)) else { return };
        let half = match dir {
            SplitDir::Horizontal => Area::new(area.x, area.y, area.w / 2.0, area.h),
            SplitDir::Vertical => Area::new(area.x, area.y, area.w, area.h / 2.0),
        };
        let Some(id) = self.spawn_pane_with(kind, half, prompt) else { return };
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.root.split(focus, id, dir);
        }
        self.set_focus(id);
        self.redraw();
    }

    /// Quita un panel (ya sea por cerrarlo o porque su shell terminó).
    fn remove_pane(&mut self, id: PaneId, event_loop: &ActiveEventLoop) {
        self.panes.remove(&id); // al soltar la sesión se cierra el PTY
        let Some(ti) = self.tabs.iter().position(|t| t.root.contains(id)) else { return };
        let tab = &mut self.tabs[ti];
        if tab.root == Node::Leaf(id) {
            self.tabs.remove(ti);
            if self.tabs.is_empty() {
                event_loop.exit();
                return;
            }
            if self.active > ti || self.active >= self.tabs.len() {
                self.set_active(self.active.saturating_sub(1));
            }
        } else {
            tab.root.remove(id);
            if tab.focus == id {
                tab.focus = tab.root.leaves()[0];
            }
        }
        if self.search.is_some() && !self.panes.contains_key(&self.focused_id().unwrap_or(0)) {
            self.search = None;
        }
        self.redraw();
    }

    fn close_tab(&mut self, i: usize, event_loop: &ActiveEventLoop) {
        let Some(tab) = self.tabs.get(i) else { return };
        for id in tab.root.leaves() {
            self.remove_pane(id, event_loop);
        }
    }

    fn select_tab(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.active {
            self.close_search();
            self.set_active(i);
        }
    }

    fn close_search(&mut self) {
        if self.search.take().is_some()
            && let Some(p) = self.focused_pane()
        {
            p.session.search_clear();
        }
    }

    fn handle_action(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        let n = self.tabs.len();
        // Las acciones de terminales salen de la mesa y del chat.
        if (self.board_visible || self.chat_visible)
            && matches!(
                action,
                Action::NewTab
                    | Action::NewAi
                    | Action::NewAgent
                    | Action::SelectTab(_)
                    | Action::NextTab
                    | Action::PrevTab
                    | Action::SplitRight
                    | Action::SplitDown
                    | Action::Find
            )
        {
            self.show_board(false);
            self.show_chat(false);
        }
        // Con el chat delante no se tocan las terminales de debajo.
        if self.chat_visible
            && matches!(
                action,
                Action::Close
                    | Action::ClearScreen
                    | Action::Paste
                    | Action::Copy
                    | Action::SelectAll
                    | Action::Focus(_)
                    | Action::BlockPrev
                    | Action::BlockNext
                    | Action::AskAi
                    | Action::FindNext
                    | Action::FindPrev
                    | Action::ToggleRight
            )
        {
            return;
        }
        if self.board_visible && action == Action::Paste {
            if let Some(t) = self.clipboard.as_mut().and_then(|c| c.get_text().ok()) {
                self.board_paste(&t);
            }
            return;
        }
        match action {
            Action::Board => self.show_board(!self.board_visible),
            Action::Chat => self.show_chat(!self.chat_visible),
            Action::NewChat => {
                self.show_chat(true);
                self.new_chat();
            }
            Action::Quit => event_loop.exit(),
            Action::NewTab => {
                self.open_tab(PaneKind::Shell);
            }
            Action::NewAi => {
                if let Some(p) = self.selected_provider() {
                    self.open_tab(PaneKind::Ai(p));
                }
            }
            Action::Close => {
                if let Some(id) = self.focused_id() {
                    self.remove_pane(id, event_loop);
                }
            }
            Action::SelectTab(n) => {
                if let Some(&i) = self.display_order().get(n) {
                    self.select_tab(i);
                }
            }
            Action::NewAgent => self.open_agent_worktree(),
            Action::Inbox => {
                self.inbox = if self.inbox.is_some() { None } else { Some(0) };
                self.dashboard = None;
                self.review = None;
            }
            Action::Dashboard => {
                self.dashboard = if self.dashboard.is_some() { None } else { Some(0) };
                self.inbox = None;
                self.review = None;
            }
            Action::Review => self.toggle_review(),
            Action::NextTab if n > 0 => self.select_tab((self.active + 1) % n),
            Action::PrevTab if n > 0 => self.select_tab((self.active + n - 1) % n),
            Action::NextTab | Action::PrevTab => {}
            Action::ToggleLeft => self.show_left = !self.show_left,
            Action::ToggleRight => self.right_override = Some(!self.right_visible()),
            Action::SplitRight => self.split(SplitDir::Horizontal),
            Action::SplitDown => self.split(SplitDir::Vertical),
            Action::Focus(nav) => {
                if let Some(id) = self.focused_id().and_then(|f| panes::neighbor(&self.pane_areas, f, nav)) {
                    self.set_focus(id);
                }
            }
            Action::Paste => {
                let text = self.clipboard.as_mut().and_then(|c| c.get_text().ok());
                if let (Some(t), Some(p)) = (text, self.focused_pane()) {
                    p.session.paste(&t);
                }
            }
            Action::BlockPrev | Action::BlockNext => {
                if let Some(focus) = self.focused_id()
                    && let Some(p) = self.panes.get(&focus)
                {
                    let cur = self.selected_block.filter(|(pid, _)| *pid == focus).map(|(_, b)| b);
                    let up = action == Action::BlockPrev;
                    // El último bloque es el prompt actual: bajar hasta él quita la selección.
                    let prompt = p.session.snapshot().blocks.last().map(|b| b.id);
                    match p.session.jump_block(cur, up) {
                        Some(b) if !up && Some(b) == prompt => self.selected_block = None,
                        Some(b) => self.selected_block = Some((focus, b)),
                        None => {}
                    }
                }
            }
            Action::AskAi => {
                let target = self.selected_block.or_else(|| {
                    let focus = self.focused_id()?;
                    let snap = self.panes.get(&focus)?.session.snapshot();
                    snap.blocks.iter().rev().find(|b| b.info.failed()).map(|b| (focus, b.id))
                });
                if let Some((pane, block)) = target {
                    self.ask_ai(pane, block);
                }
            }
            Action::Copy => {
                // Sin texto seleccionado, ⌘C copia el bloque seleccionado.
                let has_text = self.focused_pane().and_then(|p| p.session.selection_text()).is_some();
                if !has_text
                    && let Some((pane, block)) = self.selected_block
                    && let Some(t) = self.panes.get(&pane).and_then(|p| p.session.block_text(block))
                {
                    let mut out = String::new();
                    if let Some(c) = &t.command {
                        out.push_str(&format!("$ {c}\n"));
                    }
                    out.push_str(&t.output);
                    if let Some(c) = self.clipboard.as_mut() {
                        let _ = c.set_text(out);
                    }
                    self.redraw();
                    return;
                }
                let text = self.focused_pane().and_then(|p| p.session.selection_text());
                if let (Some(t), Some(c)) = (text, self.clipboard.as_mut()) {
                    let _ = c.set_text(t);
                }
            }
            Action::SelectAll => {
                if let Some(p) = self.focused_pane() {
                    p.session.select_all();
                }
                self.redraw();
            }
            Action::ClearScreen => {
                if let Some(p) = self.focused_pane() {
                    p.session.write(&b"\x0c"[..]);
                }
            }
            Action::FontBigger | Action::FontSmaller | Action::FontReset => {
                self.font_delta = match action {
                    Action::FontBigger => self.font_delta + 1.0,
                    Action::FontSmaller => self.font_delta - 1.0,
                    _ => 0.0,
                };
                let spec = self.font_spec();
                if let Some(g) = &mut self.gfx {
                    g.set_font(spec);
                }
            }
            Action::Find => {
                if self.search.is_none() {
                    self.search = Some(SearchUi::default());
                }
            }
            Action::FindNext | Action::FindPrev => {
                if let (Some(s), Some(p)) =
                    (self.search.as_mut(), self.panes.get(&self.tabs[self.active].focus))
                    && !s.query.is_empty()
                {
                    s.found = p.session.search_step(action == Action::FindNext);
                }
            }
        }
        self.redraw();
    }

    /// Teclas mientras la barra de búsqueda está abierta. Devuelve si la consumió.
    fn search_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        let Some(id) = self.focused_id() else { return false };
        let Some(s) = self.search.as_mut() else { return false };
        let Some(pane) = self.panes.get(&id) else { return false };
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                pane.session.search_clear();
                self.search = None;
            }
            Key::Named(NamedKey::Enter) => {
                if !s.query.is_empty() {
                    s.found = pane.session.search_step(!self.mods.shift_key());
                }
            }
            Key::Named(NamedKey::Backspace) => {
                s.query.pop();
                s.found = pane.session.search_set(&s.query);
            }
            _ => {
                if self.mods.super_key() || self.mods.control_key() {
                    return false;
                }
                let Some(text) = event.text.as_deref().filter(|t| !t.chars().any(char::is_control)) else {
                    return true;
                };
                s.query.push_str(text);
                s.found = pane.session.search_set(&s.query);
            }
        }
        self.redraw();
        true
    }

    fn click_ui(&mut self, hit: Hit, event_loop: &ActiveEventLoop) {
        match hit {
            Hit::Tab(i) => self.select_tab(i),
            Hit::CloseTab(i) => self.close_tab(i, event_loop),
            Hit::NewTab => {
                self.open_tab(PaneKind::Shell);
            }
            Hit::NewAi => {
                if let Some(p) = self.selected_provider() {
                    self.open_tab(PaneKind::Ai(p));
                }
            }
            Hit::Provider(i) => self.usage_sel = i,
            Hit::AskAi(pane, block) => self.ask_ai(pane, block),
            Hit::OpenInbox => self.inbox = Some(0),
            Hit::OpenBoard => self.show_board(true),
            Hit::OpenChat => self.show_chat(true),
            Hit::OpenTerminal => {
                self.show_chat(false);
                self.show_board(false);
            }
            Hit::Chat(h) => self.chat_click(h),
            Hit::InboxItem(i) => {
                if let Some((pane, _)) = self.inbox_items().into_iter().nth(i) {
                    self.go_to_pane(pane);
                }
            }
            Hit::DashAgent(i) => {
                if let Some((pane, _)) = self.dash_items(&self.usage_state()).into_iter().nth(i) {
                    self.go_to_pane(pane);
                }
            }
            Hit::ReviewFile(_) | Hit::ReviewMerge | Hit::ReviewDiscard => self.review_click(hit, event_loop),
            Hit::ReviewBox => {}
            Hit::BoardChat(id) => self.go_to_chat(id),
            Hit::BoardChatAnswer(id, tool, yes) => self.answer_chat(id, tool, yes),
            Hit::BoardCard(_)
            | Hit::BoardShell(_)
            | Hit::BoardReview(_)
            | Hit::BoardAnswer(..)
            | Hit::BoardVerify(_)
            | Hit::BoardFix(_)
            | Hit::BoardVerifyEnable(_)
            | Hit::BoardProject
            | Hit::BoardWorker
            | Hit::BoardLaunch => self.board_click(hit),
        }
        self.redraw();
    }

    /// Texto compuesto por el teclado (´ + a = á): va a donde se escribe.
    fn ime_text(&mut self, text: &str) {
        if self.review.is_some() || self.inbox.is_some() || self.dashboard.is_some() {
            return;
        }
        if self.chat_visible {
            return self.chat_text(text);
        }
        if self.board_visible {
            self.board.input.push_str(text);
            self.board.notice = None;
        } else if let (Some(s), Some(p)) =
            (self.search.as_mut(), self.tabs.get(self.active).and_then(|t| self.panes.get(&t.focus)))
        {
            s.query.push_str(text);
            s.found = p.session.search_set(&s.query);
        } else if let Some(p) = self.focused_pane() {
            p.session.clear_selection();
            p.session.write(text.as_bytes().to_vec());
        }
        self.redraw();
    }

    /// Abre a la derecha una sesión del asistente con la pregunta sobre el bloque.
    fn ask_ai(&mut self, pane: PaneId, block: u64) {
        let Some(p) = self.panes.get(&pane) else { return };
        let Some(text) = p.session.block_text(block) else { return };
        let prompt = ask_prompt(&text, p.info.cwd.as_deref());
        let provider = self.selected_provider().unwrap_or(Provider::Claude);
        let Some(dir) = dirs::cache_dir().map(|d| d.join("karbyx").join("prompts")) else { return };
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let file = dir.join(format!("bloque-{}-{pane}-{block}.txt", std::process::id()));
        if std::fs::write(&file, prompt).is_err() {
            return;
        }
        self.set_focus(pane);
        self.split_with(SplitDir::Horizontal, PaneKind::Ai(provider), Some(file));
    }

    /// Bloque bajo una fila de un panel (según el último frame).
    fn block_at(&self, pane: PaneId, row: usize) -> Option<u64> {
        self.pane_blocks.get(&pane)?.iter().find(|(_, a, b)| (*a..=*b).contains(&row)).map(|(id, _, _)| *id)
    }

    fn pane_at(&self, x: f32, y: f32) -> Option<(PaneId, Area)> {
        self.pane_areas.iter().copied().find(|(_, a)| a.contains(x, y))
    }

    /// Celda bajo el ratón en un panel (fila, columna, mitad derecha).
    fn cell_under(&self, id: PaneId, area: Area) -> Option<(usize, usize, bool)> {
        let gfx = self.gfx.as_ref()?;
        let size = self.panes.get(&id)?.session.size();
        Some(gfx.cell_at(area, self.cursor.0, self.cursor.1, size.rows as usize, size.cols as usize))
    }

    fn mouse_mods(&self) -> MouseMods {
        MouseMods { shift: self.mods.shift_key(), alt: self.mods.alt_key(), ctrl: self.mods.control_key() }
    }

    /// Envía un evento de ratón a la aplicación del panel si lo pide (y no se
    /// pulsa ⇧, que fuerza la selección normal, como en iTerm2).
    fn report_mouse(&self, id: PaneId, area: Area, button: Option<TermButton>, action: MouseAction) -> bool {
        if self.mods.shift_key() {
            return false;
        }
        let (Some(pane), Some((row, col, _))) = (self.panes.get(&id), self.cell_under(id, area)) else {
            return false;
        };
        match mouse::encode(pane.session.mode(), button, action, col, row, self.mouse_mods()) {
            Some(bytes) => {
                pane.session.write(bytes);
                true
            }
            None => false,
        }
    }

    fn mouse_press(&mut self, button: MouseButton, event_loop: &ActiveEventLoop) {
        let (x, y) = self.cursor;
        if button == MouseButton::Left
            && let Some(hit) = ui::hit_at(&self.hits, x, y)
        {
            self.click_ui(hit, event_loop);
            return;
        }
        if self.chat_visible {
            if button == MouseButton::Left {
                let double = self.chat_click_at.is_some_and(|(t, (px, py))| {
                    t.elapsed() < Duration::from_millis(400) && (px - x).abs() + (py - y).abs() < 6.0
                });
                self.chat_click_at = Some((Instant::now(), (x, y)));
                self.drag = self.chat_press(x, y, double).map(Drag::Chat);
            }
            return;
        }
        if button == MouseButton::Left
            && let Some(d) = self.dividers.iter().find(|d| d.grip.contains(x, y)).cloned()
        {
            self.drag = Some(Drag::Divider(d));
            return;
        }
        let Some((id, area)) = self.pane_at(x, y) else { return };
        self.set_focus(id);
        if button == MouseButton::Left
            && let Some(gfx) = &self.gfx
            && x < area.x + gfx.metrics.pad
            && let Some((row, _, _)) = self.cell_under(id, area)
        {
            let hit = self.block_at(id, row).map(|b| (id, b));
            self.selected_block = if self.selected_block == hit { None } else { hit };
            self.redraw();
            return;
        }
        let tb = match button {
            MouseButton::Left => TermButton::Left,
            MouseButton::Middle => TermButton::Middle,
            MouseButton::Right => TermButton::Right,
            _ => return,
        };
        if self.report_mouse(id, area, Some(tb), MouseAction::Press) {
            self.drag = Some(Drag::Report(id));
            return;
        }
        if button != MouseButton::Left {
            return;
        }
        let Some((row, col, right)) = self.cell_under(id, area) else { return };
        let now = Instant::now();
        let count = match self.last_click {
            Some(c)
                if c.pane == id
                    && c.row == row
                    && c.col.abs_diff(col) <= 1
                    && now - c.at < Duration::from_millis(400) =>
            {
                c.count % 3 + 1
            }
            _ => 1,
        };
        self.last_click = Some(LastClick { at: now, pane: id, row, col, count });
        // ⇧ extiende la selección hasta aquí.
        if self.mods.shift_key()
            && count == 1
            && self.panes.get(&id).is_some_and(|p| p.session.has_selection())
        {
            self.extend_selection(id, x, y);
            self.drag = Some(Drag::Select(id));
            self.redraw();
            return;
        }
        let kind = match count {
            2 => SelectionKind::Word,
            3 => SelectionKind::Line,
            _ if self.mods.alt_key() => SelectionKind::Block,
            _ => SelectionKind::Simple,
        };
        if let Some(p) = self.panes.get(&id) {
            p.session.start_selection(kind, row, col, right);
        }
        self.drag = Some(Drag::Select(id));
        self.redraw();
    }

    fn mouse_release(&mut self, button: MouseButton) {
        match self.drag.take() {
            Some(Drag::Report(id)) => {
                if let Some((_, area)) = self.pane_areas.iter().find(|(p, _)| *p == id).copied() {
                    let tb = match button {
                        MouseButton::Middle => TermButton::Middle,
                        MouseButton::Right => TermButton::Right,
                        _ => TermButton::Left,
                    };
                    self.report_mouse(id, area, Some(tb), MouseAction::Release);
                }
            }
            Some(Drag::Chat(_)) => self.chat_drag_end(),
            // Un clic sin arrastrar no selecciona nada, pero desde ahí se extiende con ⇧.
            _ => {}
        }
        self.redraw();
    }

    fn mouse_move(&mut self) {
        let (x, y) = self.cursor;
        match self.drag.clone() {
            Some(Drag::Select(id)) => {
                self.extend_selection(id, x, y);
                self.redraw();
            }
            Some(Drag::Divider(d)) => {
                let ratio = match d.dir {
                    SplitDir::Horizontal => (x - d.parent.x) / d.parent.w.max(1.0),
                    SplitDir::Vertical => (y - d.parent.y) / d.parent.h.max(1.0),
                };
                if let Some(t) = self.tabs.get_mut(self.active) {
                    t.root.set_ratio(&d.path, ratio);
                }
                self.redraw();
            }
            Some(Drag::Report(id)) => {
                if let Some((_, area)) = self.pane_areas.iter().find(|(p, _)| *p == id).copied() {
                    self.report_mouse(id, area, Some(TermButton::Left), MouseAction::Motion);
                }
            }
            Some(Drag::Chat(input)) => self.chat_drag(x, y, input),
            None => {}
        }

        // Bloque bajo el ratón (para mostrar el botón de IA).
        let hb = self
            .pane_at(x, y)
            .and_then(|(id, area)| Some((id, self.cell_under(id, area)?.0)))
            .and_then(|(id, row)| Some((id, self.block_at(id, row)?)));
        if hb != self.hover_block {
            self.hover_block = hb;
            self.redraw();
        }

        // Hover de la UI y forma del cursor.
        let hover = ui::hit_at(&self.hits, x, y);
        if hover != self.hover {
            self.hover = hover;
            self.redraw();
        }
        let icon = if let Some(Drag::Divider(d)) = &self.drag {
            if d.dir == SplitDir::Horizontal { CursorIcon::ColResize } else { CursorIcon::RowResize }
        } else if let Some(d) = self.dividers.iter().find(|d| d.grip.contains(x, y)) {
            if d.dir == SplitDir::Horizontal { CursorIcon::ColResize } else { CursorIcon::RowResize }
        } else if hover.is_some() {
            CursorIcon::Pointer
        } else if self.pane_at(x, y).is_some() || (self.chat_visible && self.chat_text_at(x, y)) {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        };
        if icon != self.cursor_icon {
            self.cursor_icon = icon;
            if let Some(g) = &self.gfx {
                g.window.set_cursor(icon);
            }
        }
    }

    fn wheel(&mut self, delta: MouseScrollDelta) {
        if self.board_visible && self.review.is_none() {
            return;
        }
        if self.chat_visible && self.review.is_none() {
            let scale = self.gfx.as_ref().map_or(1.0, |g| g.scale());
            self.chat_wheel_at(
                self.cursor,
                match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 48.0 * scale,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                },
            );
            return;
        }
        if self.review.is_some() {
            let lines = match delta {
                MouseScrollDelta::LineDelta(_, y) => y as f64 * 3.0,
                MouseScrollDelta::PixelDelta(p) => {
                    p.y / (17.0 * self.gfx.as_ref().map_or(1.0, |g| g.scale()) as f64)
                }
            };
            self.review_wheel(lines);
            return;
        }
        let Some((id, area)) = self.pane_at(self.cursor.0, self.cursor.1) else { return };
        let Some(gfx) = &self.gfx else { return };
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y as f64 * 3.0,
            MouseScrollDelta::PixelDelta(p) => p.y / gfx.metrics.cell_h as f64,
        };
        self.scroll_accum += lines;
        let whole = self.scroll_accum.trunc() as i32;
        if whole == 0 {
            return;
        }
        self.scroll_accum -= whole as f64;
        let Some(pane) = self.panes.get(&id) else { return };
        let mode = pane.session.mode();
        let button = if whole > 0 { TermButton::WheelUp } else { TermButton::WheelDown };
        if mouse::wants_mouse(mode) && !self.mods.shift_key() {
            for _ in 0..whole.unsigned_abs().min(5) {
                self.report_mouse(id, area, Some(button), MouseAction::Press);
            }
        } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            // En apps a pantalla completa (vim, less) la rueda envía flechas.
            let seq: &[u8] = if whole > 0 { b"\x1bOA" } else { b"\x1bOB" };
            pane.session.write(seq.repeat(whole.unsigned_abs() as usize));
        } else {
            pane.session.scroll(whole);
            if matches!(self.drag, Some(Drag::Select(d)) if d == id) {
                self.extend_selection(id, self.cursor.0, self.cursor.1);
            }
        }
        self.redraw();
    }

    fn apply_config(&mut self, config: Config) {
        self.theme = config.theme();
        self.bindings = Bindings::new(&config.keys);
        self.config = config;
        for p in self.panes.values_mut() {
            p.session.set_theme(self.theme.clone());
        }
        let spec = self.font_spec();
        if let Some(g) = &mut self.gfx
            && *g.font() != spec
        {
            g.set_font(spec);
        }
        self.redraw();
    }

    /// Procesa eventos de todas las sesiones (también las de fondo, que
    /// pueden necesitar respuestas) y retira las que terminaron.
    fn pump_sessions(&mut self, event_loop: &ActiveEventLoop) {
        let mut exited = Vec::new();
        let mut finished = Vec::new();
        for (id, pane) in &self.panes {
            for ev in pane.session.poll_events() {
                match ev {
                    TermEvent::Exited => exited.push(*id),
                    TermEvent::ClipboardStore(s) => {
                        if let Some(c) = self.clipboard.as_mut() {
                            let _ = c.set_text(s);
                        }
                    }
                    TermEvent::Bell => {
                        if let Some(g) = &self.gfx {
                            g.window.request_user_attention(None);
                        }
                    }
                    TermEvent::CommandFinished { exit, duration, command, .. } => {
                        finished.push((*id, exit, duration, command));
                    }
                    TermEvent::Title(_) | TermEvent::ResetTitle => {}
                }
            }
        }
        for id in exited {
            self.remove_pane(id, event_loop);
        }
        let min = self.config.terminal.notify_after_secs;
        for (pane, exit, duration, command) in finished {
            let visible = self.tabs.get(self.active).is_some_and(|t| t.root.contains(pane));
            if min == 0 || duration.as_secs() < min || (self.focused && visible) {
                continue;
            }
            let cmd = command.unwrap_or_else(|| "comando".into());
            let body = match exit {
                Some(0) | None => format!("✓ {cmd} · {}", ui::fmt_duration(duration)),
                Some(c) => format!("✗ {cmd} falló (código {c}) · {}", ui::fmt_duration(duration)),
            };
            notify("Karbyx", &body);
            if let Some(g) = &self.gfx {
                g.window.request_user_attention(Some(winit::window::UserAttentionType::Informational));
            }
        }
        if self.info_refreshed.elapsed() > Duration::from_millis(800) {
            for pane in self.panes.values_mut() {
                pane.info = pane.session.process_info();
                pane.project = project_of(pane.info.cwd.as_deref());
            }
            self.info_refreshed = Instant::now();
        }
        self.update_agents();
    }

    /// ¿El usuario está viendo este panel? (pestaña activa y ventana con foco)
    fn pane_visible(&self, pane: PaneId) -> bool {
        self.focused && !self.occluded && self.tabs.get(self.active).is_some_and(|t| t.root.contains(pane))
    }

    /// Nombre corto del agente de un panel.
    fn agent_name(&self, pane: PaneId) -> String {
        self.panes
            .get(&pane)
            .and_then(|p| p.title().1)
            .map(|p| p.name().to_string())
            .unwrap_or_else(|| "El agente".into())
    }

    /// Aviso de macOS cuando un agente pasa a esperarte o termina sin que lo veas.
    fn notify_agent(&self, pane: PaneId, before: Option<AgentState>) {
        let Some(p) = self.panes.get(&pane) else { return };
        let Some(a) = &p.agent else { return };
        if !a.unseen || before == Some(a.state) {
            return;
        }
        let name = self.agent_name(pane);
        let what = a.detail.clone().or_else(|| a.task.clone()).unwrap_or_default();
        let body = match a.state {
            AgentState::Waiting => format!("{name} te espera en {}. {what}", p.project),
            AgentState::Done => format!("{name} ha terminado en {}. {what}", p.project),
            AgentState::Working => return,
        };
        notify("Karbyx", body.trim_end_matches(". "));
        if let Some(g) = &self.gfx {
            g.window.request_user_attention(Some(winit::window::UserAttentionType::Informational));
        }
    }

    /// Evento de un hook de Claude Code.
    fn on_hook(&mut self, msg: agents::HookMsg) {
        let visible = self.pane_visible(msg.pane);
        let Some(p) = self.panes.get_mut(&msg.pane) else { return };
        let before = p.agent.as_ref().filter(|a| a.from_hooks).map(|a| a.state);
        p.agent = agents::apply_hook(p.agent.take(), msg.event, visible);
        if let (Some(a), Some(sid)) = (p.agent.as_mut(), msg.session) {
            a.session = Some(sid);
        }
        self.notify_agent(msg.pane, before);
        self.after_agent_change(msg.pane, before);
        self.redraw();
    }

    /// Heurística para agentes sin hooks y marcas de "visto".
    fn update_agents(&mut self) {
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        let mut any = false;
        for id in ids {
            let visible = self.pane_visible(id);
            let Some(p) = self.panes.get_mut(&id) else { continue };
            if visible && let Some(a) = p.agent.as_mut() {
                a.unseen = false;
            }
            let provider = p.title().1;
            let running_ai = provider.is_some();
            if p.agent.as_ref().is_some_and(|a| a.from_hooks) {
                any = true;
                continue;
            }
            // Claude informa por hooks: hasta el primer evento no se adivina nada
            // (si no, un Claude recién abierto saldría como "te espera").
            if provider == Some(Provider::Claude) && self.claude_settings.is_some() {
                p.agent = None;
                continue;
            }
            if !running_ai {
                p.agent = None;
                continue;
            }
            any = true;
            let before = p.agent.as_ref().map(|a| (a.state, a.since));
            let idle = p.session.idle_for();
            p.agent = Some(agents::heuristic(p.agent.take(), idle, visible));
            self.after_agent_change(id, before.map(|b| b.0));
            // Solo avisar si trabajó un rato (no por un parpadeo de salida).
            if let Some((AgentState::Working, since)) = before
                && since.elapsed() >= Duration::from_secs(5)
            {
                self.notify_agent(id, Some(AgentState::Working));
            }
        }
        // Con el panel de revisión abierto el diff se refresca solo.
        let down =
            self.chat_visible && self.chats.servers.iter().any(|s| matches!(s.status, chat::Status::Down(_)));
        self.tick_needed.store(
            any || self.review.is_some() || self.board_visible || self.chats.animating() || down,
            Ordering::Relaxed,
        );
    }

    /// Orden de las pestañas en pantalla: agrupadas por proyecto.
    fn display_order(&self) -> Vec<usize> {
        let project = |t: &Tab| self.panes.get(&t.focus).map(|p| p.project.clone()).unwrap_or_default();
        let mut groups: Vec<String> = Vec::new();
        for t in &self.tabs {
            let pr = project(t);
            if !groups.contains(&pr) {
                groups.push(pr);
            }
        }
        let mut order = Vec::with_capacity(self.tabs.len());
        for g in &groups {
            order.extend(self.tabs.iter().enumerate().filter(|(_, t)| &project(t) == g).map(|(i, _)| i));
        }
        order
    }

    /// Agentes para la bandeja, del más urgente al menos.
    fn inbox_items(&self) -> Vec<(PaneId, ui::InboxItem)> {
        let mut v: Vec<(u8, Instant, PaneId, ui::InboxItem)> = self
            .panes
            .iter()
            .filter_map(|(id, p)| {
                let a = p.agent.as_ref()?;
                let rank = match (a.state, a.unseen) {
                    (AgentState::Waiting, true) => 0,
                    (AgentState::Waiting, false) => 1,
                    (AgentState::Done, true) => 2,
                    (AgentState::Working, _) => 3,
                    (AgentState::Done, false) => 4,
                };
                let (name, ai) = p.title();
                let title = a.task.clone().unwrap_or(name);
                let detail = a.detail.clone().unwrap_or_else(|| state_text(a.state).into());
                let item = ui::InboxItem {
                    title,
                    detail,
                    project: p.project.clone(),
                    state: a.state,
                    ai,
                    elapsed: ago(a.since),
                    unseen: a.unseen,
                };
                Some((rank, a.since, *id, item))
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        v.into_iter().map(|(_, _, id, it)| (id, it)).collect()
    }

    /// Agentes que necesitan tu atención (te esperan o terminaron sin verlos).
    fn attention(&self) -> usize {
        self.panes
            .values()
            .filter_map(|p| p.agent.as_ref())
            .filter(|a| a.state == AgentState::Waiting || (a.state == AgentState::Done && a.unseen))
            .count()
    }

    /// Lleva al usuario al panel: su pestaña, foco y marca como visto.
    fn go_to_pane(&mut self, pane: PaneId) {
        if let Some(ti) = self.tabs.iter().position(|t| t.root.contains(pane)) {
            self.select_tab(ti);
            self.set_focus(pane);
        }
        if let Some(a) = self.panes.get_mut(&pane).and_then(|p| p.agent.as_mut()) {
            a.unseen = false;
        }
        self.inbox = None;
        self.dashboard = None;
        self.review = None;
        self.board_visible = false;
        self.chat_visible = false;
        self.redraw();
    }

    /// Agentes abiertos para el panel de mando, en el orden de la barra lateral,
    /// con los tokens de su sesión si se conocen.
    fn dash_items(&self, usage: &UsageState) -> Vec<(PaneId, ui::DashAgent)> {
        let mut v = Vec::new();
        for i in self.display_order() {
            for id in self.tabs[i].root.leaves() {
                let Some(p) = self.panes.get(&id) else { continue };
                let (name, ai) = p.title();
                let Some(provider) = ai else { continue };
                let a = p.agent.as_ref();
                let tokens = a.and_then(|a| a.session.as_ref()).and_then(|sid| {
                    usage.views.iter().find(|u| u.provider == provider)?.sessions.get(sid).copied()
                });
                v.push((
                    id,
                    ui::DashAgent {
                        title: a.and_then(|a| a.task.clone()).unwrap_or(name),
                        project: p.project.clone(),
                        ai,
                        state: a.map(|a| a.state),
                        tokens,
                    },
                ));
            }
        }
        v
    }

    /// Teclas con el panel de mando abierto. Las consume todas.
    fn dash_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        let Some(sel) = self.dashboard else { return false };
        let items = self.dash_items(&self.usage_state());
        let n = items.len();
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.dashboard = None,
            Key::Named(NamedKey::ArrowDown) if n > 0 => self.dashboard = Some((sel + 1) % n),
            Key::Named(NamedKey::ArrowUp) if n > 0 => self.dashboard = Some((sel + n - 1) % n),
            Key::Named(NamedKey::Enter) => {
                if let Some((pane, _)) = items.into_iter().nth(sel) {
                    self.go_to_pane(pane);
                }
            }
            _ => {}
        }
        self.redraw();
        true
    }

    /// Teclas con la bandeja abierta. Devuelve si la consumió.
    fn inbox_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        let Some(sel) = self.inbox else { return false };
        let n = self.inbox_items().len();
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => self.inbox = None,
            Key::Named(NamedKey::ArrowDown) if n > 0 => self.inbox = Some((sel + 1) % n),
            Key::Named(NamedKey::ArrowUp) if n > 0 => self.inbox = Some((sel + n - 1) % n),
            Key::Named(NamedKey::Enter) => {
                if let Some((pane, _)) = self.inbox_items().into_iter().nth(sel) {
                    self.go_to_pane(pane);
                }
            }
            _ => {}
        }
        self.redraw();
        true
    }

    fn tab_views(&self) -> Vec<TabView> {
        self.display_order()
            .into_iter()
            .enumerate()
            .filter_map(|(pos, i)| {
                let t = &self.tabs[i];
                let focus = self.panes.get(&t.focus)?;
                // La pestaña muestra su agente más urgente (o el panel con foco).
                let urgency = |a: &AgentStatus| match (a.state, a.unseen) {
                    (AgentState::Waiting, _) => 0,
                    (AgentState::Done, true) => 1,
                    (AgentState::Working, _) => 2,
                    (AgentState::Done, false) => 3,
                };
                let agent_pane = t
                    .root
                    .leaves()
                    .into_iter()
                    .filter_map(|id| self.panes.get(&id))
                    .filter(|p| p.agent.is_some())
                    .min_by_key(|p| p.agent.as_ref().map(urgency).unwrap_or(9));
                let pane = agent_pane.unwrap_or(focus);
                let (name, ai) = pane.title();
                let n = t.root.leaves().len();
                let (title, mut subtitle, state, unseen, elapsed) = match &pane.agent {
                    Some(a) => (
                        a.task.clone().unwrap_or(name),
                        a.detail.clone().unwrap_or_else(|| state_text(a.state).into()),
                        Some(a.state),
                        a.unseen,
                        Some(ago(a.since)),
                    ),
                    None => {
                        let mut sub = pane.info.cwd.as_deref().map(tilde).unwrap_or_default();
                        if n > 1 {
                            sub = format!("{sub} · {n} paneles");
                        }
                        (name, sub, None, false, None)
                    }
                };
                if let Some(w) = &pane.worktree {
                    subtitle = format!("⎇ {} · {subtitle}", w.name());
                }
                Some(TabView {
                    index: i,
                    shortcut: (pos < 9).then_some(pos + 1),
                    title,
                    subtitle,
                    project: focus.project.clone(),
                    ai,
                    state,
                    unseen,
                    elapsed,
                    active: i == self.active,
                })
            })
            .collect()
    }

    fn render(&mut self, event_loop: &ActiveEventLoop) {
        let mut prof = Profiler::new();
        self.pump_sessions(event_loop);
        self.tick_review();
        self.tick_board();
        self.tick_chat();
        // `KARBYX_CAPTURE_REVIEW`: abre el panel de revisión antes de capturar.
        if capture::target().is_some()
            && self.review.is_none()
            && std::env::var_os("KARBYX_CAPTURE_REVIEW").is_some()
            && self.started.elapsed() + Duration::from_millis(1500) > capture::delay()
        {
            self.toggle_review();
        }
        prof.mark("pump");
        if self.occluded || self.tabs.is_empty() {
            return;
        }
        let Some(layout) = self.layout() else { return };
        let views = self.tab_views();
        let attention = self.attention();
        let tick = (self.started.elapsed().as_millis() / 400) as u64;
        let inbox = self.inbox.map(|sel| {
            let items: Vec<ui::InboxItem> = self.inbox_items().into_iter().map(|(_, it)| it).collect();
            (sel.min(items.len().saturating_sub(1)), items)
        });
        // Al cambiar el asistente de la pestaña activa, el panel muestra el suyo.
        let tab_ai = self.tab_ai();
        if tab_ai != self.last_tab_ai {
            self.last_tab_ai = tab_ai;
            if tab_ai.is_some() {
                self.pending_provider = tab_ai;
            }
        }
        let ask_provider = Some(self.selected_provider().unwrap_or(Provider::Claude));
        let usage = self.usage_state();
        let dash = self.dashboard.map(|sel| {
            let items: Vec<ui::DashAgent> = self.dash_items(&usage).into_iter().map(|(_, a)| a).collect();
            let sel = (!items.is_empty()).then(|| sel.min(items.len() - 1));
            (sel, items)
        });
        let board_view = self.board_visible.then(|| self.board_view(&usage));
        let chat_on = self.chat_visible;
        let chat_env = chat_on.then(|| chat::draw::Env {
            mcp: self.chats.saved.mcp.values().any(|s| s.always),
            tick,
            hover: self.hover.and_then(Self::chat_hit),
            zoom: (self.font_spec().size_pt / 14.0).clamp(0.6, 2.5),
            focused: self.focused,
            projects: self.chat_projects(),
            claude: Provider::detect_all().contains(&Provider::Claude),
        });
        let Some(gfx) = &mut self.gfx else { return };
        let scale = gfx.scale();
        let tab = &self.tabs[self.active];
        let mut deco = Ui::new(scale);
        let mut overlay = Ui::new(scale);
        let mut pane_blocks = HashMap::new();

        // Geometría de los paneles de la pestaña activa.
        let gap = scale.round().max(1.0);
        let (mut pane_areas, mut dividers) = (Vec::new(), Vec::new());
        if board_view.is_none() && !chat_on {
            tab.root.layout(layout.term, gap, &mut pane_areas, &mut dividers);
        }

        let mut rects: Vec<Rect> = Vec::with_capacity(1024);
        let mut texts: Vec<PaneText> = Vec::new();
        let mut labels: Vec<Label> = Vec::new();
        let multi = pane_areas.len() > 1;
        let m = gfx.metrics;
        for (id, area) in &pane_areas {
            let Some(pane) = self.panes.get_mut(id) else { continue };
            pane.session.resize(gfx.grid_size(*area));
            let focused = *id == tab.focus;
            let mut snap = pane.session.snapshot();
            let cursor = match (focused, self.focused) {
                (true, true) => CursorStyle::Block,
                _ => CursorStyle::Hollow,
            };
            gfx::style_snapshot(&mut snap, &self.theme, cursor, multi && !focused);
            let (ox, oy) = (area.x + m.pad, area.y + m.pad);
            rects.extend(gfx::terminal_rects(&snap, &self.theme, &m, ox, oy, cursor));
            gfx.prepare_rows(*id, &snap);
            texts.push(PaneText { pane: *id, ox, oy, clip: *area });
            let pick = |b: Option<(PaneId, u64)>| b.filter(|(p, _)| p == id).map(|(_, b)| b);
            ui::draw_blocks(
                &mut deco,
                &mut overlay,
                &ui::BlockCtx {
                    pane: *id,
                    area: *area,
                    origin: (ox, oy),
                    cell: (m.cell_w, m.cell_h),
                    snap: &snap,
                    selected: pick(self.selected_block),
                    hovered: pick(self.hover_block),
                    ai: ask_provider,
                },
            );
            pane_blocks
                .insert(*id, snap.blocks.iter().map(|b| (b.id, b.first_row, b.last_row)).collect::<Vec<_>>());
            if snap.display_offset > 0 {
                let w = 160.0 * scale;
                labels.push(Label {
                    text: format!("↑ {} líneas", snap.display_offset),
                    area: Area::new(area.right() - w - m.pad, area.y + 4.0 * scale, w, 18.0 * scale),
                    px: 11.0 * scale,
                    color: ui::DIM,
                    bold: false,
                    align: Align::Right,
                    font: Font::Sans,
                    clip: None,
                });
            }
        }
        prof.mark("panes");
        rects.append(&mut deco.rects);
        labels.append(&mut deco.labels);
        self.pane_blocks = pane_blocks;
        for d in &dividers {
            rects.push(Rect {
                pos: [d.line.x, d.line.y],
                size: [d.line.w, d.line.h],
                color: linear(ui::BORDER, 1.0),
                ..Default::default()
            });
        }

        // Título de la ventana: el del panel activo (o la mesa).
        if board_view.is_some() || chat_on {
            let title = if chat_on { "Chat — Karbyx" } else { "Mesa — Karbyx" };
            if self.window_title != title {
                gfx.window.set_title(title);
                self.window_title = title.into();
            }
        } else if let Some(v) = views.iter().find(|v| v.index == self.active) {
            let title = if v.subtitle.is_empty() {
                format!("{} — Karbyx", v.title)
            } else {
                format!("{} · {} — Karbyx", v.title, v.subtitle)
            };
            // Cambiar el título implica al servidor de ventanas: solo si cambia.
            if title != self.window_title {
                gfx.window.set_title(&title);
                self.window_title = title;
            }
        }

        // Paneles laterales y barra de búsqueda.
        let mut ui = Ui::new(scale);
        if usage.loaded
            && let Some(p) = self.pending_provider.take()
            && let Some(i) = usage.views.iter().position(|v| v.provider == p)
        {
            self.usage_sel = i;
        }
        let ai = usage
            .loaded
            .then(|| {
                usage.views.get(self.usage_sel.min(usage.views.len().saturating_sub(1))).map(|v| v.provider)
            })
            .flatten();
        let screen = Area::new(0.0, 0.0, gfx.config.width as f32, gfx.config.height as f32);
        if let Some(v) = &board_view {
            board::draw_board(&mut ui, screen, v, tick, self.hover);
        }
        let mut rich: Vec<RichDraw> = Vec::new();
        if let Some(env) = &chat_env {
            let l = ui::layout(screen.w, screen.h, scale, self.show_left, false);
            chat::draw::draw_chat(&mut ui, &mut overlay, gfx, &mut rich, l.term, &mut self.chats, env);
            if let Some(side) = l.left {
                chat::draw::draw_side(&mut ui, side, &mut self.chats, env, self.hover);
            }
        }
        if let Some(area) = layout.left.filter(|_| board_view.is_none() && !chat_on) {
            ui::draw_left(&mut ui, area, &views, ai, attention, tick, self.hover);
        }
        if let Some(area) = layout.right.filter(|_| board_view.is_none() && !chat_on) {
            ui::draw_right(&mut ui, area, &usage, self.usage_sel, self.hover, Utc::now());
        }
        if let Some(s) = &self.search
            && let Some((_, area)) = pane_areas.iter().find(|(id, _)| *id == tab.focus)
        {
            ui::draw_search(&mut overlay, *area, &s.query, s.found);
        }
        if let Some((sel, items)) = &inbox {
            let screen = Area::new(0.0, 0.0, gfx.config.width as f32, gfx.config.height as f32);
            ui::draw_inbox(&mut overlay, screen, items, *sel, tick, self.hover);
        }
        if let Some((sel, items)) = &dash {
            let screen = Area::new(0.0, 0.0, gfx.config.width as f32, gfx.config.height as f32);
            ui::draw_dashboard(&mut overlay, screen, &usage, items, *sel, tick, self.hover, Utc::now());
        }
        if let Some(r) = &mut self.review {
            let screen = Area::new(0.0, 0.0, gfx.config.width as f32, gfx.config.height as f32);
            let view = ui::ReviewView {
                project: &r.project,
                branch: r.worktree.as_ref().map(|w| w.branch.as_str()),
                target: &r.target,
                files: r.files.as_ref().map(|f| f.as_ref().map(Vec::as_slice).map_err(String::as_str)),
                sel: r.sel,
                scroll: r.scroll,
                confirm_discard: r.confirm_discard,
                message: r.message.as_ref().map(|(m, e)| (m.as_str(), *e)),
                busy: r.busy,
            };
            let rows = ui::draw_review(&mut overlay, screen, &view, self.hover);
            if rows > 0 {
                r.rows = rows;
            }
        }
        rects.extend(ui.rects.iter().copied());
        labels.append(&mut ui.labels);
        // Prioridad de clic: capa superpuesta, luego paneles laterales.
        let mut hits = std::mem::take(&mut overlay.hits);
        hits.append(&mut ui.hits);
        self.hits = hits;
        self.pane_areas = pane_areas;
        self.dividers = dividers;

        // Captura (modo desarrollo) cuando la shell ya ha pintado.
        let capture = capture::target().filter(|_| self.started.elapsed() > capture::delay());
        // `KARBYX_CAPTURE_SELECT`: selecciona el último bloque fallido antes de capturar.
        if capture::target().is_some()
            && self.selected_block.is_none()
            && std::env::var_os("KARBYX_CAPTURE_SELECT").is_some()
            && let Some(start) = self.started.elapsed().checked_sub(Duration::from_millis(500))
            && start + Duration::from_millis(500)
                > capture::delay().saturating_sub(Duration::from_millis(400))
        {
            let focus = self.tabs[self.active].focus;
            if let Some((id, _, _)) = self.pane_blocks.get(&focus).and_then(|v| {
                v.iter().rev().find(|(id, _, _)| {
                    self.panes
                        .get(&focus)
                        .and_then(|p| p.session.block_text(*id))
                        .is_some_and(|t| t.exit.is_some_and(|c| c != 0))
                })
            }) {
                self.selected_block = Some((focus, *id));
            }
        }
        // `KARBYX_CAPTURE_INBOX`: abre la bandeja de agentes antes de capturar.
        if capture::target().is_some()
            && self.inbox.is_none()
            && std::env::var_os("KARBYX_CAPTURE_INBOX").is_some()
            && self.started.elapsed() + Duration::from_millis(800) > capture::delay()
        {
            self.inbox = Some(0);
            gfx.window.request_redraw();
        }
        // `KARBYX_CAPTURE_DASH`: abre el panel de mando antes de capturar.
        if capture::target().is_some()
            && self.dashboard.is_none()
            && std::env::var_os("KARBYX_CAPTURE_DASH").is_some()
            && self.started.elapsed() + Duration::from_millis(800) > capture::delay()
        {
            self.dashboard = Some(0);
            gfx.window.request_redraw();
        }
        // `KARBYX_SEARCH=texto` abre la búsqueda en modo captura, para verla.
        if capture::target().is_some()
            && self.search.is_none()
            && self.started.elapsed() > Duration::from_secs(3)
            && let Ok(q) = std::env::var("KARBYX_SEARCH")
            && let Some(p) = self.panes.get(&self.tabs[self.active].focus)
        {
            let found = p.session.search_set(&q);
            self.search = Some(SearchUi { query: q, found });
            gfx.window.request_redraw();
        }
        let captured = gfx.draw(
            &rects,
            &texts,
            &labels,
            &rich,
            &overlay.rects,
            &overlay.labels,
            self.theme.foreground,
            self.theme.background,
            capture.as_deref(),
        );
        prof.mark("draw");
        prof.report();
        self.drag_tick();
        if !captured
            && let Some(gfx) = &self.gfx
            && capture::target().is_some()
        {
            gfx.window.request_redraw();
        }
        if captured {
            event_loop.exit();
        }
    }

    /// Selección en marcha: al arrastrar fuera de la zona, se desplaza sola.
    fn drag_tick(&mut self) {
        // Con saltos largos (la ventana no se dibujaba) no se desplaza de golpe.
        let dt = self.drag_at.replace(Instant::now()).map_or(0.0, |t| t.elapsed().as_secs_f32().min(0.05));
        let (x, y) = self.cursor;
        match self.drag.clone() {
            Some(Drag::Chat(false)) => {
                self.chat_drag_tick(dt);
                if self.chats.current().is_some_and(|c| chat::outside(c.view, y) != 0.0) {
                    self.redraw();
                }
            }
            Some(Drag::Select(id)) => {
                let Some(gfx) = &self.gfx else { return };
                let (pad, cell) = (gfx.metrics.pad, gfx.metrics.cell_h);
                let Some((_, a)) = self.pane_areas.iter().find(|(p, _)| *p == id).copied() else { return };
                let inner = Area::new(a.x, a.y + pad, a.w, (a.h - 2.0 * pad).max(cell));
                let over = chat::outside(inner, y);
                if over == 0.0 {
                    return;
                }
                self.scroll_accum -= (chat::scroll_speed(over, inner.h) * dt / cell) as f64;
                let lines = self.scroll_accum.trunc();
                self.scroll_accum -= lines;
                if lines != 0.0 {
                    if let Some(p) = self.panes.get(&id) {
                        p.session.scroll(lines as i32);
                    }
                    self.extend_selection(id, x, y);
                }
                self.redraw();
            }
            _ => self.drag_at = None,
        }
    }

    /// La selección de una terminal llega hasta un punto.
    fn extend_selection(&self, id: PaneId, x: f32, y: f32) {
        let (Some(gfx), Some(p)) = (&self.gfx, self.panes.get(&id)) else { return };
        let Some((_, a)) = self.pane_areas.iter().find(|(p, _)| *p == id).copied() else { return };
        let size = p.session.size();
        let (row, col, right) = gfx.cell_at(a, x, y, size.rows as usize, size.cols as usize);
        // Por encima se coge la línea entera desde su principio; por debajo, hasta el final.
        let over = chat::outside(Area::new(a.x, a.y, a.w, a.h), y);
        let (col, right) = match over {
            o if o < 0.0 => (0, false),
            o if o > 0.0 => (size.cols as usize, true),
            _ => (col, right),
        };
        p.session.update_selection(row, col, right);
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Karbyx")
            .with_inner_size(LogicalSize::new(1320.0, 800.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("no se pudo crear la ventana: {e}");
                event_loop.exit();
                return;
            }
        };
        // Sin esto macOS no compone los acentos (´ + a).
        window.set_ime_allowed(true);
        match pollster::block_on(Gfx::new(window.clone(), event_loop, self.font_spec())) {
            Ok(g) => self.gfx = Some(g),
            Err(e) => {
                log::error!("no se pudo iniciar la GPU: {e}");
                event_loop.exit();
                return;
            }
        }
        // Canal de hooks de Claude Code (antes de abrir sesiones: va en su entorno).
        let proxy = self.proxy.clone();
        self.agent_socket = agents::start_server(move |m| {
            let _ = proxy.send_event(match m {
                agents::Incoming::Hook(h) => UserEvent::Hook(h),
                agents::Incoming::Query(q) => UserEvent::Query(q),
            });
        });
        if let Ok(exe) = std::env::current_exe() {
            self.claude_settings = agents::write_claude_settings(&exe);
        }
        // Latido para animaciones y la heurística de agentes (solo si hay agentes).
        let (need, wake) = (self.tick_needed.clone(), self.waker());
        std::thread::Builder::new()
            .name("tick".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(400));
                    if need.load(Ordering::Relaxed) {
                        wake();
                    }
                }
            })
            .ok();
        if !self.open_tab(PaneKind::Shell) {
            event_loop.exit();
            return;
        }
        // Se abre en el chat (o donde diga `start`); en modo captura, solo si
        // se pide con `KARBYX_CAPTURE_CHAT` o `KARBYX_CAPTURE_BOARD`.
        self.board_visible = if capture::target().is_some() {
            std::env::var_os("KARBYX_CAPTURE_BOARD").is_some()
        } else {
            self.config.window.start == "mesa"
        };
        let start_chat = if capture::target().is_some() {
            std::env::var_os("KARBYX_CAPTURE_CHAT").is_some()
        } else {
            !matches!(self.config.window.start.as_str(), "mesa" | "terminal")
        };
        if start_chat {
            self.show_chat(true);
            self.capture_chat();
        }
        self.probe_servers();
        if capture::target().is_some() {
            // En modo captura: una segunda pestaña y la primera dividida en dos.
            self.open_tab(PaneKind::Shell);
            self.set_active(0);
            self.split(SplitDir::Horizontal);
            if std::env::var_os("KARBYX_CAPTURE_WORKTREE").is_some() {
                // Agente en un worktree, como con ⌘⇧A.
                self.open_agent_worktree();
            } else if std::env::var_os("KARBYX_CAPTURE_AI").is_some() {
                self.open_tab(PaneKind::Ai(Provider::Claude));
            }
        }
        // `KARBYX_CAPTURE_LAUNCH="tarea"`: lanza esa tarea desde la mesa al arrancar.
        if capture::target().is_some()
            && let Ok(task) = std::env::var("KARBYX_CAPTURE_LAUNCH")
        {
            self.board.input = task;
            self.board_launch();
        }
        // `KARBYX_CAPTURE_INPUT="texto"`: deja ese texto escrito en la barra de la mesa.
        if capture::target().is_some()
            && let Ok(text) = std::env::var("KARBYX_CAPTURE_INPUT")
        {
            self.board.input = text;
        }
        self.usage = Some(usage::spawn(self.waker()));
        let proxy = self.proxy.clone();
        config::watch(move |c| {
            let _ = proxy.send_event(UserEvent::Config(Box::new(c)));
        });
        if std::env::var_os("KARBYX_SELFTEST").is_some() {
            spawn_selftest(self.proxy.clone());
        }
        // `KARBYX_TYPE="cmd1\ncmd2\n"`: en modo captura, teclea esos comandos al arrancar.
        if capture::target().is_some()
            && let Ok(text) = std::env::var("KARBYX_TYPE")
        {
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                for line in text.split('\n').filter(|l| !l.is_empty()) {
                    let _ = proxy.send_event(UserEvent::Type(format!("{line}\r")));
                    std::thread::sleep(Duration::from_millis(700));
                }
            });
        }
        window.request_redraw();
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(sock) = &self.agent_socket {
            agents::cleanup(sock);
        }
        self.close_chats();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Type(text) => {
                if let Some(p) = self.focused_pane() {
                    p.session.write(text.into_bytes());
                }
            }
            UserEvent::Config(c) => self.apply_config(*c),
            UserEvent::Hook(m) => self.on_hook(m),
            UserEvent::Query(q) => self.on_query(q),
            UserEvent::Review(r) => self.on_review(r),
            UserEvent::BoardStats(s) => self.on_board_stats(s),
            UserEvent::Verify(pane, generation, r) => self.on_verify(pane, generation, r),
            UserEvent::Chat(id, e) => self.on_chat_event(id, e),
            UserEvent::Probe(server, r) => self.on_probe(*server, r),
            UserEvent::McpProbe(name, config, r) => self.on_mcp_probe(name, *config, r),
            UserEvent::McpFound(q, p, r) => self.on_mcp_found(q, p, r),
            UserEvent::Skills(t, r) => self.on_skills(t, *r),
            UserEvent::SkillHits(t, r) => self.on_skill_hits(t, r),
            UserEvent::Folder(p) => self.set_chat_project(Some(p)),
            UserEvent::Toast(t) => self.toast(t),
            UserEvent::ChatSay(t) if t == "!discard" => self.review_click(Hit::ReviewDiscard, event_loop),
            UserEvent::ChatSay(t) => self.chat_say(t),
            UserEvent::WritePane(pane, bytes) => {
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.session.write(bytes);
                }
            }
            UserEvent::Wake => {
                self.wake_pending.store(false, Ordering::Release);
                // Las sesiones se atienden aunque la ventana no se vea
                // (respuestas a la shell, sesiones que terminan...).
                self.pump_sessions(event_loop);
                self.redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(g) = &mut self.gfx {
                    g.resize_surface(size.width, size.height);
                }
                self.redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                let spec = self.font_spec();
                if let Some(g) = &mut self.gfx {
                    g.set_font(spec);
                }
                self.redraw();
            }
            WindowEvent::Occluded(o) => {
                self.occluded = o;
                self.redraw();
            }
            WindowEvent::Focused(f) => {
                self.focused = f;
                self.redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                // El chat edita texto: sus teclas van antes que los atajos.
                if self.chat_visible
                    && self.review.is_none()
                    && self.inbox.is_none()
                    && self.dashboard.is_none()
                    && self.chat_key(&event)
                {
                    return;
                }
                // Los atajos de la terminal tienen prioridad, también sobre la búsqueda.
                if let Some(a) = self.bindings.lookup(&event, self.mods) {
                    self.handle_action(a, event_loop);
                    return;
                }
                if self.review.is_some() && self.review_key(&event, event_loop) {
                    return;
                }
                if self.chat_visible && self.inbox.is_none() && self.dashboard.is_none() {
                    return;
                }
                if self.board_visible && self.board_key(&event) {
                    return;
                }
                if self.inbox.is_some() && self.inbox_key(&event) {
                    return;
                }
                if self.dashboard.is_some() && self.dash_key(&event) {
                    return;
                }
                if self.selected_block.is_some() && event.logical_key == Key::Named(NamedKey::Escape) {
                    self.selected_block = None;
                    self.redraw();
                    return;
                }
                if self.search.is_some() && self.search_key(&event) {
                    return;
                }
                let Some(mode) = self.focused_pane().map(|p| p.session.mode()) else { return };
                let outcome = input::translate(
                    &event,
                    self.mods,
                    mode,
                    &self.bindings,
                    self.config.terminal.option_as_alt,
                );
                match outcome {
                    KeyOutcome::Bytes(b) => {
                        self.selected_block = None;
                        if let Some(pane) = self.focused_pane() {
                            pane.session.clear_selection();
                            pane.session.write(b);
                        }
                    }
                    KeyOutcome::Action(a) => self.handle_action(a, event_loop),
                    KeyOutcome::None => {}
                }
            }
            // Acentos y otros caracteres compuestos.
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => self.ime_text(&text),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32, position.y as f32);
                self.mouse_move();
            }
            WindowEvent::CursorLeft { .. } => {
                if self.hover.take().is_some() {
                    self.redraw();
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button, .. } => {
                if self.review.is_some() {
                    match ui::hit_at(&self.hits, self.cursor.0, self.cursor.1) {
                        Some(h @ (Hit::ReviewFile(_) | Hit::ReviewMerge | Hit::ReviewDiscard)) => {
                            self.review_click(h, event_loop)
                        }
                        Some(Hit::ReviewBox) => {}
                        _ => {
                            self.review = None;
                            self.redraw();
                        }
                    }
                    return;
                }
                if self.inbox.is_some()
                    && !matches!(
                        ui::hit_at(&self.hits, self.cursor.0, self.cursor.1),
                        Some(Hit::InboxItem(_))
                    )
                {
                    self.inbox = None;
                    self.redraw();
                    return;
                }
                if self.dashboard.is_some()
                    && !matches!(
                        ui::hit_at(&self.hits, self.cursor.0, self.cursor.1),
                        Some(Hit::DashAgent(_))
                    )
                {
                    self.dashboard = None;
                    self.redraw();
                    return;
                }
                self.mouse_press(button, event_loop)
            }
            WindowEvent::MouseInput { state: ElementState::Released, button, .. } => {
                self.mouse_release(button)
            }
            WindowEvent::MouseWheel { delta, .. } => self.wheel(delta),
            WindowEvent::RedrawRequested => self.render(event_loop),
            _ => {}
        }
    }
}

/// ¿Se ve el panel de consumo? Manda el ajuste manual; si no hay, solo
/// cuando la pestaña tiene una sesión de IA.
fn right_panel_visible(manual: Option<bool>, tab_has_ai: bool) -> bool {
    manual.unwrap_or(tab_has_ai)
}

/// Texto del estado de un agente.
fn state_text(s: AgentState) -> &'static str {
    match s {
        AgentState::Working => "Trabajando…",
        AgentState::Waiting => "Te espera",
        AgentState::Done => "Terminado",
    }
}

fn main() -> anyhow::Result<()> {
    // `karbyx-app hook`: lo invocan los hooks de Claude Code.
    if std::env::args().nth(1).as_deref() == Some("hook") {
        agents::run_hook_client();
        return Ok(());
    }
    // `karbyx-app mcp`: servidor MCP que arrancan los asistentes.
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        mcp::run();
        return Ok(());
    }
    clean_inherited_env();
    complete_gui_env();
    disable_press_and_hold();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let mut app = App::new(event_loop.create_proxy(), Config::load());
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_lang_from_apple_locale() {
        let all = |_: &str| true;
        assert_eq!(gui_lang("es_ES\n", all), "es_ES.UTF-8");
        assert_eq!(gui_lang("es_ES@rg=eszzzz", all), "es_ES.UTF-8");
        assert_eq!(gui_lang("", all), "en_US.UTF-8");
        assert_eq!(gui_lang("xx_YY", |_| false), "en_US.UTF-8");
    }

    #[test]
    fn ask_prompt_includes_command_output_and_code() {
        let t = karbyx_term::BlockText {
            command: Some("cargo build".into()),
            output: "error[E0308]: mismatched types".into(),
            exit: Some(101),
        };
        let p = ask_prompt(&t, Some(std::path::Path::new("/Users/x/proyecto")));
        assert!(p.contains("$ cargo build"));
        assert!(p.contains("error[E0308]"));
        assert!(p.contains("código 101"));
        assert!(p.contains("/Users/x/proyecto"));
        let vacio = ask_prompt(&karbyx_term::BlockText::default(), None);
        assert!(vacio.contains("(sin salida)"));
    }

    #[test]
    fn ai_command_quotes_prompt_path() {
        let path = std::path::Path::new("/tmp/it's here/p.txt");
        let c = ai_command_with_prompt(Provider::Claude, path);
        assert!(
            c.contains(r#"--settings "$KARBYX_CLAUDE_SETTINGS" "$(cat '/tmp/it'\''s here/p.txt')""#),
            "{c}"
        );
        // Sin Karbyx (variable vacía) el comando queda como `claude "<pregunta>"`.
        assert!(ai_command(Provider::Codex.command()).contains("then codex;"));
        let g = ai_command_with_prompt(Provider::Gemini, path);
        assert!(g.contains("gemini -i"), "{g}");
        // Comprobado con sh: la comilla del nombre no rompe el comando.
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf %s '{}'", path.to_string_lossy().replace('\'', "'\\''")))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), path.to_string_lossy());
    }

    /// El comando de lanzar Claude debe pasar `--settings` y la ruta como dos
    /// argumentos en zsh y en sh (zsh no parte `${VAR:+...}` en palabras).
    #[test]
    fn claude_launch_args_in_zsh_and_sh() {
        let dir = std::env::temp_dir().join(format!("karbyx-fake-claude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("claude");
        std::fs::write(&fake, "#!/bin/sh\nfor a in \"$@\"; do printf '[%s]' \"$a\"; done\n").unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
        for shell in ["/bin/zsh", "/bin/sh"] {
            if !std::path::Path::new(shell).exists() {
                continue;
            }
            let run = |settings: Option<&str>| {
                let mut c = std::process::Command::new(shell);
                c.arg("-c").arg(launch("claude", "\"hola mundo\"")).env("PATH", &path);
                match settings {
                    Some(v) => c.env("KARBYX_CLAUDE_SETTINGS", v).env("KARBYX_CLAUDE_MCP", "/m c/mcp.json"),
                    None => c.env_remove("KARBYX_CLAUDE_SETTINGS"),
                };
                String::from_utf8_lossy(&c.output().unwrap().stdout).into_owned()
            };
            assert_eq!(
                run(Some("/x y/hooks.json")),
                "[--mcp-config][/m c/mcp.json][--settings][/x y/hooks.json][hola mundo]",
                "{shell}"
            );
            assert_eq!(run(None), "[hola mundo]", "{shell}");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn usage_panel_visibility() {
        assert!(!right_panel_visible(None, false), "sin IA no se ve");
        assert!(right_panel_visible(None, true), "con IA aparece solo");
        assert!(right_panel_visible(Some(true), false), "⌘⌥B lo fuerza");
        assert!(!right_panel_visible(Some(false), true), "⌘⌥B lo oculta");
    }
}

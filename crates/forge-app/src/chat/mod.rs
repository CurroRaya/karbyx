// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! El chat: conversaciones con modelos locales que trabajan en un proyecto.
//! Aquí está el estado y lo que hace cada tecla y cada clic; `draw` lo pinta.

pub mod draw;
pub mod edit;
pub mod extend;
pub mod md;
pub mod panels;
pub mod store;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use forge_agent::agent::{Agent, Ask, Decision, Event, Setup};
use forge_agent::api::{self, Model, Msg, Server, Stats};
use forge_agent::http::Base;
use forge_agent::know::{Know, Scope, Skill};
use forge_agent::tools::{self, Detail};
use winit::event::KeyEvent;
use winit::keyboard::{Key, NamedKey};

use crate::gfx::TextPos;
use crate::owner;
use crate::ui::{Area, Hit};
use crate::worktree::Worktree;
use crate::{App, PaneKind, Spawn, UserEvent};
use edit::{Editor, Move};
use extend::{McpForm, McpPanel, McpSearch, McpView, SkillForm};
use forge_ai::Provider;
use store::{Saved, Stored, StoredItem};

/// Salida en vivo de un comando que se guarda para enseñar.
const LIVE_MAX: usize = 24 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatHit {
    New,
    Open(u64),
    Delete(u64),
    Send,
    Stop,
    Mode,
    ModelMenu,
    ProjectMenu,
    /// (servidor, modelo)
    PickModel(u16, u16),
    PickClaude,
    AddServer,
    EditServer(u16),
    RemoveServer(u16),
    PickProject(u16),
    PickNoProject,
    BrowseProject,
    Allow(u64),
    AllowAll(u64),
    Deny(u64),
    /// Desplegar o plegar un elemento.
    Toggle(u16),
    /// Copiar el código del trozo (elemento, trozo) o la respuesta entera.
    CopyCode(u16, u16),
    CopyAnswer(u16),
    /// Resumir la conversación ahora.
    Compact,
    /// Panel de memoria: abrirlo, olvidar una nota, abrir su archivo, y la
    /// carpeta de las habilidades.
    Memory,
    ForgetNote(bool, u16),
    MemoryFile(bool),
    SkillsFolder,
    /// Habilidad del menú «/».
    PickSkill(u16),
    /// Servidores MCP: panel, añadir, traer, estado, herramientas...
    Mcp,
    McpAdd,
    McpAddOk,
    McpImport,
    McpState(u16),
    McpApprove(u16),
    McpRemove(u16),
    McpSignIn(u16),
    /// Buscador: abrirlo, buscar, ficha, forma de conectar, dato, opción del
    /// dato, revisar y ver su código.
    McpSearch,
    McpSearchGo,
    McpFound(u16),
    McpChoice(u16),
    McpInput(u16),
    McpOption(u16, u16),
    McpReview,
    McpRepo,
    /// Clave de un servidor, y campo en que se escribe.
    McpKey(u16),
    McpField(bool),
    McpSignOut(u16),
    McpOpen(u16),
    McpTool(u16, u16),
    McpTrust(u16, u16),
    /// Instalar habilidades.
    SkillAdd,
    SkillFetch,
    SkillHit(u16),
    SkillPick(u16),
    SkillInstall,
    ExtendBack,
    /// Permitir siempre una herramienta MCP.
    AllowTool(u64),
    /// Revisar los cambios de su copia.
    Review,
    CopyCommand(u8),
    Suggest(u8),
    Bottom,
    Field(u8),
    FormOk,
    FormCancel,
    /// Fondo de un menú o formulario: no hace nada.
    Inside,
}

/// Estado de un servidor.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Checking,
    Ready(Vec<Model>),
    Down(String),
}

pub struct ServerView {
    pub server: Server,
    pub status: Status,
    checked: Instant,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolState {
    Asking(Ask),
    /// Aceptada o libre, aún sin empezar.
    Queued,
    Running,
    Done {
        ok: bool,
        summary: String,
        took: Duration,
    },
}

pub struct ToolItem {
    pub id: u64,
    pub verb: String,
    pub target: String,
    pub detail: Detail,
    pub state: ToolState,
    /// Salida (en vivo y luego la final).
    pub output: String,
    pub since: Instant,
}

pub enum Body {
    User(String),
    Assistant(String),
    Think {
        text: String,
        since: Instant,
        took: Option<Duration>,
    },
    Tool(Box<ToolItem>),
    /// Lo anterior se resumió (tokens antes y después).
    Summary {
        text: String,
        before: u32,
        after: u32,
    },
    Notice(String),
    Error(String),
}

pub struct Item {
    pub body: Body,
    pub open: bool,
    /// Cambia con cada modificación (invalida lo maquetado).
    pub rev: u64,
    pub cache: Option<draw::Cached>,
}

impl Item {
    fn new(body: Body) -> Self {
        Self { body, open: false, rev: 0, cache: None }
    }
}

/// Qué está haciendo el modelo ahora mismo.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    /// Leyendo la conversación: aún no ha escrito nada.
    Reading(Instant),
    Writing,
    /// Escribiendo una llamada a herramienta.
    Preparing(String, usize),
    /// Resumiendo la conversación.
    Compacting(Instant),
    /// Arrancando un servidor MCP.
    Starting(String, Instant),
    /// Ejecutando o esperando permiso.
    Tool,
    Stopping,
}

/// Posición en el texto de la conversación: elemento, trozo y lugar.
pub type DocPos = (usize, usize, TextPos);

pub struct Chat {
    pub id: u64,
    pub title: String,
    pub items: Vec<Item>,
    /// Se crea al abrir la conversación.
    agent: Option<Agent>,
    /// Dónde se guarda, y último cambio (segundos desde 1970).
    file: Option<PathBuf>,
    pub updated: i64,
    /// Ya se leyó del disco (o es nueva).
    loaded: bool,
    /// Lo que ve el modelo, para guardarlo.
    history: Vec<Msg>,
    pub input: Editor,
    pub phase: Phase,
    pub root: Option<PathBuf>,
    /// Copia del proyecto donde trabaja, si se lanzó desde la mesa.
    pub worktree: Option<Worktree>,
    /// Su trabajo ya se fusionó o se descartó: no hay nada que revisar.
    settled: bool,
    pub stats: Option<Stats>,
    /// Mensaje escrito mientras trabajaba: se envía al acabar.
    pub queued: Option<String>,
    /// Terminó o pregunta algo y no se ha visto.
    pub unseen: bool,
    pub scroll: f32,
    /// Pegado al final (sigue lo que llega).
    pub stick: bool,
    pub view_h: f32,
    pub content_h: f32,
    pub sel: Option<(DocPos, DocPos)>,
    /// Textos visibles en el último frame, para la selección.
    pub placed: Vec<draw::Placed>,
    /// Caja de escritura en el último frame.
    pub input_at: Option<draw::Placed>,
}

impl Chat {
    fn empty(id: u64) -> Self {
        Self {
            id,
            title: String::new(),
            items: Vec::new(),
            agent: None,
            file: None,
            updated: 0,
            loaded: true,
            history: Vec::new(),
            input: Editor::default(),
            phase: Phase::Idle,
            root: None,
            worktree: None,
            settled: false,
            stats: None,
            queued: None,
            unseen: false,
            scroll: 0.0,
            stick: true,
            view_h: 0.0,
            content_h: 0.0,
            sel: None,
            placed: Vec::new(),
            input_at: None,
        }
    }

    pub fn busy(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// Acción que espera permiso.
    pub fn asking(&self) -> Option<&ToolItem> {
        self.items.iter().rev().find_map(|i| match &i.body {
            Body::Tool(t) if matches!(t.state, ToolState::Asking(_)) => Some(&**t),
            _ => None,
        })
    }

    pub fn auto(&self) -> bool {
        self.agent.as_ref().is_some_and(Agent::auto)
    }

    /// Memoria y habilidades de esta conversación.
    pub fn know(&self) -> Know {
        let project = self.worktree.as_ref().map(|w| w.repo.clone()).or(self.root.clone());
        Know {
            memory: store::memory_dir(),
            skills: store::skills_dir(),
            project,
            root: self.root.clone(),
            // En una tarea que trabaja sola no hay quien apruebe lo que se guarda.
            writable: self.worktree.is_none(),
        }
    }

    /// Nombre del proyecto (el del repositorio, si trabaja en una copia).
    pub fn project(&self) -> String {
        let dir = self.worktree.as_ref().map(|w| &w.repo).or(self.root.as_ref());
        dir.map_or_else(|| "sin carpeta".into(), |p| folder_name(p))
    }

    /// Nueva y sin usar.
    pub fn blank(&self) -> bool {
        self.loaded && self.items.is_empty()
    }

    /// Lo que se guarda en disco.
    fn stored(&self) -> Stored {
        let ms = |d: Duration| d.as_millis() as u64;
        let items = self
            .items
            .iter()
            .map(|i| match &i.body {
                Body::User(t) => StoredItem::User(t.clone()),
                Body::Assistant(t) => StoredItem::Assistant(t.clone()),
                Body::Summary { text, before, after } => {
                    StoredItem::Summary { text: text.clone(), before: *before, after: *after }
                }
                Body::Notice(t) => StoredItem::Notice(t.clone()),
                Body::Error(t) => StoredItem::Error(t.clone()),
                Body::Think { text, since, took } => {
                    StoredItem::Think { text: text.clone(), ms: ms(took.unwrap_or_else(|| since.elapsed())) }
                }
                Body::Tool(t) => {
                    let (ok, summary, took) = match &t.state {
                        ToolState::Done { ok, summary, took } => (*ok, summary.clone(), *took),
                        _ => (false, "interrumpido".into(), Duration::ZERO),
                    };
                    // De las salidas largas basta el final: es solo para verlas.
                    let mut from = t.output.len().saturating_sub(LIVE_MAX);
                    while !t.output.is_char_boundary(from) {
                        from += 1;
                    }
                    StoredItem::Tool {
                        verb: t.verb.clone(),
                        target: t.target.clone(),
                        detail: t.detail.clone(),
                        ok,
                        summary,
                        ms: ms(took),
                        output: t.output[from..].to_string(),
                    }
                }
            })
            .collect();
        Stored { busy: self.busy(), stats: self.stats, history: self.history.clone(), items }
    }

    /// Recupera lo guardado.
    fn restore(&mut self, s: Stored) {
        let now = Instant::now();
        self.items = s
            .items
            .into_iter()
            .map(|i| {
                Item::new(match i {
                    StoredItem::User(t) => Body::User(t),
                    StoredItem::Assistant(t) => Body::Assistant(t),
                    StoredItem::Summary { text, before, after } => Body::Summary { text, before, after },
                    StoredItem::Notice(t) => Body::Notice(t),
                    StoredItem::Error(t) => Body::Error(t),
                    StoredItem::Think { text, ms } => {
                        Body::Think { text, since: now, took: Some(Duration::from_millis(ms)) }
                    }
                    StoredItem::Tool { verb, target, detail, ok, summary, ms, output } => {
                        let took = Duration::from_millis(ms);
                        Body::Tool(Box::new(ToolItem {
                            // Ninguna acción viva lleva este número.
                            id: 0,
                            verb,
                            target,
                            detail,
                            state: ToolState::Done { ok, summary, took },
                            output,
                            since: now,
                        }))
                    }
                })
            })
            .collect();
        if s.busy {
            self.items.push(Item::new(Body::Notice("Se quedó a medias al cerrar forge.".into())));
        }
        self.stats = s.stats;
        self.history = s.history;
    }

    /// Guarda la conversación.
    fn save(&mut self) -> std::io::Result<()> {
        if !self.loaded || self.items.is_empty() {
            return Ok(());
        }
        if self.file.is_none() {
            self.file = store::new_file();
        }
        let Some(f) = &self.file else { return Ok(()) };
        let meta = store::Meta {
            v: store::VERSION,
            title: self.title.clone(),
            root: self.root.clone(),
            worktree: self.worktree.clone(),
            settled: self.settled,
            updated: self.updated,
        };
        store::store(f, &meta, &self.stored())
    }

    fn tool(&mut self, id: u64) -> Option<&mut ToolItem> {
        self.items.iter_mut().rev().find_map(|i| match &mut i.body {
            Body::Tool(t) if t.id == id => {
                i.rev += 1;
                Some(&mut **t)
            }
            _ => None,
        })
    }

    /// Cierra el razonamiento en curso.
    fn end_thinking(&mut self) {
        if let Some(Item { body: Body::Think { since, took, .. }, rev, .. }) = self.items.last_mut()
            && took.is_none()
        {
            *took = Some(since.elapsed());
            *rev += 1;
        }
    }

    /// Mientras se para, lo que aún llega no cambia el estado.
    fn set_phase(&mut self, p: Phase) {
        if self.phase != Phase::Stopping {
            self.phase = p;
        }
    }

    fn on_event(&mut self, e: Event) {
        match e {
            Event::Thinking => self.set_phase(Phase::Reading(Instant::now())),
            Event::Text(t) => {
                self.end_thinking();
                self.set_phase(Phase::Writing);
                match self.items.last_mut() {
                    Some(Item { body: Body::Assistant(s), rev, .. }) => {
                        s.push_str(&t);
                        *rev += 1;
                    }
                    // Los saltos sueltos antes de una herramienta no son un mensaje.
                    _ if t.trim().is_empty() => {}
                    _ => self.items.push(Item::new(Body::Assistant(t.trim_start().to_string()))),
                }
            }
            Event::Reasoning(t) => {
                self.set_phase(Phase::Writing);
                match self.items.last_mut() {
                    Some(Item { body: Body::Think { text, took: None, .. }, rev, .. }) => {
                        text.push_str(&t);
                        *rev += 1;
                    }
                    _ => self.items.push(Item::new(Body::Think {
                        text: t.trim_start().to_string(),
                        since: Instant::now(),
                        took: None,
                    })),
                }
            }
            Event::Preparing(name, n) => {
                self.end_thinking();
                self.set_phase(Phase::Preparing(name, n));
            }
            Event::Action { id, verb, target, detail, ask } => {
                self.end_thinking();
                self.set_phase(Phase::Tool);
                let state = ask.map_or(ToolState::Queued, ToolState::Asking);
                let since = Instant::now();
                let verb = verb.to_string();
                let t = ToolItem { id, verb, target, detail, state, output: String::new(), since };
                self.items.push(Item::new(Body::Tool(Box::new(t))));
            }
            Event::Running(id) => {
                if let Some(t) = self.tool(id) {
                    t.state = ToolState::Running;
                    t.since = Instant::now();
                }
            }
            Event::Output(id, s) => {
                if let Some(t) = self.tool(id) {
                    t.output.push_str(&forge_agent::exec::strip_ansi(&s));
                    if t.output.len() > 2 * LIVE_MAX {
                        let mut cut = t.output.len() - LIVE_MAX;
                        while !t.output.is_char_boundary(cut) {
                            cut += 1;
                        }
                        t.output.drain(..cut);
                    }
                }
            }
            Event::Finished { id, ok, summary, output, took } => {
                if let Some(t) = self.tool(id) {
                    t.state = ToolState::Done { ok, summary, took };
                    // De un comando ya se vio su salida; la final lleva notas para el modelo.
                    if t.output.is_empty() || !matches!(t.detail, Detail::Command { .. }) {
                        t.output = output;
                    }
                }
            }
            Event::Stats(s) => {
                let speed = s.speed.or(self.stats.and_then(|o| o.speed));
                self.stats = Some(Stats { speed, ..s });
            }
            Event::Starting(n) => self.set_phase(Phase::Starting(n, Instant::now())),
            Event::Compacting => {
                self.end_thinking();
                self.set_phase(Phase::Compacting(Instant::now()));
            }
            Event::Compacted { summary, before, after } => {
                self.items.push(Item::new(Body::Summary { text: summary, before, after }));
                if let Some(s) = &mut self.stats {
                    (s.prompt, s.output) = (after, 0);
                }
            }
            Event::History(h) => self.history = h,
            Event::Notice(m) => self.items.push(Item::new(Body::Notice(m))),
            Event::Error(m) => self.items.push(Item::new(Body::Error(m))),
            Event::Done => {
                self.end_thinking();
                if self.phase == Phase::Stopping {
                    self.items.push(Item::new(Body::Notice("Parado.".into())));
                }
                self.phase = Phase::Idle;
                self.unseen = true;
            }
        }
    }

    /// Texto plano de la selección.
    fn selected_text(&self) -> Option<String> {
        let (a, b) = self.sel?;
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let mut out = String::new();
        let mut last: Option<usize> = None;
        for (i, item) in self.items.iter().enumerate().take(b.0 + 1).skip(a.0) {
            let Some(c) = &item.cache else { continue };
            for (j, text) in c.texts() {
                if (i, j) < (a.0, a.1) || (i, j) > (b.0, b.1) {
                    continue;
                }
                let at = |p: TextPos| {
                    let mut e = Editor::default();
                    e.set(text.clone());
                    e.byte(p)
                };
                let from = if (i, j) == (a.0, a.1) { at(a.2) } else { 0 };
                let to = if (i, j) == (b.0, b.1) { at(b.2) } else { text.len() };
                if let Some(l) = last {
                    out.push_str(if l == i { "\n" } else { "\n\n" });
                }
                last = Some(i);
                out.push_str(text.get(from..to).unwrap_or_default());
            }
        }
        (!out.trim().is_empty()).then_some(out)
    }
}

/// Una conversación tal como se ve en la mesa.
pub struct ChatCard {
    pub id: u64,
    pub title: String,
    pub project: String,
    /// Su copia del proyecto; cambios y verificación, si los hay.
    pub branch: Option<String>,
    pub stats: Option<crate::board::Stats>,
    pub verify: Option<crate::board::Badge>,
    pub can_verify: bool,
    pub detail: String,
    /// Última línea de la respuesta.
    pub last: String,
    /// Acción que espera permiso, y por qué no se decide desde la mesa.
    pub ask: Option<u64>,
    pub warn: Option<String>,
    pub busy: bool,
    pub unseen: bool,
    pub used: Option<u32>,
    pub since: i64,
}

impl ChatCard {
    /// Lleva la fila de verificación.
    pub fn has_badge(&self) -> bool {
        self.branch.is_some() && (self.verify.is_some() || self.can_verify)
    }
}

/// Menú o formulario abierto sobre el chat.
#[derive(Default)]
pub enum Popup {
    #[default]
    None,
    Models,
    Projects,
    Memory(Box<MemoryView>),
    Mcp(Box<McpPanel>),
    McpForm(Box<McpForm>),
    McpSearch(Box<McpSearch>),
    Skill(Box<SkillForm>),
    Form(Box<Form>),
}

/// Lo que enseña el panel de memoria.
pub struct MemoryView {
    pub user: Vec<String>,
    pub project: Vec<String>,
    /// Nombre del proyecto, si hay.
    pub name: Option<String>,
    pub skills: Vec<Skill>,
}

/// Formulario de un servidor.
#[derive(Default)]
pub struct Form {
    pub url: Editor,
    pub key: Editor,
    pub focus: u8,
    pub status: Option<Result<(), String>>,
    /// Servidor que se edita (si no, uno nuevo).
    pub edit: Option<usize>,
    /// Clave sugerida para arrancar el servidor.
    pub suggested: String,
}

pub struct Chats {
    pub list: Vec<Chat>,
    pub cur: usize,
    next: u64,
    pub servers: Vec<ServerView>,
    pub saved: Saved,
    pub popup: Popup,
    /// Conversación que se pide confirmar borrar.
    pub confirm: Option<u64>,
    /// Aviso breve («Copiado»).
    pub toast: Option<(String, Instant)>,
    /// Barra lateral: desplazamiento, lo que sobra, y su zona.
    pub side_scroll: f32,
    pub side_max: f32,
    pub side_area: Area,
    /// Habilidades para el menú «/», la elegida, y si se cerró con esc.
    pub skills: Vec<Skill>,
    pub slash_sel: usize,
    pub slash_off: bool,
    /// Servidores MCP, y por qué no se pudieron leer.
    pub mcp: Vec<McpView>,
    pub mcp_error: Option<String>,
    /// Para cancelar el inicio de sesión en curso.
    pub signing: Arc<AtomicBool>,
    /// Ya se avisó de que no se puede guardar.
    save_failed: bool,
}

impl Chats {
    pub fn new() -> Self {
        let saved = Saved::load();
        let servers = saved
            .servers
            .iter()
            .map(|s| ServerView { server: s.clone(), status: Status::Checking, checked: Instant::now() })
            .collect();
        forge_agent::oauth::set_path(store::sessions_path());
        let mut stored = store::list();
        stored.sort_by_key(|(_, m)| m.updated);
        let list: Vec<Chat> = stored
            .into_iter()
            .enumerate()
            .map(|(i, (file, m))| Chat {
                title: m.title,
                // Si su copia ya no existe, se queda como conversación.
                worktree: m.worktree.clone().filter(|w| w.path.is_dir()),
                root: m.root.filter(|r| m.worktree.is_none() || r.is_dir()),
                settled: m.settled,
                updated: m.updated,
                file: Some(file),
                loaded: false,
                ..Chat::empty(i as u64 + 1)
            })
            .collect();
        Self {
            next: list.len() as u64 + 1,
            list,
            cur: 0,
            servers,
            saved,
            popup: Popup::None,
            confirm: None,
            toast: None,
            side_scroll: 0.0,
            side_max: 0.0,
            side_area: Area::default(),
            skills: Vec::new(),
            slash_sel: 0,
            slash_off: false,
            mcp: Vec::new(),
            mcp_error: None,
            signing: Arc::default(),
            save_failed: false,
        }
    }

    /// Conversaciones con algo dentro, la última primero.
    pub fn recent(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.list.len()).filter(|i| !self.list[*i].blank()).collect();
        v.sort_by_key(|i| std::cmp::Reverse((self.list[*i].updated, self.list[*i].id)));
        v
    }

    /// Habilidades que encajan con lo escrito tras «/», si el menú toca.
    pub fn slash(&self) -> Vec<&Skill> {
        let Some(c) = self.current().filter(|_| !self.slash_off && matches!(self.popup, Popup::None)) else {
            return Vec::new();
        };
        let Some(q) = c.input.text().strip_prefix('/').filter(|q| !q.contains(char::is_whitespace)) else {
            return Vec::new();
        };
        let mut v: Vec<&Skill> = self.skills.iter().filter(|s| s.name.contains(q)).collect();
        // Las que empiezan así, primero.
        v.sort_by_key(|s| !s.name.starts_with(q));
        v.truncate(8);
        v
    }

    pub fn by_id(&self, id: u64) -> Option<&Chat> {
        self.list.iter().find(|c| c.id == id)
    }

    pub fn current(&self) -> Option<&Chat> {
        self.list.get(self.cur)
    }

    pub fn current_mut(&mut self) -> Option<&mut Chat> {
        self.list.get_mut(self.cur)
    }

    /// Modelo elegido; si no hay o ya no está, el primero disponible.
    pub fn model(&self) -> Option<(&Server, &Model)> {
        let ready = || {
            self.servers.iter().filter_map(|s| match &s.status {
                Status::Ready(m) => Some((&s.server, m)),
                _ => None,
            })
        };
        if let Some((url, id)) = &self.saved.model {
            if let Some((s, m)) = ready().find(|(s, _)| s.url == *url) {
                // El mismo servidor puede haber cargado otro modelo.
                return m.iter().find(|m| m.id == *id).or(m.first()).map(|m| (s, m));
            }
            // Está apagado: no se cambia de servidor sin que el usuario lo sepa.
            if self.servers.iter().any(|s| s.server.url == *url) {
                return None;
            }
        }
        ready().next().and_then(|(s, m)| m.first().map(|m| (s, m)))
    }

    /// Algo se mueve en pantalla (hay que repintar a ritmo).
    pub fn animating(&self) -> bool {
        self.list.iter().any(Chat::busy)
            || self.toast.is_some()
            || self.servers.iter().any(|s| s.status == Status::Checking)
    }
}

/// Mensaje que empieza por `/nombre` de una habilidad, con ella dentro.
fn expand(text: &str, know: &Know) -> Option<String> {
    let rest = text.strip_prefix('/')?;
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    know.skills().into_iter().find(|s| s.name == name).map(|s| s.invoke(args))
}

/// Última línea de texto de una respuesta (no de su código).
fn last_line(t: &str) -> Option<&str> {
    let mut code = false;
    let mut last = None;
    for l in t.lines().map(str::trim) {
        if l.starts_with("```") {
            code = !code;
        } else if !code && l.chars().any(char::is_alphanumeric) {
            last = Some(l);
        }
    }
    last
}

/// Nombre corto de una carpeta.
pub fn folder_name(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Clave aleatoria para sugerir.
fn random_key() -> String {
    let mut b = [0u8; 12];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b))
        .is_ok();
    if !ok {
        return String::new();
    }
    format!("forge-{}", b.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Órdenes para arrancar el servidor en el PC.
pub fn server_command(which: u8, key: &str) -> String {
    match which {
        0 => {
            format!("llama-server -m modelo.gguf --host 0.0.0.0 --port 8080 -c 32768 -ngl 99 --api-key {key}")
        }
        _ => format!("strata launch --access lan --port 8080 --api-key {key}"),
    }
}

impl App {
    pub(super) fn show_chat(&mut self, on: bool) {
        self.chat_visible = on;
        if on {
            self.reload_mcp();
            self.board_visible = false;
            self.inbox = None;
            self.dashboard = None;
            self.review = None;
            self.close_search();
            // Al arrancar hay conversaciones guardadas, pero ninguna abierta.
            if self.chats.current().is_none_or(|c| !c.loaded) {
                self.new_chat();
            }
            if let Some(c) = self.chats.current_mut() {
                c.unseen = false;
            }
        }
        self.chats.popup = Popup::None;
        self.redraw();
    }

    pub(super) fn new_chat(&mut self) {
        // Si ya hay una en blanco, esa.
        if let Some(i) = self.chats.list.iter().position(Chat::blank) {
            self.chats.cur = i;
            return;
        }
        let id = self.chats.next;
        self.chats.next += 1;
        let root = self.chats.saved.project.clone().filter(|p| p.is_dir());
        self.chats.list.push(Chat { root, ..Chat::empty(id) });
        self.chats.cur = self.chats.list.len() - 1;
        self.start_agent(self.chats.cur);
    }

    /// Pone en marcha el agente de una conversación, desde su historia.
    fn start_agent(&mut self, i: usize) {
        let Some(c) = self.chats.list.get_mut(i).filter(|c| c.agent.is_none()) else { return };
        let (id, proxy) = (c.id, self.proxy.clone());
        c.agent = Some(Agent::spawn(c.history.clone(), move |e| {
            drop(proxy.send_event(UserEvent::Chat(id, e)));
        }));
    }

    /// La lee del disco, si hace falta.
    fn load_chat(&mut self, i: usize) -> Result<(), String> {
        let c = self.chats.list.get_mut(i).ok_or("Esa conversación ya no existe")?;
        if !c.loaded {
            let f = c.file.as_ref().ok_or("Esa conversación no está guardada")?;
            // Si no se entiende, el archivo no se toca.
            let s = store::load(f).map_err(|e| {
                log::warn!("{}: {e}", f.display());
                "No se puede leer esa conversación"
            })?;
            c.restore(s);
            c.loaded = true;
        }
        Ok(())
    }

    /// Abre una conversación, leyéndola del disco si hace falta.
    fn open_chat(&mut self, i: usize) {
        if let Err(e) = self.load_chat(i) {
            return self.toast(e);
        }
        let Some(c) = self.chats.list.get_mut(i) else { return };
        c.unseen = false;
        c.stick = true;
        self.chats.cur = i;
        self.start_agent(i);
    }

    /// Guarda una conversación; si no se puede, se avisa una vez.
    fn save_chat(&mut self, i: usize) {
        let Some(c) = self.chats.list.get_mut(i) else { return };
        match c.save() {
            Ok(()) => self.chats.save_failed = false,
            Err(e) => {
                log::warn!("no se pudo guardar la conversación: {e}");
                if !std::mem::replace(&mut self.chats.save_failed, true) {
                    self.toast("No se puede guardar la conversación");
                }
            }
        }
    }

    /// Al salir: para lo que esté en marcha (con sus comandos) y guarda.
    pub(super) fn close_chats(&mut self) {
        for c in &self.chats.list {
            if let Some(a) = c.agent.as_ref().filter(|a| a.working()) {
                a.stop();
            }
        }
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(1500)
            && self.chats.list.iter().any(|c| c.agent.as_ref().is_some_and(Agent::working))
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        for i in 0..self.chats.list.len() {
            if self.chats.list[i].busy() {
                self.save_chat(i);
            }
        }
    }

    /// Pregunta a los servidores qué tienen, en segundo plano.
    pub(super) fn probe_servers(&mut self) {
        for s in &mut self.chats.servers {
            if !matches!(s.status, Status::Ready(_)) {
                s.status = Status::Checking;
            }
            s.checked = Instant::now();
            let (server, proxy) = (s.server.clone(), self.proxy.clone());
            std::thread::spawn(move || {
                let r = api::probe(&server).map_err(|e| e.to_string());
                let _ = proxy.send_event(UserEvent::Probe(Box::new(server), r));
            });
        }
    }

    pub(super) fn on_probe(&mut self, server: Server, r: Result<Vec<Model>, String>) {
        if let Popup::Form(f) = &mut self.chats.popup
            && f.status == Some(Ok(()))
            && self.chats.servers.iter().all(|s| s.server != server)
        {
            // Resultado del formulario: se guarda solo si conecta.
            match r {
                Err(e) => f.status = Some(Err(e)),
                Ok(models) => {
                    let edit = f.edit.filter(|i| *i < self.chats.servers.len());
                    self.chats.saved.model = models.first().map(|m| (server.url.clone(), m.id.clone()));
                    let view = ServerView { server, status: Status::Ready(models), checked: Instant::now() };
                    match edit {
                        Some(i) => self.chats.servers[i] = view,
                        None => {
                            self.chats.servers.retain(|s| s.server.url != view.server.url);
                            self.chats.servers.push(view);
                        }
                    }
                    self.save_servers();
                    self.chats.popup = Popup::None;
                    self.toast("Conectado");
                }
            }
            return self.redraw();
        }
        if let Some(s) = self.chats.servers.iter_mut().find(|s| s.server == server) {
            s.status = match r {
                Ok(m) => Status::Ready(m),
                Err(e) => Status::Down(e),
            };
        }
        self.redraw();
    }

    fn save_servers(&mut self) {
        self.chats.saved.servers = self.chats.servers.iter().map(|s| s.server.clone()).collect();
        self.chats.saved.save();
    }

    pub(super) fn toast(&mut self, t: impl Into<String>) {
        self.chats.toast = Some((t.into(), Instant::now()));
    }

    /// En cada latido: avisos que caducan y servidores caídos que se reintentan.
    pub(super) fn tick_chat(&mut self) {
        if self.chats.toast.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_millis(1800)) {
            self.chats.toast = None;
        }
        let retry = |s: &ServerView| {
            matches!(s.status, Status::Down(_)) && s.checked.elapsed() > Duration::from_secs(20)
        };
        if self.chat_visible && self.chats.servers.iter().any(retry) {
            self.probe_servers();
        }
    }

    pub(super) fn on_chat_event(&mut self, id: u64, e: Event) {
        let Some(i) = self.chats.list.iter().position(|c| c.id == id) else { return };
        let failed = matches!(e, Event::Error(_));
        let seen = self.chat_visible && self.chats.cur == i;
        let notice = match &e {
            Event::Action { ask: Some(_), verb, target, .. } => {
                Some(format!("Pide permiso: {verb} {target}"))
            }
            Event::Done => Some("Ha terminado".to_string()),
            _ => None,
        };
        let done = matches!(e, Event::Done);
        let save = matches!(e, Event::Done | Event::History(_));
        let c = &mut self.chats.list[i];
        c.on_event(e);
        if seen {
            c.unseen = false;
        } else if c.asking().is_some() {
            c.unseen = true;
        }
        // Terminó por su cuenta y sin errores: toca verificarlo.
        let ended = done
            && c.worktree.is_some()
            && !matches!(c.items.last().map(|i| &i.body), Some(Body::Error(_) | Body::Notice(_)));
        let queued = if done { c.queued.take() } else { None };
        let idle = queued.is_none();
        if let Some(n) = notice
            && queued.is_none()
            && (!self.focused || self.occluded)
        {
            let title = if c.title.is_empty() { "forge".to_string() } else { c.title.clone() };
            crate::notify(&title, &n);
        }
        if save {
            self.save_chat(i);
        }
        if let Some(q) = queued {
            self.send_to(i, q);
        }
        if failed {
            self.probe_servers();
        }
        if ended && idle && self.config.verify.auto {
            self.start_verify(owner::of_chat(id), true);
            self.refresh_board_stats();
        }
        self.redraw();
    }

    /// Envía un mensaje a una conversación.
    fn send_to(&mut self, i: usize, text: String) {
        let Some((server, model)) = self.chats.model() else {
            // Sin modelo no se pierde lo escrito.
            if let Some(c) = self.chats.list.get_mut(i) {
                c.input.set(text);
            }
            if self.chats.servers.is_empty() {
                self.open_form(None);
            } else {
                self.chats.popup = Popup::Models;
                self.probe_servers();
            }
            return;
        };
        let (server, model, ctx) = (server.clone(), model.id.clone(), model.ctx);
        self.start_agent(i);
        let Some(c) = self.chats.list.get_mut(i) else { return };
        c.updated = chrono::Utc::now().timestamp();
        if c.title.is_empty() {
            c.title = crate::agents::one_line(&text, 60);
        }
        let know = c.know();
        // `/habilidad lo que sea`: el modelo recibe la habilidad entera.
        let said = expand(&text, &know).unwrap_or_else(|| text.clone());
        c.items.push(Item::new(Body::User(text)));
        c.phase = Phase::Reading(Instant::now());
        c.settled = false;
        c.stick = true;
        c.sel = None;
        let root = c.root.clone();
        if let Some(a) = &mut c.agent {
            let copy_of = c.worktree.as_ref().map(|w| w.repo.clone());
            let mcp = extend::servers_for(&self.chats.mcp, &self.chats.saved.mcp, know.project.as_deref());
            let know = Some(know);
            a.prompt(said, Setup { server, model, ctx, root: root.clone(), copy_of, know, mcp });
        }
        let (id, copy) = (c.id, c.worktree.is_some());
        if copy {
            // Vuelve a trabajar: lo verificado ya no vale.
            self.stop_verify(owner::of_chat(id));
        } else if let Some(p) = root {
            self.chats.saved.used_project(p);
            self.chats.saved.save();
        }
    }

    /// Resume la conversación actual.
    fn compact_chat(&mut self) {
        let Some((server, model)) = self.chats.model() else { return };
        let (server, model, ctx) = (server.clone(), model.id.clone(), model.ctx);
        let Some(c) = self.chats.current_mut().filter(|c| !c.busy() && !c.items.is_empty()) else { return };
        let root = c.root.clone();
        let copy_of = c.worktree.as_ref().map(|w| w.repo.clone());
        if let Some(a) = &mut c.agent {
            a.compact(Setup { server, model, ctx, root, copy_of, know: None, mcp: vec![] });
            c.phase = Phase::Compacting(Instant::now());
            c.stick = true;
        }
    }

    /// Conversación nueva con una tarea, desde la mesa. En una copia del
    /// proyecto trabaja sin preguntar; si no, pregunta. Devuelve cuál es.
    pub(super) fn launch_chat(&mut self, task: String, root: PathBuf, worktree: Option<Worktree>) -> u64 {
        let id = self.chats.next;
        self.chats.next += 1;
        let copy = worktree.is_some();
        self.chats.list.push(Chat { root: Some(root), worktree, ..Chat::empty(id) });
        let i = self.chats.list.len() - 1;
        self.start_agent(i);
        if let Some(a) = &self.chats.list[i].agent {
            a.set_auto(copy);
        }
        self.send_to(i, task);
        self.save_chat(i);
        id
    }

    /// Le dice algo a una conversación desde la mesa.
    pub(super) fn say_to_chat(&mut self, id: u64, text: String) -> Result<(), String> {
        let i = self.chats.list.iter().position(|c| c.id == id).ok_or("Esa conversación ya no existe")?;
        self.load_chat(i)?;
        if self.chats.list[i].busy() {
            return Err("El modelo está trabajando; espera a que termine".into());
        }
        if self.chats.model().is_none() {
            self.probe_servers();
            return Err("Tu modelo local no responde".into());
        }
        self.chats.list[i].unseen = false;
        self.send_to(i, text);
        Ok(())
    }

    pub(super) fn set_chat_base(&mut self, id: u64, head: &str) {
        let Some(i) = self.chats.list.iter().position(|c| c.id == id) else { return };
        if let Some(w) = &mut self.chats.list[i].worktree {
            w.base = head.to_string();
        }
        self.chats.list[i].settled = true;
        if self.load_chat(i).is_ok() {
            self.save_chat(i);
        }
    }

    /// Se va a borrar una copia: para a quien trabaje en ella y la olvida.
    /// Devuelve las marcas de «sigue trabajando».
    pub(super) fn leave_worktree(&mut self, wt: &Worktree) -> Vec<Arc<AtomicBool>> {
        let mut flags = Vec::new();
        for i in 0..self.chats.list.len() {
            let c = &self.chats.list[i];
            let inside = c.root.as_ref().is_some_and(|r| r.starts_with(&wt.path));
            if !inside && c.worktree.as_ref() != Some(wt) {
                continue;
            }
            if let Some(a) = &c.agent {
                a.stop();
                flags.push(a.working_flag());
            }
            let loaded = self.load_chat(i).is_ok();
            let c = &mut self.chats.list[i];
            c.queued = None;
            (c.worktree, c.root, c.settled) = (None, None, true);
            if c.busy() {
                c.phase = Phase::Stopping;
            }
            c.items.push(Item::new(Body::Notice("Su copia del proyecto se ha borrado.".into())));
            if loaded {
                self.save_chat(i);
            }
        }
        flags
    }

    /// Abre el chat en una conversación (desde la mesa).
    pub(super) fn go_to_chat(&mut self, id: u64) {
        let Some(i) = self.chats.list.iter().position(|c| c.id == id) else { return };
        self.open_chat(i);
        self.show_chat(true);
    }

    /// Contesta a un permiso desde la mesa.
    pub(super) fn answer_chat(&mut self, id: u64, tool: u64, yes: bool) {
        let Some(i) = self.chats.list.iter().position(|c| c.id == id) else { return };
        self.answer_in(i, tool, if yes { Decision::Allow } else { Decision::Deny(String::new()) });
    }

    /// Conversaciones para la mesa: las abiertas y las de hoy.
    pub(super) fn chat_cards(&self) -> Vec<ChatCard> {
        let order = self.chats.recent();
        let today = chrono::Local::now().date_naive().and_time(chrono::NaiveTime::MIN);
        let today = today.and_local_timezone(chrono::Local).earliest().map_or(0, |d| d.timestamp());
        order
            .into_iter()
            .map(|i| &self.chats.list[i])
            .filter(|c| (c.agent.is_some() || c.updated >= today) && (!c.settled || c.busy()))
            .take(12)
            .map(|c| {
                let ask = c.asking();
                let last = c.items.iter().rev().find_map(|i| match &i.body {
                    Body::Assistant(t) => last_line(t),
                    _ => None,
                });
                let detail = match (ask, &c.phase) {
                    (Some(t), _) => match &t.detail {
                        Detail::Diff { added, removed, .. } => {
                            format!("{} {}  +{added} −{removed}", t.verb, t.target)
                        }
                        _ => format!("{} {}", t.verb, t.target),
                    },
                    (_, Phase::Idle) => "Te toca".into(),
                    (_, Phase::Reading(_)) => "Leyendo la conversación".into(),
                    (_, Phase::Compacting(_)) => "Resumiendo la conversación".into(),
                    (_, Phase::Starting(n, _)) => format!("Arrancando {n}"),
                    (_, Phase::Stopping) => "Parando…".into(),
                    _ => match c.items.last().map(|i| &i.body) {
                        Some(Body::Think { .. }) => "Pensando".into(),
                        Some(Body::Tool(t)) if !matches!(t.state, ToolState::Done { .. }) => {
                            format!("{} {}", t.verb, t.target)
                        }
                        _ => "Escribiendo".into(),
                    },
                };
                ChatCard {
                    id: c.id,
                    title: c.title.clone(),
                    project: c.project(),
                    branch: c.worktree.as_ref().map(|w| w.name().to_string()),
                    stats: None,
                    verify: None,
                    can_verify: false,
                    detail,
                    last: last.unwrap_or_default().trim_start_matches(['#', '-', '*', '>', ' ']).to_string(),
                    ask: ask.map(|t| t.id),
                    warn: ask.and_then(|t| match &t.state {
                        ToolState::Asking(a) if a.reason.is_some() => a.reason.clone(),
                        // Un comando no se aprueba sin verlo entero.
                        _ if matches!(t.detail, Detail::Command { .. }) => {
                            Some("mira el comando antes de permitirlo".into())
                        }
                        _ => None,
                    }),
                    busy: c.busy(),
                    unseen: c.unseen,
                    used: c.stats.map(|s| s.prompt + s.output),
                    since: c.updated,
                }
            })
            .collect()
    }

    /// ↵: envía, contesta a un permiso con un motivo, o deja en cola.
    fn chat_submit(&mut self) {
        let i = self.chats.cur;
        let Some(c) = self.chats.list.get_mut(i) else { return };
        let text = c.input.text().trim().to_string();
        if text.is_empty() {
            return;
        }
        c.input.take();
        if let Some(id) = c.asking().map(|t| t.id) {
            return self.chat_answer(id, Decision::Deny(text));
        }
        if c.busy() {
            c.queued = Some(match c.queued.take() {
                Some(q) => format!("{q}\n{text}"),
                None => text,
            });
            return;
        }
        self.send_to(i, text);
    }

    fn chat_answer(&mut self, id: u64, d: Decision) {
        self.answer_in(self.chats.cur, id, d);
    }

    fn answer_in(&mut self, chat: usize, id: u64, d: Decision) {
        let Some(c) = self.chats.list.get_mut(chat) else { return };
        // Solo si sigue pendiente (un doble clic no contesta a lo siguiente).
        if c.asking().is_none_or(|t| t.id != id) {
            return;
        }
        if let Some(t) = c.tool(id) {
            t.state = ToolState::Queued;
        }
        if let Some(a) = &c.agent {
            a.answer(id, d);
        }
    }

    fn chat_stop(&mut self) {
        let Some(c) = self.chats.current_mut() else { return };
        if c.busy() {
            c.phase = Phase::Stopping;
            c.queued = None;
            if let Some(a) = &c.agent {
                a.stop();
            }
        }
    }

    fn open_form(&mut self, edit: Option<usize>) {
        let mut f = Form { edit, ..Form::default() };
        match edit.and_then(|i| self.chats.servers.get(i)) {
            Some(s) => {
                f.url.set(s.server.url.trim_start_matches("http://"));
                f.key.set(s.server.key.clone().unwrap_or_default());
            }
            None => f.suggested = random_key(),
        }
        self.chats.popup = Popup::Form(Box::new(f));
    }

    fn form_submit(&mut self) {
        let Popup::Form(f) = &mut self.chats.popup else { return };
        if f.status == Some(Ok(())) {
            return;
        }
        let base = match Base::parse(f.url.text()) {
            Ok(b) => b,
            Err(e) => {
                f.status = Some(Err(e));
                return;
            }
        };
        let key = Some(f.key.text().trim().to_string()).filter(|k| !k.is_empty());
        let server = Server { name: base.host.clone(), url: base.url(), key };
        // El mismo de antes, sin cambios: nada que guardar.
        if self.chats.servers.iter().any(|s| s.server == server) {
            self.chats.popup = Popup::None;
            self.probe_servers();
            return;
        }
        f.status = Some(Ok(()));
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let r = api::probe(&server).map_err(|e| e.to_string());
            let _ = proxy.send_event(UserEvent::Probe(Box::new(server), r));
        });
    }

    /// Selector de carpetas de macOS.
    fn browse_folder(&mut self) {
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let out = std::process::Command::new("/usr/bin/osascript")
                .args(["-e", "POSIX path of (choose folder with prompt \"¿En qué carpeta trabajamos?\")"])
                .output();
            if let Ok(o) = out
                && o.status.success()
            {
                let p = String::from_utf8_lossy(&o.stdout).trim().trim_end_matches('/').to_string();
                let _ = proxy.send_event(UserEvent::Folder(p.into()));
            }
        });
    }

    /// Carpetas para elegir: recientes y las de las terminales abiertas.
    pub(super) fn chat_projects(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.chats.saved.recents.iter().filter(|p| p.is_dir()).cloned().collect();
        for p in self.board_projects() {
            if !v.contains(&p) && tools::Ctx::new(&p, 1).is_some() {
                v.push(p);
            }
        }
        v.truncate(9);
        v
    }

    pub(super) fn set_chat_project(&mut self, p: Option<PathBuf>) {
        self.chats.popup = Popup::None;
        if let Some(p) = &p {
            if tools::Ctx::new(p, 1).is_none() {
                self.toast("Elige una carpeta de proyecto, no la carpeta personal entera");
                return self.redraw();
            }
            self.chats.saved.used_project(p.clone());
        } else {
            self.chats.saved.project = None;
        }
        self.chats.saved.save();
        // Una conversación empezada se queda en su carpeta.
        if self.chats.current().is_some_and(|c| !c.blank() && c.root != p) {
            let draft = self.chats.current_mut().map(|c| c.input.take()).unwrap_or_default();
            self.new_chat();
            if let Some(c) = self.chats.current_mut()
                && !draft.is_empty()
            {
                c.input.set(draft);
            }
        }
        if let Some(c) = self.chats.current_mut() {
            c.root = p;
        }
        self.redraw();
    }

    fn set_clipboard(&mut self, t: String, toast: &str) {
        if let Some(c) = self.clipboard.as_mut()
            && c.set_text(t).is_ok()
        {
            self.toast(toast);
        }
    }

    pub(super) fn chat_click(&mut self, hit: ChatHit) {
        // Un clic en otro sitio cancela la confirmación de borrado.
        let confirm = self.chats.confirm.take();
        match hit {
            ChatHit::New => {
                self.chats.popup = Popup::None;
                self.new_chat();
            }
            ChatHit::Open(id) => {
                self.chats.popup = Popup::None;
                if let Some(i) = self.chats.list.iter().position(|c| c.id == id) {
                    self.open_chat(i);
                }
            }
            ChatHit::Delete(id) => {
                if confirm != Some(id) {
                    self.chats.confirm = Some(id);
                } else if let Some(i) = self.chats.list.iter().position(|c| c.id == id) {
                    let gone = self.chats.list.remove(i);
                    if let Some(w) = &gone.worktree {
                        // El trabajo no se borra con la conversación.
                        self.toast(format!("Su copia sigue en {}", w.name()));
                    }
                    if let Some(f) = &gone.file
                        && let Err(e) = std::fs::remove_file(f)
                        && e.kind() != std::io::ErrorKind::NotFound
                    {
                        log::warn!("no se pudo borrar {}: {e}", f.display());
                        self.toast("No se ha podido borrar del disco");
                    }
                    match self.chats.cur.cmp(&i) {
                        std::cmp::Ordering::Equal => self.new_chat(),
                        std::cmp::Ordering::Greater => self.chats.cur -= 1,
                        std::cmp::Ordering::Less => {}
                    }
                }
            }
            ChatHit::Send => self.chat_submit(),
            ChatHit::Stop => self.chat_stop(),
            ChatHit::Mode => {
                if let Some(a) = self.chats.current().and_then(|c| c.agent.as_ref()) {
                    a.set_auto(!a.auto());
                }
            }
            ChatHit::ModelMenu => {
                self.chats.popup =
                    if matches!(self.chats.popup, Popup::Models) { Popup::None } else { Popup::Models };
                self.probe_servers();
            }
            ChatHit::ProjectMenu => {
                self.chats.popup =
                    if matches!(self.chats.popup, Popup::Projects) { Popup::None } else { Popup::Projects };
            }
            ChatHit::PickModel(s, m) => {
                if let Some(v) = self.chats.servers.get(s as usize)
                    && let Status::Ready(models) = &v.status
                    && let Some(m) = models.get(m as usize)
                {
                    self.chats.saved.model = Some((v.server.url.clone(), m.id.clone()));
                    self.chats.saved.save();
                }
                self.chats.popup = Popup::None;
            }
            ChatHit::PickClaude => {
                // Claude trabaja en su propio programa, en la misma carpeta.
                self.chats.popup = Popup::None;
                let cwd = self.chats.current().and_then(|c| c.root.clone());
                self.show_chat(false);
                self.open_tab_ext(PaneKind::Ai(Provider::Claude), Spawn { cwd, ..Spawn::default() });
            }
            ChatHit::AddServer => self.open_form(None),
            ChatHit::EditServer(i) => self.open_form(Some(i as usize)),
            ChatHit::RemoveServer(_) => {
                if let Popup::Form(f) = &self.chats.popup
                    && let Some(i) = f.edit.filter(|i| *i < self.chats.servers.len())
                {
                    self.chats.servers.remove(i);
                    self.save_servers();
                }
                self.chats.popup = Popup::None;
            }
            ChatHit::PickProject(i) => {
                let p = self.chat_projects().into_iter().nth(i as usize);
                if p.is_some() {
                    self.set_chat_project(p);
                }
            }
            ChatHit::PickNoProject => self.set_chat_project(None),
            ChatHit::BrowseProject => {
                self.chats.popup = Popup::None;
                self.browse_folder();
            }
            ChatHit::Allow(id) => self.chat_answer(id, Decision::Allow),
            ChatHit::AllowTool(id) => {
                let tool = self.chats.current().and_then(|c| c.asking()).and_then(|t| match &t.state {
                    ToolState::Asking(a) if t.id == id => a.tool.clone(),
                    _ => None,
                });
                if let Some((server, tool)) = tool {
                    self.trust_tool(&server, &tool);
                    self.chat_answer(id, Decision::AllowTool);
                }
            }
            ChatHit::AllowAll(id) => self.chat_answer(id, Decision::AllowAll),
            ChatHit::Deny(id) => self.chat_answer(id, Decision::Deny(String::new())),
            ChatHit::Toggle(i) => {
                if let Some(it) = self.chats.current_mut().and_then(|c| c.items.get_mut(i as usize)) {
                    it.open = !it.open;
                    it.rev += 1;
                }
            }
            ChatHit::CopyCode(i, j) => {
                let text = self.chats.current().and_then(|c| {
                    let cache = c.items.get(i as usize)?.cache.as_ref()?;
                    cache.texts().find(|(k, _)| *k == j as usize).map(|(_, t)| t)
                });
                if let Some(t) = text {
                    self.set_clipboard(t, "Código copiado");
                }
            }
            ChatHit::CopyAnswer(i) => {
                if let Some(Body::Assistant(t)) =
                    self.chats.current().and_then(|c| c.items.get(i as usize)).map(|i| &i.body)
                {
                    let t = t.trim().to_string();
                    self.set_clipboard(t, "Respuesta copiada");
                }
            }
            ChatHit::Compact => self.compact_chat(),
            ChatHit::Review => self.toggle_review(),
            ChatHit::Memory => self.open_memory(),
            ChatHit::ForgetNote(project, i) => self.forget_note(project, i as usize),
            ChatHit::MemoryFile(project) => {
                let scope = if project { Scope::Project } else { Scope::User };
                if let Some(f) = self.chats.current().and_then(|c| c.know().file(scope)) {
                    self.reveal(f, true);
                }
            }
            ChatHit::SkillsFolder => self.reveal(store::skills_dir(), false),
            ChatHit::PickSkill(i) => {
                self.slash_pick(i as usize);
            }
            ChatHit::CopyCommand(w) => {
                if let Popup::Form(f) = &mut self.chats.popup {
                    // La clave sugerida se usa también para conectar.
                    if f.key.is_empty() {
                        f.key.set(f.suggested.clone());
                    }
                    let cmd = server_command(w, f.key.text());
                    self.set_clipboard(cmd, "Orden copiada: pégala en el PC");
                }
            }
            ChatHit::Suggest(i) => {
                if let Some(c) = self.chats.current_mut() {
                    c.input.set(draw::SUGGESTIONS[(i as usize).min(draw::SUGGESTIONS.len() - 1)].1);
                }
            }
            ChatHit::Bottom => {
                if let Some(c) = self.chats.current_mut() {
                    c.stick = true;
                }
            }
            ChatHit::Field(n) => {
                if let Popup::Form(f) = &mut self.chats.popup {
                    f.focus = n;
                }
            }
            ChatHit::FormOk => self.form_submit(),
            ChatHit::FormCancel => self.chats.popup = Popup::None,
            ChatHit::Inside => {}
            h => self.extend_click(h),
        }
        self.redraw();
    }

    /// Texto tecleado o compuesto (acentos).
    pub(super) fn chat_text(&mut self, t: &str) {
        match &mut self.chats.popup {
            Popup::Form(f) => {
                let ed = if f.focus == 0 { &mut f.url } else { &mut f.key };
                ed.insert(&t.replace(['\n', '\r', '\t'], ""));
                if f.status != Some(Ok(())) {
                    f.status = None;
                }
            }
            Popup::McpForm(_) => self.mcp_text(t),
            Popup::McpSearch(_) => self.search_text(t),
            Popup::Skill(f) => {
                if f.bundle.is_none() && !f.busy {
                    f.url.insert(&t.replace(['\n', '\r', '\t'], ""));
                    f.error = None;
                }
            }
            _ => {
                self.chats.popup = Popup::None;
                if let Some(c) = self.chats.current_mut() {
                    c.input.insert(t);
                }
                self.slash_changed();
            }
        }
        self.redraw();
    }

    /// Se ha escrito algo: el menú «/» vuelve a empezar.
    fn slash_changed(&mut self) {
        self.chats.slash_sel = 0;
        self.chats.slash_off = false;
        // Al escribir «/» se buscan las habilidades que haya ahora.
        if let Some(c) = self.chats.current()
            && c.input.text() == "/"
        {
            self.chats.skills = c.know().skills();
        }
    }

    /// Completa con la habilidad `i` del menú. `false`: no hay menú, o ya
    /// está escrita entera.
    fn slash_pick(&mut self, i: usize) -> bool {
        let Some(name) = self.chats.slash().get(i).map(|s| s.name.clone()) else { return false };
        let Some(c) = self.chats.current_mut() else { return false };
        if c.input.text() == format!("/{name}") {
            return false;
        }
        c.input.set(format!("/{name} "));
        true
    }

    pub(super) fn open_memory(&mut self) {
        let Some(c) = self.chats.current() else { return };
        let k = c.know();
        self.chats.popup = Popup::Memory(Box::new(MemoryView {
            user: k.notes(Scope::User),
            project: k.notes(Scope::Project),
            name: k.project.as_deref().map(folder_name),
            skills: k.skills(),
        }));
    }

    fn forget_note(&mut self, project: bool, i: usize) {
        let scope = if project { Scope::Project } else { Scope::User };
        let Popup::Memory(v) = &self.chats.popup else { return };
        let list = if project { &v.project } else { &v.user };
        let (Some(text), Some(c)) = (list.get(i).cloned(), self.chats.current()) else { return };
        match c.know().forget(scope, &text) {
            Ok(()) => self.toast("Olvidado"),
            Err(e) => self.toast(format!("No se ha podido olvidar: {e}")),
        }
        // Las conversaciones abiertas lo tendrán en cuenta.
        for a in self.chats.list.iter().filter_map(|c| c.agent.as_ref()) {
            a.refresh();
        }
        self.open_memory();
    }

    /// Abre un archivo o carpeta de forge con su programa, creándolo antes.
    fn reveal(&mut self, path: PathBuf, text: bool) {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let dir = if text { path.parent().map(Path::to_path_buf) } else { Some(path.clone()) };
        if let Some(d) = dir {
            let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(d);
        }
        if text {
            let _ = std::fs::OpenOptions::new().append(true).create(true).mode(0o600).open(&path);
        }
        let mut c = std::process::Command::new("/usr/bin/open");
        if text {
            c.arg("-t");
        }
        if c.arg(&path).spawn().is_err() {
            self.toast("No se ha podido abrir");
        }
        self.chats.popup = Popup::None;
        for a in self.chats.list.iter().filter_map(|c| c.agent.as_ref()) {
            a.refresh();
        }
    }

    pub(super) fn chat_paste(&mut self) {
        if let Some(t) = self.clipboard.as_mut().and_then(|c| c.get_text().ok()) {
            let t = t.replace("\r\n", "\n").replace('\r', "\n");
            let t: String = t.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect();
            self.chat_text(&t);
        }
    }

    /// Teclas con el chat a la vista. Devuelve si la consumió.
    pub(super) fn chat_key(&mut self, event: &KeyEvent) -> bool {
        let (cmd, alt, shift) = (self.mods.super_key(), self.mods.alt_key(), self.mods.shift_key());
        let ctrl = self.mods.control_key();
        let form = matches!(self.chats.popup, Popup::Form(_));
        if cmd
            && !ctrl
            && !alt
            && let Key::Character(ch) = &event.logical_key
        {
            match ch.to_lowercase().as_str() {
                "v" => self.chat_paste(),
                "a" => {
                    if let Some(e) = self.chat_editor() {
                        e.select_all();
                    }
                }
                "z" => {
                    if let Some(e) = self.chat_editor() {
                        e.undo();
                    }
                }
                k @ ("c" | "x") => {
                    let in_box = self.chat_editor().and_then(|e| e.selected().map(str::to_string));
                    match in_box {
                        Some(t) => {
                            if k == "x"
                                && let Some(e) = self.chat_editor()
                            {
                                e.delete(Move::Left);
                            }
                            self.set_clipboard(t, "Copiado");
                        }
                        None => {
                            if k == "c"
                                && let Some(t) = self.chats.current().and_then(Chat::selected_text)
                            {
                                self.set_clipboard(t, "Copiado");
                            }
                        }
                    }
                }
                _ => return false,
            }
            self.redraw();
            return true;
        }
        let Key::Named(named) = &event.logical_key else {
            if cmd || ctrl {
                return false;
            }
            if let Some(t) = event.text.as_deref().filter(|t| !t.chars().any(char::is_control)) {
                self.chat_text(t);
            }
            return true;
        };
        let step = |left: bool| match (cmd, alt, left) {
            (true, _, true) => Move::LineStart,
            (true, _, false) => Move::LineEnd,
            (_, true, true) => Move::WordLeft,
            (_, true, false) => Move::WordRight,
            (_, _, true) => Move::Left,
            (_, _, false) => Move::Right,
        };
        match named {
            NamedKey::Enter | NamedKey::Tab
                if !cmd && !shift && !alt && self.slash_pick(self.chats.slash_sel) => {}
            NamedKey::ArrowUp | NamedKey::ArrowDown if !cmd && !self.chats.slash().is_empty() => {
                let n = self.chats.slash().len();
                let d = if *named == NamedKey::ArrowUp { n - 1 } else { 1 };
                self.chats.slash_sel = (self.chats.slash_sel + d) % n;
            }
            NamedKey::Escape if !self.chats.slash().is_empty() => self.chats.slash_off = true,
            NamedKey::Enter if !cmd && self.extend_enter() => {}
            NamedKey::Escape if self.extend_back() => {}
            NamedKey::Enter if form => self.form_submit(),
            NamedKey::Enter if cmd => match self.chats.current().and_then(|c| c.asking()).map(|t| t.id) {
                Some(id) => self.chat_answer(id, Decision::Allow),
                None => self.chat_submit(),
            },
            NamedKey::Enter if shift || alt => self.chat_text("\n"),
            NamedKey::Enter => self.chat_submit(),
            NamedKey::Escape => {
                let asking = self.chats.current().and_then(|c| c.asking()).map(|t| t.id);
                if !matches!(self.chats.popup, Popup::None) {
                    self.chats.popup = Popup::None;
                } else if let Some(id) = asking {
                    self.chat_answer(id, Decision::Deny(String::new()));
                } else if self.chats.current().is_some_and(|c| c.sel.is_some()) {
                    if let Some(c) = self.chats.current_mut() {
                        c.sel = None;
                    }
                } else {
                    self.chat_stop();
                }
            }
            NamedKey::Tab if matches!(self.chats.popup, Popup::McpSearch(_)) => {
                if let Popup::McpSearch(f) = &mut self.chats.popup {
                    let n = f.values.len().max(1);
                    f.focus = (f.focus + if shift { n - 1 } else { 1 }) % n;
                }
            }
            NamedKey::Tab if matches!(self.chats.popup, Popup::McpForm(_)) => {
                if let Popup::McpForm(f) = &mut self.chats.popup {
                    f.on_key = !f.on_key && f.wants_key();
                }
            }
            NamedKey::Tab if form => {
                if let Popup::Form(f) = &mut self.chats.popup {
                    f.focus = 1 - f.focus.min(1);
                }
            }
            // ⇧⇥ cambia el modo, como en otros agentes.
            NamedKey::Tab if shift => self.chat_click(ChatHit::Mode),
            NamedKey::Tab => {}
            NamedKey::Space if !cmd && !ctrl => self.chat_text(" "),
            NamedKey::Backspace | NamedKey::Delete => {
                let m = step(*named == NamedKey::Backspace);
                if let Some(e) = self.chat_editor() {
                    e.delete(m);
                }
                self.slash_changed();
                self.mcp_parse();
            }
            NamedKey::ArrowLeft | NamedKey::ArrowRight => {
                let m = step(*named == NamedKey::ArrowLeft);
                if let Some(e) = self.chat_editor() {
                    e.go(m, shift);
                }
            }
            NamedKey::ArrowUp | NamedKey::ArrowDown => {
                self.chat_vertical(*named == NamedKey::ArrowUp, cmd, shift)
            }
            NamedKey::Home | NamedKey::End => self.chat_scroll_to(*named == NamedKey::Home),
            NamedKey::PageUp | NamedKey::PageDown => {
                let page = self.chats.current().map_or(0.0, |c| c.view_h * 0.85);
                self.chat_wheel(if *named == NamedKey::PageUp { page } else { -page });
            }
            _ => return false,
        }
        self.redraw();
        true
    }

    /// El texto que se edita ahora: un campo del formulario o la caja.
    fn chat_editor(&mut self) -> Option<&mut Editor> {
        match &mut self.chats.popup {
            Popup::Form(f) => Some(if f.focus == 0 { &mut f.url } else { &mut f.key }),
            Popup::McpForm(f) => Some(if f.on_key { &mut f.key } else { &mut f.text }),
            Popup::McpSearch(f) => f.editor(),
            Popup::Skill(f) => Some(&mut f.url),
            _ => self.chats.list.get_mut(self.chats.cur).map(|c| &mut c.input),
        }
    }

    fn chat_vertical(&mut self, up: bool, cmd: bool, shift: bool) {
        let to_end = if up { Move::Start } else { Move::End };
        if cmd
            || matches!(
                self.chats.popup,
                Popup::Form(_) | Popup::McpForm(_) | Popup::McpSearch(_) | Popup::Skill(_)
            )
        {
            if let Some(e) = self.chat_editor() {
                e.go(to_end, shift);
            }
            return;
        }
        let Some(c) = self.chats.list.get_mut(self.chats.cur) else { return };
        // Con la caja vacía, las flechas mueven la conversación.
        if c.input.is_empty() {
            let d = 60.0 * self.gfx.as_ref().map_or(1.0, |g| g.scale());
            return self.chat_wheel(if up { d } else { -d });
        }
        let (Some(g), Some(at)) = (&self.gfx, &c.input_at) else { return };
        let Some((x, y)) = g.rich_caret(at.key, c.input.pos(c.input.caret())) else { return };
        let next = y + at.line * if up { -0.5 } else { 1.5 };
        let target = if next < 0.0 || next > at.area.h {
            None
        } else {
            g.rich_hit(at.key, x + 1.0, next).map(|p| c.input.byte(p))
        };
        match target {
            Some(b) => c.input.place(b, shift),
            None => c.input.go(to_end, shift),
        }
    }

    fn chat_scroll_to(&mut self, top: bool) {
        if let Some(c) = self.chats.current_mut() {
            c.stick = !top;
            if top {
                c.scroll = 0.0;
            }
        }
        self.redraw();
    }

    /// Rueda del ratón: la lista o la conversación.
    pub(super) fn chat_wheel_at(&mut self, (x, y): (f32, f32), px: f32) {
        if self.extend_wheel(px) {
            return;
        }
        let c = &mut self.chats;
        if c.side_area.contains(x, y) {
            c.side_scroll = (c.side_scroll - px).clamp(0.0, c.side_max);
            return self.redraw();
        }
        self.chat_wheel(px);
    }

    /// Desplaza la conversación (positivo: hacia arriba).
    pub(super) fn chat_wheel(&mut self, px: f32) {
        let Some(c) = self.chats.current_mut() else { return };
        let max = (c.content_h - c.view_h).max(0.0);
        c.scroll = (c.scroll - px).clamp(0.0, max);
        c.stick = c.scroll >= max - 2.0;
        self.redraw();
    }

    /// Posición del texto bajo el ratón (o la más cercana).
    fn chat_doc_pos(&self, x: f32, y: f32, exact: bool) -> Option<DocPos> {
        let (g, c) = (self.gfx.as_ref()?, self.chats.current()?);
        let d = |a: &Area| {
            let dy = (a.y - y).max(y - a.bottom()).max(0.0);
            let dx = (a.x - x).max(x - a.right()).max(0.0);
            dy * 4.0 + dx
        };
        let p = c.placed.iter().min_by(|a, b| d(&a.area).total_cmp(&d(&b.area)))?;
        if exact && d(&p.area) > 0.0 {
            return None;
        }
        Some((p.item, p.piece, g.rich_hit(p.key, x - p.area.x, y - p.area.y)?))
    }

    /// Botón izquierdo en el chat (los clics en botones ya se atendieron).
    /// Devuelve qué se arrastra: la caja (`true`) o la conversación.
    pub(super) fn chat_press(&mut self, x: f32, y: f32, double: bool) -> Option<bool> {
        self.chats.confirm = None;
        if !matches!(self.chats.popup, Popup::None) {
            self.chats.popup = Popup::None;
            self.redraw();
            return None;
        }
        let shift = self.mods.shift_key();
        let c = self.chats.list.get_mut(self.chats.cur)?;
        if let (Some(at), Some(g)) = (&c.input_at, &self.gfx)
            && at.area.inset(-8.0 * g.scale()).contains(x, y)
        {
            c.sel = None;
            if let Some(p) = g.rich_hit(at.key, x - at.area.x, y - at.area.y) {
                let b = c.input.byte(p);
                c.input.place(b, shift);
                if double {
                    c.input.go(Move::WordLeft, false);
                    c.input.go(Move::WordRight, true);
                }
            }
            self.redraw();
            return Some(true);
        }
        let pos = self.chat_doc_pos(x, y, true);
        if let Some(c) = self.chats.current_mut() {
            c.sel = pos.map(|p| (p, p));
        }
        self.redraw();
        pos.map(|_| false)
    }

    pub(super) fn chat_drag(&mut self, x: f32, y: f32, input: bool) {
        if input {
            let Some(c) = self.chats.list.get_mut(self.chats.cur) else { return };
            if let (Some(at), Some(g)) = (&c.input_at, &self.gfx)
                && let Some(p) = g.rich_hit(at.key, x - at.area.x, y - at.area.y)
            {
                let b = c.input.byte(p);
                c.input.place(b, true);
            }
        } else if let Some(p) = self.chat_doc_pos(x, y, false)
            && let Some(c) = self.chats.current_mut()
            && let Some(s) = &mut c.sel
        {
            s.1 = p;
        }
        self.redraw();
    }

    /// Fin del arrastre: un clic sin mover no deja selección.
    pub(super) fn chat_drag_end(&mut self) {
        if let Some(c) = self.chats.current_mut()
            && c.sel.is_some_and(|(a, b)| a == b)
        {
            c.sel = None;
        }
        self.redraw();
    }

    /// ¿Hay texto seleccionable bajo el ratón?
    pub(super) fn chat_text_at(&self, x: f32, y: f32) -> bool {
        self.chats
            .current()
            .is_some_and(|c| c.placed.iter().chain(&c.input_at).any(|p| p.area.contains(x, y)))
    }

    /// Escribe y envía (solo para pruebas).
    pub(super) fn chat_say(&mut self, t: String) {
        match t.as_str() {
            "!allow" => {
                if let Some(id) = self.chats.current().and_then(|c| c.asking()).map(|t| t.id) {
                    self.chat_answer(id, Decision::Allow);
                }
            }
            // Clic en un punto: `!click x y` (píxeles de la captura).
            t if t.starts_with("!click ") => {
                let n: Vec<f32> = t[7..].split(' ').filter_map(|v| v.parse().ok()).collect();
                if let [x, y] = n[..] {
                    match crate::ui::hit_at(&self.hits, x, y).and_then(Self::chat_hit) {
                        Some(h) => self.chat_click(h),
                        None => drop(self.chat_press(x, y, false)),
                    }
                }
            }
            t if t.starts_with("!drag ") => {
                let n: Vec<f32> = t[6..].split(' ').filter_map(|v| v.parse().ok()).collect();
                if let [x, y, x2, y2] = n[..]
                    && let Some(input) = self.chat_press(x, y, false)
                {
                    self.chat_drag(x2, y2, input);
                    self.chat_drag_end();
                    eprintln!("selección: {:?}", self.chats.current().and_then(Chat::selected_text));
                }
            }
            t if t.starts_with("!open ") => {
                let n: usize = t[6..].parse().unwrap_or(0);
                if let Some(&i) = self.chats.recent().get(n) {
                    self.open_chat(i);
                }
            }
            t if t.starts_with("!delete ") => {
                let n: usize = t[8..].parse().unwrap_or(0);
                if let Some(id) = self.chats.recent().get(n).map(|i| self.chats.list[*i].id) {
                    self.chat_click(ChatHit::Delete(id));
                }
            }
            t if t.starts_with("!task ") => {
                self.board.input = t[6..].to_string();
                self.board_launch();
            }
            t @ ("!verify" | "!review" | "!fix") => {
                if let Some(o) = self.chats.recent().first().map(|i| owner::of_chat(self.chats.list[*i].id)) {
                    match t {
                        "!verify" => self.start_verify(o, false),
                        "!fix" => self.board_fix(o),
                        _ => self.open_review(o),
                    }
                }
            }
            "!memory" => self.open_memory(),
            "!mcp" => self.chat_click(ChatHit::Mcp),
            "!mcpadd" => self.chat_click(ChatHit::McpAdd),
            "!mcpok" => self.chat_click(ChatHit::McpAddOk),
            "!key" => self.chat_click(ChatHit::McpKey(0)),
            "!search" => self.chat_click(ChatHit::McpSearch),
            "!go" => self.chat_click(ChatHit::McpSearchGo),
            t if t.starts_with("!found ") => {
                self.chat_click(ChatHit::McpFound(t[7..].parse().unwrap_or(0)));
            }
            t if t.starts_with("!choice ") => {
                self.chat_click(ChatHit::McpChoice(t[8..].parse().unwrap_or(0)));
            }
            "!review2" => self.chat_click(ChatHit::McpReview),
            "!signin" => self.chat_click(ChatHit::McpSignIn(0)),
            "!mcpopen" => self.chat_click(ChatHit::McpOpen(0)),
            "!skill" => self.chat_click(ChatHit::SkillAdd),
            "!fetch" => self.chat_click(ChatHit::SkillFetch),
            t if t.starts_with("!hit ") => {
                self.chat_click(ChatHit::SkillHit(t[5..].parse().unwrap_or(0)));
            }
            "!back" => self.chat_click(ChatHit::ExtendBack),
            "!skillpick" => self.chat_click(ChatHit::SkillPick(0)),
            "!install" => self.chat_click(ChatHit::SkillInstall),
            "!allowtool" => {
                if let Some(id) = self.chats.current().and_then(|c| c.asking()).map(|t| t.id) {
                    self.chat_click(ChatHit::AllowTool(id));
                }
            }
            t if t.starts_with("!keys ") => t[6..].chars().for_each(|c| self.chat_text(&c.to_string())),
            "!pick" => {
                self.slash_pick(0);
            }
            "!merge" => self.review_merge(),
            "!compact" => self.compact_chat(),
            "!board" => self.show_board(true),
            "!side" => self.chats.side_scroll = 520.0,
            "!models" => self.chats.popup = Popup::Models,
            "!projects" => self.chats.popup = Popup::Projects,
            "!form" => self.open_form(None),
            t if t.starts_with("!type ") => {
                if let Some(c) = self.chats.current_mut() {
                    c.input.set(&t[6..]);
                }
            }
            _ => {
                if let Some(c) = self.chats.current_mut() {
                    c.input.set(t);
                }
                self.chat_submit();
            }
        }
        self.redraw();
    }

    /// `FORGE_CAPTURE_CHAT="3000:hola\n9000:!allow"`: en modo captura, hace eso
    /// en esos milisegundos. Y `FORGE_CAPTURE_ROOT`, la carpeta.
    pub(super) fn capture_chat(&mut self) {
        if crate::capture::target().is_none() {
            return;
        }
        if let (Some(c), Some(r)) = (self.chats.current_mut(), std::env::var_os("FORGE_CAPTURE_ROOT")) {
            c.root = Some(r.into());
        }
        let Ok(script) = std::env::var("FORGE_CAPTURE_CHAT") else { return };
        for l in script.lines() {
            let Some((ms, t)) = l.split_once(':') else { continue };
            let (ms, t, proxy) = (ms.parse().unwrap_or(0), t.to_string(), self.proxy.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(ms));
                let _ = proxy.send_event(UserEvent::ChatSay(t));
            });
        }
    }

    pub(super) fn chat_hit(hit: Hit) -> Option<ChatHit> {
        match hit {
            Hit::Chat(h) => Some(h),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_expands_skills() {
        let know =
            Know { memory: "/no".into(), skills: "/no".into(), project: None, root: None, writable: true };
        let t = expand("/revisar solo el login", &know).unwrap();
        assert!(t.contains("git diff") && t.contains("solo el login"));
        assert!(expand("/usr/bin es una ruta", &know).is_none());
        assert!(expand("revisar", &know).is_none());
    }

    #[test]
    fn last_line_skips_code() {
        assert_eq!(last_line("Hecho:\n```py\nx = 1\n```\n"), Some("Hecho:"));
        assert_eq!(last_line("uno\n\n- dos\n---"), Some("- dos"));
        assert_eq!(last_line("```\nsolo código"), None);
    }
}

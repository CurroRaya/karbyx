// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! El chat: conversaciones con modelos locales que trabajan en un proyecto.
//! Aquí está el estado y lo que hace cada tecla y cada clic; `draw` lo pinta.

pub mod draw;
pub mod edit;
pub mod md;
pub mod store;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use forge_agent::agent::{Agent, Ask, Decision, Event, Setup};
use forge_agent::api::{self, Model, Server, Stats};
use forge_agent::http::Base;
use forge_agent::tools::{self, Detail};
use winit::event::KeyEvent;
use winit::keyboard::{Key, NamedKey};

use crate::gfx::TextPos;
use crate::ui::{Area, Hit};
use crate::{App, PaneKind, Spawn, UserEvent};
use edit::{Editor, Move};
use forge_ai::Provider;
use store::Saved;

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
    pub verb: &'static str,
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
    Think { text: String, since: Instant, took: Option<Duration> },
    Tool(Box<ToolItem>),
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
    agent: Agent,
    pub input: Editor,
    pub phase: Phase,
    pub root: Option<PathBuf>,
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
        self.agent.auto()
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

/// Menú o formulario abierto sobre el chat.
#[derive(Default)]
pub enum Popup {
    #[default]
    None,
    Models,
    Projects,
    Form(Box<Form>),
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
}

impl Chats {
    pub fn new() -> Self {
        let saved = Saved::load();
        let servers = saved
            .servers
            .iter()
            .map(|s| ServerView { server: s.clone(), status: Status::Checking, checked: Instant::now() })
            .collect();
        Self {
            list: Vec::new(),
            cur: 0,
            next: 1,
            servers,
            saved,
            popup: Popup::None,
            confirm: None,
            toast: None,
        }
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
            self.board_visible = false;
            self.inbox = None;
            self.dashboard = None;
            self.review = None;
            self.close_search();
            if self.chats.list.is_empty() {
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
        if let Some(i) = self.chats.list.iter().position(|c| c.items.is_empty()) {
            self.chats.cur = i;
            return;
        }
        let id = self.chats.next;
        self.chats.next += 1;
        let proxy = self.proxy.clone();
        let agent = Agent::spawn(Vec::new(), move |e| drop(proxy.send_event(UserEvent::Chat(id, e))));
        let root = self.chats.saved.project.clone().filter(|p| p.is_dir());
        self.chats.list.push(Chat {
            id,
            title: String::new(),
            items: Vec::new(),
            agent,
            input: Editor::default(),
            phase: Phase::Idle,
            root,
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
        });
        self.chats.cur = self.chats.list.len() - 1;
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

    fn toast(&mut self, t: impl Into<String>) {
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
        let c = &mut self.chats.list[i];
        c.on_event(e);
        if seen {
            c.unseen = false;
        } else if c.asking().is_some() {
            c.unseen = true;
        }
        let queued = if done { c.queued.take() } else { None };
        if let Some(n) = notice
            && queued.is_none()
            && (!self.focused || self.occluded)
        {
            let title = if c.title.is_empty() { "forge".to_string() } else { c.title.clone() };
            crate::notify(&title, &n);
        }
        if let Some(q) = queued {
            self.send_to(i, q);
        }
        if failed {
            self.probe_servers();
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
        let Some(c) = self.chats.list.get_mut(i) else { return };
        if c.title.is_empty() {
            c.title = crate::agents::one_line(&text, 60);
        }
        c.items.push(Item::new(Body::User(text.clone())));
        c.phase = Phase::Reading(Instant::now());
        c.stick = true;
        c.sel = None;
        let root = c.root.clone();
        c.agent.prompt(text, Setup { server, model, ctx, root: root.clone() });
        if let Some(p) = root {
            self.chats.saved.used_project(p);
            self.chats.saved.save();
        }
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
        let Some(c) = self.chats.current_mut() else { return };
        // Solo si sigue pendiente (un doble clic no contesta a lo siguiente).
        if c.asking().is_none_or(|t| t.id != id) {
            return;
        }
        if let Some(t) = c.tool(id) {
            t.state = ToolState::Queued;
        }
        c.agent.answer(id, d);
    }

    fn chat_stop(&mut self) {
        let Some(c) = self.chats.current_mut() else { return };
        if c.busy() {
            c.phase = Phase::Stopping;
            c.queued = None;
            c.agent.stop();
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
        if self.chats.current().is_some_and(|c| !c.items.is_empty() && c.root != p) {
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
                    self.chats.cur = i;
                    self.chats.list[i].unseen = false;
                }
            }
            ChatHit::Delete(id) => {
                if confirm != Some(id) {
                    self.chats.confirm = Some(id);
                } else if let Some(i) = self.chats.list.iter().position(|c| c.id == id) {
                    self.chats.list.remove(i);
                    if self.chats.list.is_empty() {
                        self.new_chat();
                    }
                    if self.chats.cur > i || self.chats.cur >= self.chats.list.len() {
                        self.chats.cur = self.chats.cur.saturating_sub(1);
                    }
                }
            }
            ChatHit::Send => self.chat_submit(),
            ChatHit::Stop => self.chat_stop(),
            ChatHit::Mode => {
                if let Some(c) = self.chats.current_mut() {
                    c.agent.set_auto(!c.agent.auto());
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
            _ => {
                self.chats.popup = Popup::None;
                if let Some(c) = self.chats.current_mut() {
                    c.input.insert(t);
                }
            }
        }
        self.redraw();
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
            _ => self.chats.list.get_mut(self.chats.cur).map(|c| &mut c.input),
        }
    }

    fn chat_vertical(&mut self, up: bool, cmd: bool, shift: bool) {
        let to_end = if up { Move::Start } else { Move::End };
        if cmd || matches!(self.chats.popup, Popup::Form(_)) {
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

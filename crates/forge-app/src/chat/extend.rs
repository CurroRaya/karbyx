// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Ampliar lo que sabe hacer el modelo: servidores MCP y habilidades de
//! GitHub. Nada se arranca ni se instala sin que el usuario lo vea y lo acepte.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use forge_agent::install::{self, Bundle, From};
use forge_agent::know::Know;
use forge_agent::mcp::{self, Client, Config, Tool};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::edit::Editor;
use super::{App, ChatHit, Popup, folder_name, store};
use crate::UserEvent;

/// Lo que el usuario decidió de un servidor MCP.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpState {
    /// Huella de la configuración que aprobó.
    pub approved: String,
    /// En todas partes, o en estos proyectos.
    pub always: bool,
    pub projects: Vec<PathBuf>,
    /// Herramientas apagadas y herramientas que no se preguntan.
    pub off: BTreeSet<String>,
    pub trusted: BTreeSet<String>,
}

impl McpState {
    pub fn on_in(&self, project: Option<&Path>) -> bool {
        self.always || project.is_some_and(|p| self.projects.iter().any(|x| x == p))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum McpStatus {
    Unknown,
    Starting,
    Ready(Vec<Tool>),
    Failed(String),
}

/// Un servidor en el panel.
#[derive(Debug, Clone)]
pub struct McpView {
    pub name: String,
    pub config: Config,
    pub status: McpStatus,
}

/// Panel de servidores.
#[derive(Default)]
pub struct McpPanel {
    /// Servidor con sus herramientas desplegadas, y desplazamiento.
    pub open: Option<usize>,
    pub scroll: f32,
    pub max: f32,
    /// Pendiente de confirmar que se quita.
    pub confirm: Option<usize>,
}

/// Añadir servidores pegando su configuración.
#[derive(Default)]
pub struct McpForm {
    pub text: Editor,
    pub parsed: Vec<(String, Config)>,
    pub error: Option<String>,
}

/// Instalar habilidades.
#[derive(Default)]
pub struct SkillForm {
    pub url: Editor,
    pub busy: bool,
    pub error: Option<String>,
    pub bundle: Option<Bundle>,
    /// La que se está mirando, y su desplazamiento en líneas.
    pub sel: Option<usize>,
    pub scroll: f32,
    pub max: f32,
}

fn mcp_path() -> PathBuf {
    match std::env::var_os("FORGE_MCP") {
        Some(p) => p.into(),
        None => dirs::home_dir().unwrap_or_default().join(".config").join("forge").join("mcp.json"),
    }
}

/// Servidores configurados, en el formato habitual.
fn load_mcp() -> Result<Vec<(String, Config)>, String> {
    let text = match std::fs::read_to_string(mcp_path()) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    if serde_json::from_str::<serde_json::Value>(&text)
        .is_ok_and(|v| v.get("mcpServers").and_then(|m| m.as_object()).is_some_and(|m| m.is_empty()))
    {
        return Ok(Vec::new());
    }
    mcp::parse(&text)
}

fn save_mcp(list: &[McpView]) -> std::io::Result<()> {
    let map: BTreeMap<&str, &Config> = list.iter().map(|v| (v.name.as_str(), &v.config)).collect();
    let text = serde_json::to_string_pretty(&json!({"mcpServers": map}))?;
    store::write_private(&mcp_path(), &format!("{text}\n"))
}

/// Servidores listos para una conversación: los que el usuario aprobó tal
/// como están ahora y activó en ese proyecto.
pub fn servers_for(
    list: &[McpView],
    states: &BTreeMap<String, McpState>,
    project: Option<&Path>,
) -> Vec<mcp::Server> {
    list.iter()
        .filter_map(|v| {
            let s = states.get(&v.name)?;
            (s.approved == v.config.fingerprint() && s.on_in(project)).then(|| mcp::Server {
                name: v.name.clone(),
                config: v.config.clone(),
                off: s.off.clone(),
                trusted: s.trusted.clone(),
            })
        })
        .collect()
}

impl App {
    /// Relee los servidores del archivo, conservando lo que ya se sabe.
    pub(super) fn reload_mcp(&mut self) {
        match load_mcp() {
            Ok(list) => {
                let old = std::mem::take(&mut self.chats.mcp);
                self.chats.mcp = list
                    .into_iter()
                    .map(|(name, config)| {
                        let status = old
                            .iter()
                            .find(|v| v.name == name && v.config == config)
                            .map_or(McpStatus::Unknown, |v| v.status.clone());
                        McpView { name, config, status }
                    })
                    .collect();
            }
            // No se pisa un archivo que no se entiende.
            Err(e) => {
                self.chats.mcp_error = Some(format!("No se entiende {}: {e}", mcp_path().display()));
                return;
            }
        }
        self.chats.mcp_error = None;
    }

    fn mcp_project(&self) -> Option<PathBuf> {
        self.chats.current().and_then(|c| c.know().project)
    }

    pub(super) fn open_mcp(&mut self) {
        self.reload_mcp();
        self.chats.popup = Popup::Mcp(Box::default());
        // Los activos se comprueban al abrir el panel.
        let project = self.mcp_project();
        let active: Vec<String> = servers_for(&self.chats.mcp, &self.chats.saved.mcp, project.as_deref())
            .into_iter()
            .map(|s| s.name)
            .collect();
        for n in active {
            self.probe_mcp(&n);
        }
    }

    /// Arranca un servidor aprobado para ver sus herramientas, y lo cierra.
    fn probe_mcp(&mut self, name: &str) {
        let project = self.mcp_project();
        let Some(v) = self.chats.mcp.iter_mut().find(|v| v.name == name) else { return };
        let approved = self.chats.saved.mcp.get(name).is_some_and(|s| s.approved == v.config.fingerprint());
        if !approved || v.status == McpStatus::Starting {
            return;
        }
        v.status = McpStatus::Starting;
        let (name, config, proxy) = (v.name.clone(), v.config.clone(), self.proxy.clone());
        let cwd = project.or_else(dirs::home_dir).unwrap_or_else(|| "/".into());
        std::thread::spawn(move || {
            let r = Client::start(&config, &cwd, &|| false).map(|c| c.tools.clone());
            let _ = proxy.send_event(UserEvent::McpProbe(name, Box::new(config), r));
        });
    }

    pub(crate) fn on_mcp_probe(&mut self, name: String, config: Config, r: Result<Vec<Tool>, String>) {
        if let Some(v) = self.chats.mcp.iter_mut().find(|v| v.name == name && v.config == config) {
            v.status = match r {
                Ok(t) => McpStatus::Ready(t),
                Err(e) => McpStatus::Failed(e),
            };
        }
        self.redraw();
    }

    /// Añade los servidores del formulario y los arranca: el usuario ha visto
    /// sus órdenes y ha pulsado el botón.
    fn add_mcp(&mut self) {
        let Popup::McpForm(f) = &mut self.chats.popup else { return };
        if f.parsed.is_empty() {
            f.error.get_or_insert_with(|| "Pega antes la configuración de un servidor".into());
            return;
        }
        let new = std::mem::take(&mut f.parsed);
        if let Some(e) = self.chats.mcp_error.clone() {
            return self.toast(e);
        }
        let project = self.mcp_project();
        let mut list = self.chats.mcp.clone();
        for (name, config) in &new {
            list.retain(|v| v.name != *name);
            list.push(McpView { name: name.clone(), config: config.clone(), status: McpStatus::Unknown });
        }
        if let Err(e) = save_mcp(&list) {
            return self.toast(format!("No se ha podido guardar: {e}"));
        }
        self.chats.mcp = list;
        for (name, config) in &new {
            let s = self.chats.saved.mcp.entry(name.clone()).or_default();
            // Si es otro programa, no hereda la confianza del anterior.
            if s.approved != config.fingerprint() {
                *s = McpState { approved: config.fingerprint(), ..McpState::default() };
            }
            match &project {
                Some(p) if !s.projects.contains(p) => s.projects.push(p.clone()),
                Some(_) => {}
                None => s.always = true,
            }
        }
        self.chats.saved.save();
        self.open_mcp();
    }

    /// Apagado → este proyecto → siempre.
    fn cycle_mcp(&mut self, i: usize) {
        let project = self.mcp_project();
        let Some(v) = self.chats.mcp.get(i) else { return };
        let (name, print) = (v.name.clone(), v.config.fingerprint());
        let s = self.chats.saved.mcp.entry(name.clone()).or_default();
        if s.approved != print {
            return;
        }
        match (&project, s.always, s.on_in(project.as_deref())) {
            (_, true, _) => {
                s.always = false;
                s.projects.retain(|p| Some(p) != project.as_ref());
            }
            (Some(p), false, false) => s.projects.push(p.clone()),
            _ => s.always = true,
        }
        let on = s.on_in(project.as_deref());
        self.chats.saved.save();
        if on {
            self.probe_mcp(&name);
        }
    }

    /// Una configuración nueva o cambiada se aprueba viéndola entera, como si
    /// se añadiera.
    fn approve_mcp(&mut self, i: usize) {
        let Some(v) = self.chats.mcp.get(i) else { return };
        if review_len(&v.config) > 900 {
            return self.toast("La orden es demasiado larga para revisarla aquí");
        }
        let parsed = vec![(v.name.clone(), v.config.clone())];
        self.chats.popup = Popup::McpForm(Box::new(McpForm { parsed, ..McpForm::default() }));
    }

    fn remove_mcp(&mut self, i: usize) {
        let Popup::Mcp(p) = &mut self.chats.popup else { return };
        if p.confirm != Some(i) {
            p.confirm = Some(i);
            return;
        }
        (p.confirm, p.open) = (None, None);
        if i >= self.chats.mcp.len() {
            return;
        }
        let mut list = self.chats.mcp.clone();
        let gone = list.remove(i);
        if let Err(e) = save_mcp(&list) {
            return self.toast(format!("No se ha podido guardar: {e}"));
        }
        self.chats.mcp = list;
        self.chats.saved.mcp.remove(&gone.name);
        self.chats.saved.save();
    }

    /// Enciende o apaga una herramienta, o cambia si se pregunta.
    fn toggle_mcp_tool(&mut self, i: usize, j: usize, trust: bool) {
        let Some(v) = self.chats.mcp.get(i) else { return };
        let McpStatus::Ready(tools) = &v.status else { return };
        let Some(t) = tools.get(j).map(|t| t.name.clone()) else { return };
        let Some(s) = self.chats.saved.mcp.get_mut(&v.name) else { return };
        let set = if trust { &mut s.trusted } else { &mut s.off };
        if !set.remove(&t) {
            set.insert(t);
        }
        self.chats.saved.save();
    }

    /// «Siempre esta herramienta», desde la tarjeta de permiso.
    pub(super) fn trust_tool(&mut self, server: &str, tool: &str) {
        if let Some(s) = self.chats.saved.mcp.get_mut(server) {
            s.trusted.insert(tool.to_string());
            self.chats.saved.save();
        }
    }

    /// Trae al formulario el `.mcp.json` del proyecto, para revisarlo.
    fn import_mcp(&mut self) {
        let root = self.chats.current().and_then(|c| c.root.clone());
        let text = root.and_then(|r| {
            let p = r.join(".mcp.json");
            let m = p.symlink_metadata().ok()?;
            (m.is_file() && m.len() < 256 * 1024).then(|| std::fs::read_to_string(p).ok())?
        });
        let Some(t) = text else { return self.toast("Este proyecto no tiene .mcp.json") };
        self.chats.popup = Popup::McpForm(Box::default());
        self.mcp_text(&t);
    }

    /// Texto escrito o pegado en el formulario de MCP.
    pub(super) fn mcp_text(&mut self, t: &str) {
        let Popup::McpForm(f) = &mut self.chats.popup else { return };
        f.text.insert(&t.replace(['\n', '\r', '\t'], " "));
        self.mcp_parse();
    }

    pub(super) fn mcp_parse(&mut self) {
        let Popup::McpForm(f) = &mut self.chats.popup else { return };
        (f.parsed, f.error) = match mcp::parse(f.text.text()) {
            // Todo lo que se aprueba tiene que caber a la vista.
            Ok(v) if v.len() > 3 => (Vec::new(), Some("Como mucho 3 a la vez, para revisarlos bien".into())),
            Ok(v) if v.iter().any(|(_, c)| review_len(c) > 900 / v.len()) => {
                (Vec::new(), Some("La orden es demasiado larga para revisarla aquí".into()))
            }
            Ok(v) => (v, None),
            Err(e) => (Vec::new(), Some(e)),
        };
    }

    // ---------- habilidades ----------

    /// Busca habilidades en lo que se escribió.
    fn fetch_skills(&mut self) {
        let Popup::Skill(f) = &mut self.chats.popup else { return };
        if f.busy {
            return;
        }
        let from = match From::parse(f.url.text()) {
            Ok(x) => x,
            Err(e) => {
                f.error = Some(e);
                return;
            }
        };
        (f.busy, f.error, f.bundle, f.sel) = (true, None, None, None);
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let r = install::fetch(&from);
            let _ = proxy.send_event(UserEvent::Skills(Box::new(r)));
        });
    }

    pub(crate) fn on_skills(&mut self, r: Result<Bundle, String>) {
        if let Popup::Skill(f) = &mut self.chats.popup
            && f.busy
        {
            f.busy = false;
            match r {
                Ok(b) => {
                    f.sel = (b.found.len() == 1).then_some(0);
                    f.bundle = Some(b);
                }
                Err(e) => f.error = Some(e),
            }
        }
        self.redraw();
    }

    fn install_skill(&mut self) {
        let Popup::Skill(f) = &mut self.chats.popup else { return };
        let Some((b, s)) = f.bundle.as_ref().and_then(|b| Some((b, b.found.get(f.sel?)?))) else { return };
        match s.install(&store::skills_dir(), &b.from) {
            Ok(()) => {
                let name = s.name.clone();
                // Si hay más, se vuelve a la lista.
                if b.found.len() > 1 {
                    (f.sel, f.scroll) = (None, 0.0);
                } else {
                    self.chats.popup = Popup::None;
                    self.open_memory();
                }
                for a in self.chats.list.iter().filter_map(|c| c.agent.as_ref()) {
                    a.refresh();
                }
                self.toast(format!("Instalada: /{name}"));
            }
            Err(e) => f.error = Some(e),
        }
    }

    /// ↵ en estos paneles.
    pub(super) fn extend_enter(&mut self) -> bool {
        match &self.chats.popup {
            Popup::McpForm(_) => self.add_mcp(),
            Popup::Skill(f) if f.bundle.is_none() => self.fetch_skills(),
            Popup::Skill(_) => {}
            _ => return false,
        }
        true
    }

    /// Esc: vuelve un paso atrás. `false`: nada que hacer.
    pub(super) fn extend_back(&mut self) -> bool {
        match &mut self.chats.popup {
            Popup::McpForm(_) => self.open_mcp(),
            Popup::Skill(f) if f.sel.is_some() && f.bundle.as_ref().is_some_and(|b| b.found.len() > 1) => {
                (f.sel, f.scroll) = (None, 0.0);
            }
            Popup::Skill(f) if f.bundle.is_some() => {
                (f.bundle, f.sel, f.scroll) = (None, None, 0.0);
            }
            Popup::Skill(_) => self.open_memory(),
            _ => return false,
        }
        true
    }

    pub(super) fn extend_wheel(&mut self, px: f32) -> bool {
        let (scroll, max) = match &mut self.chats.popup {
            Popup::Mcp(p) => (&mut p.scroll, p.max),
            Popup::Skill(f) => (&mut f.scroll, f.max),
            _ => return false,
        };
        *scroll = (*scroll - px).clamp(0.0, max);
        self.redraw();
        true
    }

    pub(super) fn extend_click(&mut self, hit: ChatHit) {
        match hit {
            ChatHit::Mcp => self.open_mcp(),
            ChatHit::McpAdd => self.chats.popup = Popup::McpForm(Box::default()),
            ChatHit::McpAddOk => self.add_mcp(),
            ChatHit::McpImport => self.import_mcp(),
            ChatHit::McpState(i) => self.cycle_mcp(i as usize),
            ChatHit::McpApprove(i) => self.approve_mcp(i as usize),
            ChatHit::McpRemove(i) => self.remove_mcp(i as usize),
            ChatHit::McpOpen(i) => {
                if let Popup::Mcp(p) = &mut self.chats.popup {
                    let i = i as usize;
                    p.open = if p.open == Some(i) { None } else { Some(i) };
                }
                let name = self.chats.mcp.get(i as usize).map(|v| (v.name.clone(), v.status.clone()));
                if let Some((n, McpStatus::Unknown | McpStatus::Failed(_))) = name {
                    self.probe_mcp(&n);
                }
            }
            ChatHit::McpTool(i, j) => self.toggle_mcp_tool(i as usize, j as usize, false),
            ChatHit::McpTrust(i, j) => self.toggle_mcp_tool(i as usize, j as usize, true),
            ChatHit::SkillAdd => self.chats.popup = Popup::Skill(Box::default()),
            ChatHit::SkillFetch => self.fetch_skills(),
            ChatHit::SkillPick(i) => {
                if let Popup::Skill(f) = &mut self.chats.popup {
                    (f.sel, f.scroll, f.error) = (Some(i as usize), 0.0, None);
                }
            }
            ChatHit::SkillInstall => self.install_skill(),
            ChatHit::ExtendBack => {
                self.extend_back();
            }
            _ => {}
        }
    }
}

/// Caracteres que hay que revisar de un servidor.
fn review_len(c: &Config) -> usize {
    c.line().chars().count() + c.env.iter().map(|(k, v)| k.len() + v.len() + 2).sum::<usize>()
}

/// Dónde se instalaría, y si sustituye a otra.
pub fn skill_exists(know: &Know, name: &str) -> bool {
    know.skills.join(name).exists()
}

/// Estado de un servidor en un proyecto, para el botón.
pub fn mcp_label(s: Option<&McpState>, project: Option<&Path>) -> String {
    match (s, project) {
        (Some(s), _) if s.always => "Siempre".into(),
        (Some(s), Some(p)) if s.on_in(Some(p)) => format!("En {}", folder_name(p)),
        _ => "Apagado".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_was_approved_runs() {
        let config = Config { command: "a".into(), ..Config::default() };
        let view =
            |c: &Config| vec![McpView { name: "x".into(), config: c.clone(), status: McpStatus::Unknown }];
        let mut states = BTreeMap::new();
        let here = Path::new("/p");
        assert!(servers_for(&view(&config), &states, Some(here)).is_empty(), "sin aprobar");
        let s =
            McpState { approved: config.fingerprint(), projects: vec!["/p".into()], ..McpState::default() };
        states.insert("x".to_string(), s);
        assert_eq!(servers_for(&view(&config), &states, Some(here)).len(), 1);
        assert!(servers_for(&view(&config), &states, Some(Path::new("/otro"))).is_empty());
        assert!(servers_for(&view(&config), &states, None).is_empty());
        // Si alguien cambia la orden, hay que aprobarla otra vez.
        let changed = Config { args: vec!["--malo".into()], ..config.clone() };
        assert!(servers_for(&view(&changed), &states, Some(here)).is_empty());
        states.get_mut("x").unwrap().always = true;
        assert_eq!(servers_for(&view(&config), &states, None).len(), 1);
        assert_eq!(mcp_label(states.get("x"), None), "Siempre");
        assert_eq!(mcp_label(None, Some(here)), "Apagado");
    }
}

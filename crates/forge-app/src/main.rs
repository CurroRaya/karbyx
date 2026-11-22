//! `forge`: terminal acelerada por GPU con sesiones en una barra lateral,
//! paneles divididos y el consumo de los asistentes de IA en un panel derecho.

mod capture;
mod config;
mod gfx;
mod input;
mod keys;
mod panes;
mod rects;
mod ui;
mod usage;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;
use forge_ai::Provider;
use forge_term::mouse::{self, MouseAction, MouseButton as TermButton, MouseMods};
use forge_term::proc::tilde;
use forge_term::{ProcessInfo, SelectionKind, Session, SessionOptions, TermEvent, TermMode, Theme};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::config::Config;
use crate::gfx::{CursorStyle, FontSpec, Gfx, PaneText};
use crate::input::KeyOutcome;
use crate::keys::{Action, Bindings};
use crate::panes::{Divider, Node, PaneId, SplitDir};
use crate::rects::{Rect, linear};
use crate::ui::{Align, Area, Font, Hit, Label, TabView, Ui};
use crate::usage::{SharedUsage, UsageState};

/// Comando de una sesión de asistente: si el asistente termina (o no está
/// instalado), la sesión sigue viva con una shell normal.
fn ai_command(cmd: &str) -> String {
    format!(
        "if command -v {cmd} >/dev/null 2>&1; then {cmd}; \
         else printf '\\033[33mNo encuentro el comando {cmd} en el PATH.\\033[0m\\n'; fi; exec \"$SHELL\" -l"
    )
}

/// Mide las fases de un frame y avisa si alguno es lento (>= 25 ms).
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
        let total = self.start.elapsed();
        if total >= Duration::from_millis(25) {
            let detail: Vec<String> =
                self.phases.iter().map(|(n, d)| format!("{n}={:.1}", d.as_secs_f64() * 1000.0)).collect();
            log::warn!("frame lento {:.1} ms: {}", total.as_secs_f64() * 1000.0, detail.join(" "));
        }
    }
}

#[derive(Debug)]
enum UserEvent {
    Wake,
    /// Modo de prueba: texto a escribir en el panel activo como si se tecleara.
    Type(String),
    /// `forge.toml` ha cambiado.
    Config(Box<Config>),
}

/// `FORGE_SELFTEST=1`: teclea líneas largas (con salto de línea y scroll) en
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
    show_right: bool,
    /// Asistente seleccionado en el panel derecho.
    usage_sel: usize,
    /// Proveedor a seleccionar en cuanto lleguen los datos (`FORGE_PROVIDER`).
    pending_provider: Option<Provider>,
    cursor: (f32, f32),
    cursor_icon: CursorIcon,
    hover: Option<Hit>,
    hits: Vec<(Area, Hit)>,
    /// Geometría del último frame: paneles visibles y divisores.
    pane_areas: Vec<(PaneId, Area)>,
    dividers: Vec<Divider>,
    drag: Option<Drag>,
    last_click: Option<LastClick>,
    search: Option<SearchUi>,
    info_refreshed: Instant,
    /// Momento de arranque, para el modo captura.
    started: Instant,
    /// La ventana no se ve (minimizada, oculta o tapada): no se pinta.
    occluded: bool,
    /// Último título puesto, para no tocar la ventana en cada frame.
    window_title: String,
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
            show_right: true,
            usage_sel: 0,
            pending_provider: std::env::var("FORGE_PROVIDER").ok().and_then(|v| Provider::from_id(&v)),
            cursor: (-1.0, -1.0),
            cursor_icon: CursorIcon::Default,
            hover: None,
            hits: Vec::new(),
            pane_areas: Vec::new(),
            dividers: Vec::new(),
            drag: None,
            last_click: None,
            search: None,
            info_refreshed: Instant::now(),
            started: Instant::now(),
            occluded: false,
            window_title: String::new(),
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
            self.show_right,
        ))
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

    fn spawn_pane(&mut self, kind: PaneKind, area: Area) -> Option<PaneId> {
        let gfx = self.gfx.as_ref()?;
        let size = gfx.grid_size(area);
        let command = match kind {
            PaneKind::Ai(p) => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
                Some((shell, vec!["-l".into(), "-i".into(), "-c".into(), ai_command(p.command())]))
            }
            PaneKind::Shell => None,
        };
        let opts = SessionOptions {
            cwd: self.new_session_cwd(),
            command,
            scrollback: Some(self.config.terminal.scrollback),
            ..Default::default()
        };
        match Session::spawn(opts, size, self.theme.clone(), self.waker()) {
            Ok(session) => {
                let id = self.next_id;
                self.next_id += 1;
                let info = session.process_info();
                self.panes.insert(id, Pane { session, kind, info });
                Some(id)
            }
            Err(e) => {
                log::error!("no se pudo abrir la sesión: {e}");
                None
            }
        }
    }

    fn open_tab(&mut self, kind: PaneKind) -> bool {
        let Some(layout) = self.layout() else { return false };
        let Some(id) = self.spawn_pane(kind, layout.term) else { return false };
        self.close_search();
        self.tabs.push(Tab { root: Node::Leaf(id), focus: id });
        self.active = self.tabs.len() - 1;
        self.redraw();
        true
    }

    fn split(&mut self, dir: SplitDir) {
        let Some(focus) = self.focused_id() else { return };
        // El panel nuevo hereda el tipo del actual solo si es una shell.
        let area = self.pane_areas.iter().find(|(id, _)| *id == focus).map(|(_, a)| *a);
        let Some(area) = area.or_else(|| self.layout().map(|l| l.term)) else { return };
        let half = match dir {
            SplitDir::Horizontal => Area::new(area.x, area.y, area.w / 2.0, area.h),
            SplitDir::Vertical => Area::new(area.x, area.y, area.w, area.h / 2.0),
        };
        let Some(id) = self.spawn_pane(PaneKind::Shell, half) else { return };
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
                self.active = self.active.saturating_sub(1);
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
            self.active = i;
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
        match action {
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
            Action::SelectTab(i) => self.select_tab(i),
            Action::NextTab if n > 0 => self.select_tab((self.active + 1) % n),
            Action::PrevTab if n > 0 => self.select_tab((self.active + n - 1) % n),
            Action::NextTab | Action::PrevTab => {}
            Action::ToggleLeft => self.show_left = !self.show_left,
            Action::ToggleRight => self.show_right = !self.show_right,
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
            Action::Copy => {
                let text = self.focused_pane().and_then(|p| p.session.selection_text());
                if let (Some(t), Some(c)) = (text, self.clipboard.as_mut()) {
                    let _ = c.set_text(t);
                }
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
        }
        self.redraw();
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
        if button == MouseButton::Left
            && let Some(d) = self.dividers.iter().find(|d| d.grip.contains(x, y)).cloned()
        {
            self.drag = Some(Drag::Divider(d));
            return;
        }
        let Some((id, area)) = self.pane_at(x, y) else { return };
        self.set_focus(id);
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
            Some(Drag::Select(id)) => {
                // Un clic simple sin arrastrar no deja selección.
                if let Some(p) = self.panes.get(&id)
                    && p.session.selection_text().is_none()
                {
                    p.session.clear_selection();
                }
            }
            _ => {}
        }
        self.redraw();
    }

    fn mouse_move(&mut self) {
        let (x, y) = self.cursor;
        match self.drag.clone() {
            Some(Drag::Select(id)) => {
                if let Some((_, area)) = self.pane_areas.iter().find(|(p, _)| *p == id).copied()
                    && let Some((row, col, right)) = self.cell_under(id, area)
                    && let Some(p) = self.panes.get(&id)
                {
                    p.session.update_selection(row, col, right);
                    self.redraw();
                }
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
            None => {}
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
        } else if self.pane_at(x, y).is_some() {
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
                    TermEvent::Title(_) | TermEvent::ResetTitle => {}
                }
            }
        }
        for id in exited {
            self.remove_pane(id, event_loop);
        }
        if self.info_refreshed.elapsed() > Duration::from_millis(800) {
            for pane in self.panes.values_mut() {
                pane.info = pane.session.process_info();
            }
            self.info_refreshed = Instant::now();
        }
    }

    fn tab_views(&self) -> Vec<TabView> {
        self.tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let pane = self.panes.get(&t.focus)?;
                let (title, ai) = pane.title();
                let mut subtitle = pane.info.cwd.as_deref().map(tilde).unwrap_or_default();
                let n = t.root.leaves().len();
                if n > 1 {
                    subtitle = format!("{subtitle} · {n} paneles");
                }
                Some(TabView { title, subtitle, ai, active: i == self.active })
            })
            .collect()
    }

    fn render(&mut self, event_loop: &ActiveEventLoop) {
        let mut prof = Profiler::new();
        self.pump_sessions(event_loop);
        prof.mark("pump");
        if self.occluded || self.tabs.is_empty() {
            return;
        }
        let Some(layout) = self.layout() else { return };
        let views = self.tab_views();
        let Some(gfx) = &mut self.gfx else { return };
        let scale = gfx.scale();
        let tab = &self.tabs[self.active];

        // Geometría de los paneles de la pestaña activa.
        let gap = scale.round().max(1.0);
        let (mut pane_areas, mut dividers) = (Vec::new(), Vec::new());
        tab.root.layout(layout.term, gap, &mut pane_areas, &mut dividers);

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
            gfx.prepare_rows(*id, &snap, area.w - m.pad);
            texts.push(PaneText { pane: *id, ox, oy, clip: *area });
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
                });
            }
        }
        prof.mark("panes");
        for d in &dividers {
            rects.push(Rect {
                pos: [d.line.x, d.line.y],
                size: [d.line.w, d.line.h],
                color: linear(ui::BORDER, 1.0),
            });
        }

        // Título de la ventana: el del panel activo.
        if let Some(v) = views.get(self.active) {
            let title = if v.subtitle.is_empty() {
                format!("{} — forge", v.title)
            } else {
                format!("{} · {} — forge", v.title, v.subtitle)
            };
            // Cambiar el título implica al servidor de ventanas: solo si cambia.
            if title != self.window_title {
                gfx.window.set_title(&title);
                self.window_title = title;
            }
        }

        // Paneles laterales y barra de búsqueda.
        let mut ui = Ui::new(scale);
        let usage: UsageState =
            self.usage.as_ref().and_then(|u| u.lock().ok().map(|g| g.clone())).unwrap_or_default();
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
        if let Some(area) = layout.left {
            ui::draw_left(&mut ui, area, &views, ai, self.hover);
        }
        if let Some(area) = layout.right {
            ui::draw_right(&mut ui, area, &usage, self.usage_sel, self.hover, Utc::now());
        }
        let mut overlay = Ui::new(scale);
        if let Some(s) = &self.search
            && let Some((_, area)) = pane_areas.iter().find(|(id, _)| *id == tab.focus)
        {
            ui::draw_search(&mut overlay, *area, &s.query, s.found);
        }
        rects.extend(ui.rects.iter().copied());
        labels.append(&mut ui.labels);
        self.hits = std::mem::take(&mut ui.hits);
        self.pane_areas = pane_areas;
        self.dividers = dividers;

        // Captura (modo desarrollo) cuando la shell ya ha pintado.
        let capture = capture::target().filter(|_| self.started.elapsed() > Duration::from_secs(4));
        // `FORGE_SEARCH=texto` abre la búsqueda en modo captura, para verla.
        if capture::target().is_some()
            && self.search.is_none()
            && self.started.elapsed() > Duration::from_secs(3)
            && let Ok(q) = std::env::var("FORGE_SEARCH")
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
            &overlay.rects,
            &overlay.labels,
            self.theme.foreground,
            self.theme.background,
            capture.as_deref(),
        );
        prof.mark("draw");
        prof.report();
        if captured {
            event_loop.exit();
        } else if capture::target().is_some() {
            gfx.window.request_redraw();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let attrs =
            Window::default_attributes().with_title("forge").with_inner_size(LogicalSize::new(1320.0, 800.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("no se pudo crear la ventana: {e}");
                event_loop.exit();
                return;
            }
        };
        match pollster::block_on(Gfx::new(window.clone(), event_loop, self.font_spec())) {
            Ok(g) => self.gfx = Some(g),
            Err(e) => {
                log::error!("no se pudo iniciar la GPU: {e}");
                event_loop.exit();
                return;
            }
        }
        if !self.open_tab(PaneKind::Shell) {
            event_loop.exit();
            return;
        }
        if capture::target().is_some() {
            // En modo captura: una segunda pestaña y la primera dividida en dos.
            self.open_tab(PaneKind::Shell);
            self.active = 0;
            self.split(SplitDir::Horizontal);
        }
        self.usage = Some(usage::spawn(self.waker()));
        let proxy = self.proxy.clone();
        config::watch(move |c| {
            let _ = proxy.send_event(UserEvent::Config(Box::new(c)));
        });
        if std::env::var_os("FORGE_SELFTEST").is_some() {
            spawn_selftest(self.proxy.clone());
        }
        window.request_redraw();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Type(text) => {
                if let Some(p) = self.focused_pane() {
                    p.session.write(text.into_bytes());
                }
            }
            UserEvent::Config(c) => self.apply_config(*c),
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
                // Los atajos de la terminal tienen prioridad, también sobre la búsqueda.
                if let Some(a) = self.bindings.lookup(&event, self.mods) {
                    self.handle_action(a, event_loop);
                    return;
                }
                if self.search.is_some() && self.search_key(&event) {
                    return;
                }
                let Some(pane) = self.focused_pane() else { return };
                let outcome = input::translate(
                    &event,
                    self.mods,
                    pane.session.mode(),
                    &self.bindings,
                    self.config.terminal.option_as_alt,
                );
                match outcome {
                    KeyOutcome::Bytes(b) => {
                        pane.session.clear_selection();
                        pane.session.write(b);
                    }
                    KeyOutcome::Action(a) => self.handle_action(a, event_loop),
                    KeyOutcome::None => {}
                }
            }
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

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let mut app = App::new(event_loop.create_proxy(), Config::load());
    event_loop.run_app(&mut app)?;
    Ok(())
}

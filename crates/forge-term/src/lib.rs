//! `forge-term`: sesión de terminal (PTY + emulador VT) independiente de la UI.
//!
//! Envuelve `alacritty_terminal`: arranca la shell en un PTY, procesa la salida
//! en un hilo propio y expone un [`Snapshot`] del grid con los colores ya
//! resueltos, para que el renderer no tenga que tocar el lock más de lo justo.

pub mod mouse;
pub mod proc;
pub mod theme;

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use crossbeam_channel::{Receiver, Sender, unbounded};

pub use alacritty_terminal::term::TermMode;
pub use theme::{Rgb8, Theme};

/// Eventos relevantes para la UI.
#[derive(Debug, Clone)]
pub enum TermEvent {
    Title(String),
    ResetTitle,
    Bell,
    /// La aplicación pide copiar texto al portapapeles (OSC 52).
    ClipboardStore(String),
    /// La shell terminó.
    Exited,
}

/// Tamaño del grid en celdas y del área en píxeles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

impl From<GridSize> for WindowSize {
    fn from(s: GridSize) -> Self {
        WindowSize {
            num_lines: s.rows,
            num_cols: s.cols,
            cell_width: s.cell_width,
            cell_height: s.cell_height,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    /// Programa y argumentos. `None` usa la shell del usuario (`$SHELL`).
    pub command: Option<(String, Vec<String>)>,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub scrollback: Option<usize>,
}

/// Puente entre los eventos de alacritty y la UI.
#[derive(Clone)]
struct Listener {
    tx: Sender<Event>,
    waker: Arc<dyn Fn() + Send + Sync>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.tx.send(event);
        (self.waker)();
    }
}

/// Una celda lista para pintar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellView {
    pub c: char,
    pub fg: Rgb8,
    pub bg: Rgb8,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    /// Carácter de doble ancho (ocupa esta celda y la siguiente).
    pub wide: bool,
    /// Segunda mitad de un carácter ancho: no se pinta texto.
    pub spacer: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorView {
    pub row: usize,
    pub col: usize,
    pub visible: bool,
}

/// Copia del contenido visible del terminal.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub cols: usize,
    pub rows: usize,
    /// `rows × cols` celdas, fila a fila.
    pub cells: Vec<CellView>,
    pub cursor: CursorView,
    pub display_offset: usize,
    pub mode: TermMode,
    pub background: Rgb8,
}

impl Snapshot {
    pub fn row(&self, r: usize) -> &[CellView] {
        &self.cells[r * self.cols..(r + 1) * self.cols]
    }
}

pub struct Session {
    term: Arc<FairMutex<Term<Listener>>>,
    notifier: Notifier,
    events: Receiver<Event>,
    theme: Theme,
    size: GridSize,
    /// Descriptor maestro del PTY (lo mantiene vivo el hilo de I/O).
    master_fd: i32,
    shell_pid: i32,
    search: RefCell<Option<SearchState>>,
}

struct SearchState {
    regex: RegexSearch,
    current: Option<Match>,
}

/// Tipo de selección con el ratón.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// Arrastrar.
    Simple,
    /// Doble clic: palabra.
    Word,
    /// Triple clic: línea.
    Line,
    /// Arrastrar con ⌥: bloque rectangular.
    Block,
}

/// Convierte texto literal en una regex que lo busca tal cual. Sin
/// mayúsculas en el texto, la búsqueda no distingue mayúsculas (smart case).
pub fn literal_regex(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    if !text.chars().any(char::is_uppercase) {
        out.push_str("(?i)");
    }
    for c in text.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Qué está ejecutando la sesión ahora mismo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessInfo {
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
}

impl Session {
    /// Arranca la shell. `waker` se invoca (desde otro hilo) cuando hay
    /// contenido nuevo o eventos, para que la UI pida un redibujado.
    pub fn spawn(
        opts: SessionOptions,
        size: GridSize,
        theme: Theme,
        waker: Arc<dyn Fn() + Send + Sync>,
    ) -> anyhow::Result<Self> {
        tty::setup_env();
        let mut env = opts.env.clone();
        env.entry("TERM_PROGRAM".into()).or_insert_with(|| "forge".into());
        let pty_opts = tty::Options {
            shell: opts.command.map(|(p, a)| tty::Shell::new(p, a)),
            working_directory: opts.cwd,
            drain_on_exit: false,
            env,
        };
        let pty = tty::new(&pty_opts, size.into(), 0)?;
        let master_fd = pty.file().as_raw_fd();
        let shell_pid = pty.child().id() as i32;

        let (tx, events) = unbounded();
        let listener = Listener { tx, waker };
        let config = Config { scrolling_history: opts.scrollback.unwrap_or(10_000), ..Config::default() };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        let event_loop = EventLoop::new(term.clone(), listener, pty, false, false)?;
        let notifier = Notifier(event_loop.channel());
        event_loop.spawn();

        Ok(Self { term, notifier, events, theme, size, master_fd, shell_pid, search: RefCell::new(None) })
    }

    /// Proceso en primer plano y su directorio (o los de la shell).
    pub fn process_info(&self) -> ProcessInfo {
        let pid = proc::foreground_pid(self.master_fd).unwrap_or(self.shell_pid);
        ProcessInfo {
            name: proc::process_name(pid),
            cwd: proc::process_cwd(pid).or_else(|| proc::process_cwd(self.shell_pid)),
        }
    }

    pub fn size(&self) -> GridSize {
        self.size
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Cambia el tema (recarga de configuración en caliente).
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// Envía bytes a la shell (teclas, pegado...).
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        // Escribir vuelve a la parte inferior del scrollback, como en iTerm2.
        self.term.lock().scroll_display(Scroll::Bottom);
        self.notifier.notify(bytes);
    }

    pub fn resize(&mut self, size: GridSize) {
        if size == self.size || size.cols == 0 || size.rows == 0 {
            return;
        }
        self.size = size;
        self.term.lock().resize(size);
        self.notifier.on_resize(size.into());
    }

    // ---------- selección ----------

    fn grid_point<T>(term: &Term<T>, row: usize, col: usize) -> Point {
        let offset = term.grid().display_offset() as i32;
        let col = col.min(term.columns().saturating_sub(1));
        let row = row.min(term.screen_lines().saturating_sub(1));
        Point::new(Line(row as i32 - offset), Column(col))
    }

    /// Empieza una selección en la celda (fila, columna) del viewport.
    /// `right_half`: el clic cayó en la mitad derecha de la celda.
    pub fn start_selection(&self, kind: SelectionKind, row: usize, col: usize, right_half: bool) {
        let mut term = self.term.lock();
        let point = Self::grid_point(&term, row, col);
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
            SelectionKind::Block => SelectionType::Block,
        };
        let side = if right_half { Side::Right } else { Side::Left };
        term.selection = Some(Selection::new(ty, point, side));
    }

    pub fn update_selection(&self, row: usize, col: usize, right_half: bool) {
        let mut term = self.term.lock();
        let point = Self::grid_point(&term, row, col);
        let side = if right_half { Side::Right } else { Side::Left };
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, side);
        }
    }

    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    /// Texto seleccionado (si la selección no está vacía).
    pub fn selection_text(&self) -> Option<String> {
        let term = self.term.lock();
        if term.selection.as_ref().is_none_or(|s| s.is_empty()) {
            return None;
        }
        term.selection_to_string().filter(|s| !s.is_empty())
    }

    // ---------- búsqueda ----------

    /// Activa la búsqueda de `text` (literal, smart case) y salta a la
    /// coincidencia más reciente. Devuelve si hay alguna.
    pub fn search_set(&self, text: &str) -> bool {
        if text.is_empty() {
            self.search_clear();
            return false;
        }
        match RegexSearch::new(&literal_regex(text)) {
            Ok(regex) => {
                *self.search.borrow_mut() = Some(SearchState { regex, current: None });
                self.search_step(true)
            }
            Err(_) => false,
        }
    }

    /// Siguiente coincidencia: `up` hacia el historial más antiguo.
    pub fn search_step(&self, up: bool) -> bool {
        let mut guard = self.search.borrow_mut();
        let Some(st) = guard.as_mut() else { return false };
        let mut term = self.term.lock();
        let found = match (st.current.as_ref(), up) {
            (Some(m), true) => {
                let origin = m.start().sub(&*term, Boundary::None, 1);
                term.search_next(&mut st.regex, origin, Direction::Left, Side::Left, None)
            }
            (Some(m), false) => {
                let origin = m.end().add(&*term, Boundary::None, 1);
                term.search_next(&mut st.regex, origin, Direction::Right, Side::Right, None)
            }
            (None, true) => {
                let origin = Point::new(term.bottommost_line(), term.last_column());
                term.search_next(&mut st.regex, origin, Direction::Left, Side::Left, None)
            }
            (None, false) => {
                let origin = Point::new(term.topmost_line(), Column(0));
                term.search_next(&mut st.regex, origin, Direction::Right, Side::Right, None)
            }
        };
        match found {
            Some(m) => {
                term.scroll_to_point(*m.start());
                st.current = Some(m);
                true
            }
            None => false,
        }
    }

    pub fn search_clear(&self) {
        *self.search.borrow_mut() = None;
    }

    pub fn search_active(&self) -> bool {
        self.search.borrow().is_some()
    }

    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    /// Pega texto respetando el modo *bracketed paste*.
    pub fn paste(&self, text: &str) {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.mode().contains(TermMode::BRACKETED_PASTE) {
            // Evita que el texto pegado cierre el bracket por su cuenta.
            let clean = text.replace("\x1b[201~", "");
            self.write(format!("\x1b[200~{clean}\x1b[201~").into_bytes());
        } else {
            self.write(text.into_bytes());
        }
    }

    /// Recoge los eventos pendientes. Algunos (respuestas a la shell) se
    /// gestionan aquí mismo.
    pub fn poll_events(&self) -> Vec<TermEvent> {
        let mut out = Vec::new();
        for ev in self.events.try_iter() {
            match ev {
                Event::Title(t) => out.push(TermEvent::Title(t)),
                Event::ResetTitle => out.push(TermEvent::ResetTitle),
                Event::Bell => out.push(TermEvent::Bell),
                Event::ClipboardStore(_, s) => out.push(TermEvent::ClipboardStore(s)),
                Event::PtyWrite(s) => self.notifier.notify(s.into_bytes()),
                Event::ColorRequest(i, fmt) => {
                    let term = self.term.lock();
                    let c = term.colors()[i];
                    drop(term);
                    let rgb = c.unwrap_or_else(|| {
                        let named = match i {
                            256 => self.theme.foreground,
                            257 => self.theme.background,
                            258 => self.theme.cursor,
                            n if n < 256 => self.theme.indexed(n as u8),
                            _ => self.theme.foreground,
                        };
                        alacritty_terminal::vte::ansi::Rgb { r: named[0], g: named[1], b: named[2] }
                    });
                    self.notifier.notify(fmt(rgb).into_bytes());
                }
                Event::TextAreaSizeRequest(fmt) => {
                    self.notifier.notify(fmt(self.size.into()).into_bytes());
                }
                Event::ChildExit(_) | Event::Exit => out.push(TermEvent::Exited),
                _ => {}
            }
        }
        out
    }

    /// Copia el contenido visible con los colores resueltos.
    pub fn snapshot(&self) -> Snapshot {
        let term = self.term.lock();
        let content = term.renderable_content();
        let cols = term.columns();
        let rows = term.screen_lines();
        let offset = content.display_offset;

        // Coincidencias de búsqueda visibles, por fila del viewport.
        let mut matches: Vec<Vec<(usize, usize, bool)>> = vec![Vec::new(); rows];
        if let Some(st) = self.search.borrow_mut().as_mut() {
            let top = Point::new(Line(-(offset as i32)), Column(0));
            let bottom = Point::new(Line(rows as i32 - 1 - offset as i32), Column(cols.saturating_sub(1)));
            let current = st.current.clone();
            let found: Vec<Match> =
                RegexIter::new(top, bottom, Direction::Right, &term, &mut st.regex).take(500).collect();
            for m in found {
                let is_cur = current.as_ref() == Some(&m);
                let (s, e) = (*m.start(), *m.end());
                for line in s.line.0..=e.line.0 {
                    let row = line + offset as i32;
                    if row < 0 || row as usize >= rows {
                        continue;
                    }
                    let c0 = if line == s.line.0 { s.column.0 } else { 0 };
                    let c1 = if line == e.line.0 { e.column.0 } else { cols - 1 };
                    matches[row as usize].push((c0, c1, is_cur));
                }
            }
        }
        let selection = content.selection;

        let mut cells = vec![
            CellView {
                c: ' ',
                fg: self.theme.foreground,
                bg: self.theme.background,
                bold: false,
                italic: false,
                underline: false,
                strikeout: false,
                wide: false,
                spacer: false,
            };
            cols * rows
        ];

        for ic in content.display_iter {
            let row = ic.point.line.0 + offset as i32;
            if row < 0 || row as usize >= rows {
                continue;
            }
            let col = ic.point.column.0;
            let f = ic.flags;
            let mut fg = self.theme.resolve(ic.fg, content.colors);
            let mut bg = self.theme.resolve(ic.bg, content.colors);
            if f.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if f.contains(Flags::HIDDEN) {
                fg = bg;
            }
            if selection.is_some_and(|s| s.contains(ic.point)) {
                bg = self.theme.selection;
            }
            if let Some(&(_, _, cur)) =
                matches[row as usize].iter().find(|(a, b, _)| (*a..=*b).contains(&col))
            {
                if cur {
                    bg = self.theme.search_current;
                    fg = self.theme.background;
                } else {
                    bg = self.theme.search_match;
                }
            }
            cells[row as usize * cols + col] = CellView {
                c: ic.c,
                fg,
                bg,
                bold: f.contains(Flags::BOLD),
                italic: f.contains(Flags::ITALIC),
                underline: f.intersects(Flags::ALL_UNDERLINES),
                strikeout: f.contains(Flags::STRIKEOUT),
                wide: f.contains(Flags::WIDE_CHAR),
                spacer: f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
            };
        }

        let cur = content.cursor.point;
        let cur_row = cur.line.0 + offset as i32;
        let cursor = CursorView {
            row: cur_row.max(0) as usize,
            col: cur.column.0,
            visible: content.mode.contains(TermMode::SHOW_CURSOR)
                && cur_row >= 0
                && (cur_row as usize) < rows,
        };

        Snapshot {
            cols,
            rows,
            cells,
            cursor,
            display_offset: offset,
            mode: content.mode,
            background: self.theme.background,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_regex_escapes() {
        assert_eq!(literal_regex("a.b"), "(?i)a\\.b");
        assert_eq!(literal_regex("Foo(1)"), "Foo\\(1\\)");
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Herramientas con las que el modelo trabaja en el proyecto. Cada llamada
//! se prepara (qué haría, qué permiso necesita, qué diff se enseña) y solo
//! después se ejecuta.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::Call;
use crate::exec::{self, End};
use crate::know;
use crate::sandbox;

/// Tamaño máximo de un archivo de texto que se lee o edita.
const MAX_FILE: u64 = 4 << 20;
/// Resultados máximos de una búsqueda.
const MAX_HITS: usize = 120;

#[derive(Debug, Clone)]
pub struct Ctx {
    /// Carpeta del proyecto (ruta real).
    pub root: PathBuf,
    /// Proyecto del que sale la carpeta (ella misma, o el repositorio si es
    /// una copia): decide la caché de compilación.
    pub project: PathBuf,
    /// Caracteres máximos de un resultado (según el contexto del modelo).
    pub max_chars: usize,
    /// Carpeta de las habilidades del usuario: leerla es libre.
    pub skills: Option<PathBuf>,
}

impl Ctx {
    /// `None` si la carpeta no puede ser un proyecto (no existe, o contiene
    /// la carpeta personal entera).
    pub fn new(root: &Path, max_chars: usize) -> Option<Self> {
        let root = root.canonicalize().ok().filter(|r| r.is_dir())?;
        let home = dirs::home_dir().and_then(|h| h.canonicalize().ok());
        if home.is_some_and(|h| h.starts_with(&root)) || root.parent().is_none() {
            return None;
        }
        Some(Self { project: root.clone(), root, max_chars, skills: None })
    }
}

/// Qué permiso necesita una acción.
#[derive(Debug, Clone, PartialEq)]
pub enum Need {
    /// Ninguno (leer dentro del proyecto).
    Free,
    /// El normal: se pregunta, salvo en modo automático.
    Normal,
    /// Se pregunta siempre, y se dice por qué.
    Always(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum LineKind {
    Same,
    Add,
    Del,
    /// Separador entre trozos.
    Gap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
}

/// Lo que se enseña al usuario de una acción.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Detail {
    None,
    Diff { lines: Vec<DiffLine>, added: usize, removed: usize, new: bool },
    Command { cmd: String, sandboxed: bool },
}

#[derive(Debug, Clone, PartialEq)]
enum Job {
    /// Memoria y habilidades.
    Know(know::Job),
    Read {
        path: PathBuf,
        offset: usize,
        limit: usize,
    },
    List {
        path: PathBuf,
        depth: usize,
    },
    Search {
        path: PathBuf,
        pattern: String,
        glob: Option<String>,
    },
    Find {
        pattern: String,
    },
    /// `before`: contenido que se vio al preparar (`None`: no existía).
    Put {
        path: PathBuf,
        before: Option<String>,
        after: String,
    },
    Run {
        cmd: String,
        sandboxed: bool,
        timeout: Duration,
    },
}

/// Una llamada lista para ejecutarse.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    /// Verbo y objeto para el usuario: («Leer», «src/main.rs»).
    pub verb: &'static str,
    pub target: String,
    pub detail: Detail,
    pub need: Need,
    job: Job,
}

impl Action {
    pub(crate) fn know(
        verb: &'static str,
        target: String,
        detail: Detail,
        need: Need,
        job: know::Job,
    ) -> Self {
        Self { verb, target, detail, need, job: Job::Know(job) }
    }

    /// Lo que hace, si es de memoria o habilidades (no necesita carpeta).
    pub(crate) fn know_job(&self) -> Option<&know::Job> {
        match &self.job {
            Job::Know(j) => Some(j),
            _ => None,
        }
    }
}

/// Resultado de una acción.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub ok: bool,
    /// Para el modelo.
    pub text: String,
    /// Para el usuario («120 líneas», «salió con 1»).
    pub summary: String,
}

/// Definiciones que se envían al modelo.
pub fn definitions() -> Vec<Value> {
    let f = |name: &str, about: &str, props: Value, required: &[&str]| {
        json!({"type": "function", "function": {"name": name, "description": about,
            "parameters": {"type": "object", "properties": props, "required": required}}})
    };
    let s = |d: &str| json!({"type": "string", "description": d});
    let n = |d: &str| json!({"type": "integer", "description": d});
    let b = |d: &str| json!({"type": "boolean", "description": d});
    vec![
        f(
            "read",
            "Read a text file. Returns lines prefixed with their number and a tab (the prefix is not part of the file).",
            json!({"path": s("File path, relative to the project"), "offset": n("First line, from 1"), "limit": n("Maximum number of lines")}),
            &["path"],
        ),
        f(
            "list",
            "List a directory as a tree (ignores .gitignore'd files).",
            json!({"path": s("Directory, relative to the project. Default: the project"), "depth": n("Levels to show, default 2")}),
            &[],
        ),
        f(
            "search",
            "Search file contents with a regular expression. Returns path:line: text.",
            json!({"pattern": s("Regular expression"), "path": s("File or directory to search. Default: the project"), "glob": s("Only files matching this glob, e.g. *.rs")}),
            &["pattern"],
        ),
        f(
            "find",
            "Find files by name with a glob, e.g. **/*.toml",
            json!({"pattern": s("Glob")}),
            &["pattern"],
        ),
        f(
            "edit",
            "Replace an exact piece of text in a file. `old` must appear exactly once (include enough surrounding lines) unless `all` is true.",
            json!({"path": s("File path"), "old": s("Exact text to replace, without line numbers"), "new": s("Replacement text"), "all": b("Replace every occurrence")}),
            &["path", "old", "new"],
        ),
        f(
            "write",
            "Create a file, or replace all its content. To change part of an existing file use edit.",
            json!({"path": s("File path"), "content": s("Full content")}),
            &["path", "content"],
        ),
        f(
            "bash",
            "Run a shell command in the project directory. It runs in a sandbox: it can read almost everything but only write inside the project, with no network. If a command needs more than that, set unsandboxed to true and the user will be asked.",
            json!({"command": s("Command"), "timeout": n("Seconds, default 120, maximum 900"), "unsandboxed": b("Run outside the sandbox (always asks the user)")}),
            &["command"],
        ),
    ]
}

/// Verbo provisional mientras el modelo aún escribe la llamada.
pub fn verb(name: &str) -> &'static str {
    match name {
        "read" => "Leer",
        "list" => "Mirar",
        "search" => "Buscar",
        "find" => "Encontrar",
        "edit" => "Editar",
        "write" => "Escribir",
        "bash" => "Ejecutar",
        "remember" => "Recordar",
        "forget" => "Olvidar",
        "skill" => "Habilidad",
        "save_skill" => "Guardar habilidad",
        _ => "Herramienta",
    }
}

// ---------- argumentos ----------

struct Args(Value);

impl Args {
    fn str(&self, names: &[&str]) -> Option<String> {
        names.iter().find_map(|n| match self.0.get(n)? {
            Value::String(s) => Some(s.clone()),
            v @ (Value::Number(_) | Value::Bool(_)) => Some(v.to_string()),
            _ => None,
        })
    }
    fn need(&self, names: &[&str]) -> Result<String, String> {
        self.str(names).ok_or(format!("Missing argument `{}`.", names[0]))
    }
    fn num(&self, names: &[&str]) -> Option<usize> {
        names.iter().find_map(|n| match self.0.get(n)? {
            Value::Number(x) => x.as_f64().map(|f| f.max(0.0) as usize),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        })
    }
    fn flag(&self, names: &[&str]) -> bool {
        names.iter().any(|n| match self.0.get(n) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
            _ => false,
        })
    }
}

// ---------- rutas ----------

/// Ruta real (resolviendo enlaces, también si el archivo aún no existe).
fn resolve(ctx: &Ctx, p: &str) -> Result<PathBuf, String> {
    let p = p.trim();
    if p.contains('\0') {
        return Err("Invalid path.".into());
    }
    let p = match p.strip_prefix("~/") {
        Some(r) => dirs::home_dir().ok_or("No home directory.")?.join(r),
        None => PathBuf::from(if p.is_empty() { "." } else { p }),
    };
    let abs = ctx.root.join(p);
    // `..` se resuelve sobre lo que existe, no sobre el texto.
    let mut real = PathBuf::new();
    let mut missing = false;
    for c in abs.components() {
        match c {
            Component::ParentDir => {
                if missing {
                    return Err("Invalid path.".into());
                }
                real.pop();
            }
            Component::CurDir => {}
            c => {
                real.push(c);
                if missing {
                    continue;
                }
                match real.canonicalize() {
                    Ok(r) => real = r,
                    // Un enlace roto: escribir en él crearía su destino, quién sabe dónde.
                    Err(_) if real.symlink_metadata().is_ok() => return Err("Broken symbolic link.".into()),
                    Err(_) => missing = true,
                }
            }
        }
    }
    Ok(real)
}

/// Ruta para enseñar: relativa al proyecto si está dentro.
fn show(ctx: &Ctx, p: &Path) -> String {
    match p.strip_prefix(&ctx.root) {
        Ok(r) if r.as_os_str().is_empty() => ".".into(),
        Ok(r) => r.display().to_string(),
        Err(_) => match dirs::home_dir().and_then(|h| p.strip_prefix(h).ok().map(Path::to_path_buf)) {
            Some(r) => format!("~/{}", r.display()),
            None => p.display().to_string(),
        },
    }
}

fn looks_secret(p: &Path) -> bool {
    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    (name.starts_with(".env") && !name.ends_with(".example") && !name.ends_with(".sample"))
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.ends_with(".pem")
        || name.ends_with(".p12")
        || name == ".netrc"
        || name == ".git-credentials"
}

fn read_need(ctx: &Ctx, p: &Path) -> Need {
    if looks_secret(p) {
        Need::Always("puede contener claves".into())
    } else if !p.starts_with(&ctx.root) && !ctx.skills.as_ref().is_some_and(|s| p.starts_with(s)) {
        Need::Always("está fuera del proyecto".into())
    } else {
        Need::Free
    }
}

fn write_need(ctx: &Ctx, p: &Path) -> Result<Need, String> {
    let Ok(rel) = p.strip_prefix(&ctx.root) else {
        return Ok(Need::Always("está fuera del proyecto".into()));
    };
    for c in rel.components() {
        let name = c.as_os_str().to_string_lossy().to_lowercase();
        if name == ".git" {
            return Err("Writing inside .git is not allowed.".into());
        }
        if sandbox::PROTECTED.contains(&name.as_str()) {
            return Ok(Need::Always("lo leen otras herramientas fuera del sandbox".into()));
        }
    }
    Ok(Need::Normal)
}

fn read_text(p: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(p).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "File not found.".to_string(),
        _ => format!("Cannot read it: {e}"),
    })?;
    if meta.is_dir() {
        return Err("It is a directory. Use list.".into());
    }
    if !meta.is_file() {
        return Err("It is not a regular file.".into());
    }
    if meta.len() > MAX_FILE {
        return Err(format!("The file is too big ({} MB).", meta.len() >> 20));
    }
    let bytes = std::fs::read(p).map_err(|e| format!("Cannot read it: {e}"))?;
    if bytes.contains(&0) {
        return Err("It is a binary file.".into());
    }
    String::from_utf8(bytes).map_err(|_| "It is not UTF-8 text.".into())
}

// ---------- preparar ----------

/// Comprueba la llamada y calcula qué haría. El error es para el modelo.
pub fn prepare(ctx: &Ctx, call: &Call) -> Result<Action, String> {
    let v: Value = serde_json::from_str(&call.args)
        .map_err(|e| format!("The arguments are not valid JSON ({e}). Call the tool again."))?;
    if !v.is_object() {
        return Err("The arguments must be a JSON object.".into());
    }
    let a = Args(v);
    const PATH: &[&str] = &["path", "file_path", "file", "filename", "filePath"];
    let verb = verb(&call.name);
    let plain =
        |target: String, need: Need, job: Job| Action { verb, target, detail: Detail::None, need, job };
    Ok(match call.name.as_str() {
        "read" => {
            let path = resolve(ctx, &a.need(PATH)?)?;
            let offset = a.num(&["offset", "start"]).unwrap_or(1).max(1);
            let limit = a.num(&["limit", "lines"]).unwrap_or(2000).max(1);
            plain(show(ctx, &path), read_need(ctx, &path), Job::Read { path, offset, limit })
        }
        "list" => {
            let path = resolve(ctx, &a.str(&["path", "dir", "directory"]).unwrap_or_default())?;
            let depth = a.num(&["depth"]).unwrap_or(2).clamp(1, 6);
            let target = if path == ctx.root { "el proyecto".into() } else { show(ctx, &path) };
            plain(target, read_need(ctx, &path), Job::List { path, depth })
        }
        "search" => {
            let pattern = a.need(&["pattern", "query", "regex"])?;
            let path = resolve(ctx, &a.str(&["path"]).unwrap_or_default())?;
            let place = if path == ctx.root { String::new() } else { format!(" en {}", show(ctx, &path)) };
            let need = read_need(ctx, &path);
            let glob = a.str(&["glob", "include"]).filter(|g| !g.is_empty());
            plain(format!("«{pattern}»{place}"), need, Job::Search { path, pattern, glob })
        }
        "find" => {
            let pattern = a.need(&["pattern", "glob", "name"])?;
            plain(pattern.clone(), Need::Free, Job::Find { pattern })
        }
        "edit" => {
            let path = resolve(ctx, &a.need(PATH)?)?;
            let need = write_need(ctx, &path)?;
            let before = read_text(&path)?;
            let old = a.need(&["old", "old_string", "oldString"])?;
            let new = a.need(&["new", "new_string", "newString"])?;
            let after = replace(&before, &old, &new, a.flag(&["all", "replace_all", "replaceAll"]))?;
            put(ctx, "Editar", path, need, Some(before), after)
        }
        "write" => {
            let path = resolve(ctx, &a.need(PATH)?)?;
            let need = write_need(ctx, &path)?;
            let after = a.need(&["content", "text", "contents"])?;
            let before = if path.exists() { Some(read_text(&path)?) } else { None };
            put(ctx, if before.is_some() { "Reescribir" } else { "Crear" }, path, need, before, after)
        }
        "bash" => {
            let cmd = a.need(&["command", "cmd"])?.trim().to_string();
            if cmd.is_empty() || cmd.contains('\0') {
                return Err("Empty command.".into());
            }
            let sandboxed = !a.flag(&["unsandboxed"]);
            let secs = a.num(&["timeout", "timeout_secs"]).unwrap_or(120).clamp(1, 900);
            Action {
                verb,
                target: cmd.lines().next().unwrap_or_default().to_string(),
                detail: Detail::Command { cmd: cmd.clone(), sandboxed },
                need: if sandboxed {
                    Need::Normal
                } else {
                    Need::Always("se ejecuta fuera del sandbox".into())
                },
                job: Job::Run { cmd, sandboxed, timeout: Duration::from_secs(secs as u64) },
            }
        }
        other => {
            return Err(format!(
                "There is no tool called `{other}`. Use read, list, search, find, edit, write or bash."
            ));
        }
    })
}

fn put(
    ctx: &Ctx,
    verb: &'static str,
    path: PathBuf,
    need: Need,
    before: Option<String>,
    after: String,
) -> Action {
    let lines = diff(before.as_deref().unwrap_or(""), &after);
    let count = |k| lines.iter().filter(|l: &&DiffLine| l.kind == k).count();
    let (added, removed) = (count(LineKind::Add), count(LineKind::Del));
    Action {
        verb,
        target: show(ctx, &path),
        detail: Detail::Diff { added, removed, new: before.is_none(), lines },
        need,
        job: Job::Put { path, before, after },
    }
}

/// Quita «  12⇥» del principio de cada línea, si todas lo llevan.
fn strip_numbers(s: &str) -> Option<String> {
    let mut out = Vec::new();
    for l in s.split('\n') {
        if l.is_empty() {
            out.push(l);
            continue;
        }
        let (n, rest) = l.split_once('\t')?;
        if n.trim().is_empty() || !n.trim().bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        out.push(rest);
    }
    Some(out.join("\n"))
}

fn replace(text: &str, old: &str, new: &str, all: bool) -> Result<String, String> {
    if old.is_empty() {
        return Err("`old` is empty. To create a file use write.".into());
    }
    // Archivos con saltos de Windows: el modelo escribe \n.
    let crlf = text.contains("\r\n") && !old.contains("\r\n");
    let fix = |s: &str| if crlf { s.replace('\n', "\r\n") } else { s.to_string() };
    let mut pairs = vec![(fix(old), fix(new))];
    // Los modelos pequeños copian a veces los números de línea.
    if let Some(o) = strip_numbers(old) {
        pairs.push((fix(&o), fix(&strip_numbers(new).unwrap_or(new.to_string()))));
    }
    for (old, new) in &pairs {
        if old.is_empty() {
            continue;
        }
        match text.matches(old.as_str()).count() {
            0 => {}
            n if n > 1 && !all => {
                return Err(format!(
                    "`old` appears {n} times. Include more surrounding lines, or set all to true."
                ));
            }
            _ if old == new => return Err("`old` and `new` are the same.".into()),
            _ if all => return Ok(text.replace(old.as_str(), new)),
            _ => return Ok(text.replacen(old.as_str(), new, 1)),
        }
    }
    Err("`old` was not found in the file. Read the file again and copy the text exactly, with its indentation.".into())
}

/// Diff por líneas con tres de contexto.
pub fn diff(before: &str, after: &str) -> Vec<DiffLine> {
    let d = similar::TextDiff::configure().timeout(Duration::from_millis(400)).diff_lines(before, after);
    let mut out = Vec::new();
    for (i, group) in d.grouped_ops(3).iter().enumerate() {
        if i > 0 {
            out.push(DiffLine { kind: LineKind::Gap, text: String::new() });
        }
        for op in group {
            for c in d.iter_changes(op) {
                let kind = match c.tag() {
                    similar::ChangeTag::Equal => LineKind::Same,
                    similar::ChangeTag::Insert => LineKind::Add,
                    similar::ChangeTag::Delete => LineKind::Del,
                };
                out.push(DiffLine { kind, text: c.value().trim_end_matches(['\n', '\r']).to_string() });
            }
        }
    }
    out
}

// ---------- ejecutar ----------

fn fail(text: impl Into<String>, summary: &str) -> Outcome {
    Outcome { ok: false, text: text.into(), summary: summary.into() }
}

fn cut_chars(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

/// Principio y final de un texto largo.
pub(crate) fn middle_out(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let head = cut_chars(s, max / 4);
    let mut from = s.len() - (max - max / 4);
    while !s.is_char_boundary(from) {
        from += 1;
    }
    format!("{head}\n[... {} characters omitted ...]\n{}", from - head.len(), &s[from..])
}

fn walker(path: &Path) -> ignore::WalkBuilder {
    let mut w = ignore::WalkBuilder::new(path);
    w.hidden(false).follow_links(false).require_git(false).filter_entry(|e| e.file_name() != ".git");
    w
}

/// Ejecuta una acción ya permitida.
pub fn execute(
    ctx: &Ctx,
    action: &Action,
    cancelled: &dyn Fn() -> bool,
    on: &mut dyn FnMut(&str),
) -> Outcome {
    match &action.job {
        Job::Know(j) => know::run(j),
        Job::Read { path, offset, limit } => match read_text(path) {
            Err(e) => fail(e, "no se pudo leer"),
            Ok(t) => {
                let total = t.lines().count();
                let mut out = String::new();
                let mut shown = 0;
                for (i, l) in t.lines().enumerate().skip(offset - 1).take(*limit) {
                    let line = format!("{}\t{}\n", i + 1, cut_chars(l, 2000));
                    if shown > 0 && out.len() + line.len() > ctx.max_chars {
                        break;
                    }
                    out.push_str(&line);
                    shown += 1;
                }
                if total == 0 {
                    out.push_str("(empty file)");
                } else if shown == 0 {
                    out.push_str(&format!("(the file only has {total} lines)"));
                } else if offset - 1 + shown < total {
                    out.push_str(&format!(
                        "[{total} lines in total; continue with offset {}]",
                        offset + shown
                    ));
                }
                let s = if shown == 1 { "1 línea".into() } else { format!("{shown} líneas") };
                Outcome { ok: true, text: out, summary: s }
            }
        },
        Job::List { path, depth } => {
            if !path.is_dir() {
                return fail("It is not a directory.", "no es una carpeta");
            }
            let (mut out, mut n) = (String::new(), 0);
            let mut w = walker(path);
            w.max_depth(Some(*depth)).sort_by_file_name(|a, b| a.cmp(b));
            for e in w.build().flatten().skip(1) {
                if cancelled() || n >= 400 || out.len() > ctx.max_chars {
                    out.push_str("[... more entries]\n");
                    break;
                }
                let dir = e.file_type().is_some_and(|t| t.is_dir());
                let name = e.file_name().to_string_lossy().into_owned();
                let pad = "  ".repeat(e.depth().saturating_sub(1));
                out.push_str(&format!("{pad}{name}{}\n", if dir { "/" } else { "" }));
                n += 1;
            }
            if n == 0 {
                out.push_str("(empty)");
            }
            Outcome { ok: true, text: out, summary: format!("{n} elementos") }
        }
        Job::Search { path, pattern, glob } => search(ctx, path, pattern, glob.as_deref(), cancelled),
        Job::Find { pattern } => {
            let mut ov = ignore::overrides::OverrideBuilder::new(&ctx.root);
            let built =
                ov.add(pattern).map_err(|e| e.to_string()).and_then(|o| o.build().map_err(|e| e.to_string()));
            let ov = match built {
                Ok(o) => o,
                Err(e) => return fail(format!("Invalid glob: {e}"), "patrón no válido"),
            };
            let mut found = Vec::new();
            for e in walker(&ctx.root).build().flatten() {
                if cancelled() || found.len() >= 300 {
                    break;
                }
                if e.file_type().is_some_and(|t| t.is_file()) && ov.matched(e.path(), false).is_whitelist() {
                    found.push(show(ctx, e.path()));
                }
            }
            found.sort();
            let summary = match found.len() {
                0 => "ninguno".into(),
                1 => "1 archivo".into(),
                n => format!("{n} archivos"),
            };
            let text = if found.is_empty() { "No files found.".into() } else { found.join("\n") };
            Outcome { ok: true, text: cut_chars(&text, ctx.max_chars).into(), summary }
        }
        Job::Put { path, before, after } => {
            // Entre que se enseñó el diff y se aceptó pudo cambiar.
            let now = if path.exists() { read_text(path).ok() } else { None };
            if now != *before || resolve(ctx, &path.to_string_lossy()).as_ref() != Ok(path) {
                return fail(
                    "The file changed on disk in the meantime. Read it again and retry.",
                    "había cambiado",
                );
            }
            if let Some(p) = path.parent()
                && let Err(e) = std::fs::create_dir_all(p)
            {
                return fail(format!("Cannot create the directory: {e}"), "no se pudo escribir");
            }
            match std::fs::write(path, after) {
                Ok(()) => Outcome { ok: true, text: "Done.".into(), summary: String::new() },
                Err(e) => fail(format!("Cannot write it: {e}"), "no se pudo escribir"),
            }
        }
        Job::Run { cmd, sandboxed, timeout } => {
            let spec = exec::Spec {
                cmd,
                dir: &ctx.root,
                project: &ctx.project,
                sandboxed: *sandboxed,
                network: false,
                timeout: *timeout,
                env: &[("PAGER", "cat"), ("GIT_PAGER", "cat")],
            };
            let out = match exec::run(&spec, cancelled, on) {
                Ok(o) => o,
                Err(e) => return fail(format!("It could not run: {e}"), "no se pudo ejecutar"),
            };
            let body = middle_out(out.text.trim_end(), ctx.max_chars);
            let body = if body.is_empty() { "(no output)".to_string() } else { body };
            match out.end {
                End::Exit(Some(0)) => Outcome { ok: true, text: body, summary: String::new() },
                End::Exit(code) => {
                    let code = code.map_or("a signal".into(), |c| c.to_string());
                    let low = body.to_lowercase();
                    let blocked = [
                        "not permitted",
                        "permission denied",
                        "resolve host",
                        "network is unreachable",
                        "read-only",
                    ];
                    let hint = if *sandboxed && blocked.iter().any(|b| low.contains(b)) {
                        "\n(If it failed only because the sandbox blocks the network or writing outside the project, retry with unsandboxed.)"
                    } else {
                        ""
                    };
                    let summary = if code == "a signal" {
                        "lo mató una señal".into()
                    } else {
                        format!("salió con {code}")
                    };
                    Outcome { ok: false, text: format!("{body}\n[exit code: {code}]{hint}"), summary }
                }
                End::Timeout => fail(
                    format!("{body}\n[stopped after {} s]", timeout.as_secs()),
                    &format!("parado a los {} s", timeout.as_secs()),
                ),
                End::Cancelled => fail(format!("{body}\n[stopped by the user]"), "parado"),
            }
        }
    }
}

fn search(
    ctx: &Ctx,
    path: &Path,
    pattern: &str,
    glob: Option<&str>,
    cancelled: &dyn Fn() -> bool,
) -> Outcome {
    let re = regex::Regex::new(pattern).or_else(|_| regex::Regex::new(&regex::escape(pattern)));
    let Ok(re) = re else { return fail("Invalid regular expression.", "expresión no válida") };
    let mut w = walker(path);
    if let Some(g) = glob {
        let mut ov = ignore::overrides::OverrideBuilder::new(path);
        match ov.add(g).and_then(|o| o.build()) {
            Ok(o) => {
                w.overrides(o);
            }
            Err(e) => return fail(format!("Invalid glob: {e}"), "patrón no válido"),
        }
    }
    let (mut out, mut hits, mut files) = (String::new(), 0, 0);
    'files: for e in w.build().flatten() {
        if cancelled() {
            break;
        }
        let p = e.path();
        if !e.file_type().is_some_and(|t| t.is_file()) || (looks_secret(p) && p != path) {
            continue;
        }
        let Ok(t) = read_text(p) else { continue };
        let mut any = false;
        for (i, l) in t.lines().enumerate().filter(|(_, l)| re.is_match(l)) {
            if hits >= MAX_HITS || out.len() > ctx.max_chars {
                out.push_str("[... more matches: narrow the search]\n");
                break 'files;
            }
            if !any {
                any = true;
                files += 1;
            }
            hits += 1;
            out.push_str(&format!("{}:{}: {}\n", show(ctx, p), i + 1, cut_chars(l.trim_end(), 300)));
        }
    }
    if hits == 0 {
        return Outcome { ok: true, text: "No matches.".into(), summary: "nada".into() };
    }
    let c = if hits == 1 { "1 resultado".into() } else { format!("{hits} resultados") };
    let f = if files == 1 { "1 archivo".into() } else { format!("{files} archivos") };
    Outcome { ok: true, text: out, summary: format!("{c} en {f}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn setup(name: &str) -> (Tmp, Ctx) {
        let base = std::env::temp_dir().join(format!("forge-tools-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("p/src")).unwrap();
        std::fs::write(base.join("p/src/a.rs"), "fn uno() {}\nfn dos() {}\n").unwrap();
        std::fs::write(base.join("p/.gitignore"), "target\n").unwrap();
        std::fs::write(base.join("p/.env"), "CLAVE=1\n").unwrap();
        std::fs::write(base.join("fuera.txt"), "secreto\n").unwrap();
        let ctx = Ctx::new(&base.join("p"), 20_000).unwrap();
        (Tmp(base), ctx)
    }

    fn call(name: &str, args: Value) -> Call {
        Call { id: "1".into(), name: name.into(), args: args.to_string() }
    }

    fn go(ctx: &Ctx, name: &str, args: Value) -> Outcome {
        let a = prepare(ctx, &call(name, args)).unwrap();
        execute(ctx, &a, &|| false, &mut |_| {})
    }

    #[test]
    fn read_list_search_find() {
        let (_t, ctx) = setup("leer");
        assert_eq!(go(&ctx, "read", json!({"path": "src/a.rs"})).text, "1\tfn uno() {}\n2\tfn dos() {}\n");
        let r = go(&ctx, "read", json!({"file_path": "src/a.rs", "limit": 1}));
        assert!(r.text.contains("continue with offset 2"), "{}", r.text);
        assert!(go(&ctx, "list", json!({})).text.contains("src/\n  a.rs"));
        assert_eq!(go(&ctx, "search", json!({"pattern": "fn d.s"})).text, "src/a.rs:2: fn dos() {}\n");
        assert_eq!(go(&ctx, "search", json!({"pattern": "CLAVE"})).text, "No matches.");
        assert_eq!(go(&ctx, "find", json!({"pattern": "**/*.rs"})).text, "src/a.rs");
    }

    #[test]
    fn edits_show_a_diff_and_detect_races() {
        let (_t, ctx) = setup("editar");
        let c = call("edit", json!({"path": "src/a.rs", "old": "2\tfn dos() {}", "new": "2\tfn tres() {}"}));
        let a = prepare(&ctx, &c).unwrap();
        assert_eq!(a.need, Need::Normal);
        let Detail::Diff { added, removed, new, .. } = &a.detail else { panic!() };
        assert_eq!((*added, *removed, *new), (1, 1, false));
        assert!(execute(&ctx, &a, &|| false, &mut |_| {}).ok);
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("src/a.rs")).unwrap(),
            "fn uno() {}\nfn tres() {}\n"
        );
        // La misma acción otra vez: el archivo ya no es el que se enseñó.
        assert!(!execute(&ctx, &a, &|| false, &mut |_| {}).ok);
        let twice = call("edit", json!({"path": "src/a.rs", "old": "fn ", "new": "pub fn "}));
        assert!(prepare(&ctx, &twice).unwrap_err().contains("2 times"));
        assert!(go(&ctx, "write", json!({"path": "nuevo/b.txt", "content": "x"})).ok);
        assert!(ctx.root.join("nuevo/b.txt").exists());
    }

    #[test]
    fn crlf_is_kept() {
        assert_eq!(replace("a\r\nb\r\n", "a\nb", "c\nd", false).unwrap(), "c\r\nd\r\n");
    }

    #[test]
    fn permissions() {
        let (t, ctx) = setup("permisos");
        let need = |name: &str, args: Value| prepare(&ctx, &call(name, args)).map(|a| a.need);
        let always = |n: Result<Need, String>| matches!(n, Ok(Need::Always(_)));
        assert_eq!(need("read", json!({"path": "src/../src/a.rs"})), Ok(Need::Free));
        assert!(always(need("read", json!({"path": "../fuera.txt"}))));
        assert!(always(need("read", json!({"path": ".env"}))));
        assert!(always(need("write", json!({"path": "../x", "content": ""}))));
        assert!(always(need("write", json!({"path": "sub/.CLAUDE/settings.json", "content": ""}))));
        assert!(need("write", json!({"path": ".Git/hooks/x", "content": ""})).is_err());
        assert!(always(need("bash", json!({"command": "ls", "unsandboxed": true}))));
        assert_eq!(need("bash", json!({"command": "ls"})), Ok(Need::Normal));
        // Enlaces que salen del proyecto.
        std::os::unix::fs::symlink(&t.0, ctx.root.join("enlace")).unwrap();
        assert!(always(need("read", json!({"path": "enlace/fuera.txt"}))));
        assert!(always(need("write", json!({"path": "enlace/otro.txt", "content": ""}))));
        std::os::unix::fs::symlink(t.0.join("no-existe"), ctx.root.join("roto")).unwrap();
        assert!(need("write", json!({"path": "roto", "content": ""})).is_err());
        assert!(need("write", json!({"path": "no/existe/../../../x", "content": ""})).is_err());
        assert!(need("volar", json!({})).is_err());
        assert!(prepare(&ctx, &Call { id: "".into(), name: "read".into(), args: "{mal".into() }).is_err());
    }

    #[test]
    fn home_is_not_a_project() {
        assert!(Ctx::new(&dirs::home_dir().unwrap(), 100).is_none());
        assert!(Ctx::new(Path::new("/"), 100).is_none());
        assert!(Ctx::new(Path::new("/no/existe"), 100).is_none());
    }

    #[test]
    fn commands_run_sandboxed() {
        if std::env::var_os("FORGE_VERIFY").is_some() {
            return;
        }
        let (t, ctx) = setup("orden");
        assert_eq!(go(&ctx, "bash", json!({"command": "echo hola > f; cat f"})).text, "hola");
        let r = go(&ctx, "bash", json!({"command": "echo x > ../fuera.txt"}));
        assert!(!r.ok && r.text.contains("exit code"), "{}", r.text);
        assert_eq!(std::fs::read_to_string(t.0.join("fuera.txt")).unwrap(), "secreto\n");
    }

    #[test]
    fn long_output_keeps_both_ends() {
        let s = format!("INICIO{}FIN", "ñ".repeat(5000));
        let m = middle_out(&s, 1000);
        assert!(m.starts_with("INICIO") && m.ends_with("FIN") && m.len() < 1100);
    }
}

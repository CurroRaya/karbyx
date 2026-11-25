// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! Cliente MCP: servidores de herramientas que se lanzan en el Mac y hablan
//! JSON-RPC por su entrada y salida.
//!
//! Un servidor MCP es un programa del usuario que se ejecuta **fuera** del
//! sandbox: solo se arranca lo que el usuario añadió y aprobó tal cual (la
//! huella de su configuración), y cada llamada se pregunta.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::api::Call;
use crate::exec;
use crate::remote;
use crate::tools::{Action, Detail, Need, Outcome, middle_out};
use crate::web;

/// Versión del protocolo que se pide.
pub(crate) const PROTOCOL: &str = "2025-06-18";
const START_TIMEOUT: Duration = Duration::from_secs(60);
const CALL_TIMEOUT: Duration = Duration::from_secs(300);
/// Una línea más larga no es razonable.
const LINE_MAX: usize = 16 << 20;
const TOOLS_MAX: usize = 200;

// ---------- configuración ----------

/// Cómo se lanza un servidor (formato habitual de `mcpServers`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub command: String,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Servidor remoto: su dirección y las cabeceras que se le mandan.
    #[serde(skip_serializing_if = "Option::is_none", alias = "serverUrl", alias = "httpUrl")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

impl Config {
    /// La orden, para enseñarla.
    pub fn line(&self) -> String {
        if let Some(u) = &self.url {
            return u.clone();
        }
        let quote = |s: &str| {
            if !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || "-_./@:=+,%".contains(c)) {
                s.to_string()
            } else {
                format!("'{}'", s.replace('\'', r"'\''"))
            }
        };
        std::iter::once(&self.command).chain(&self.args).map(|s| quote(s)).collect::<Vec<_>>().join(" ")
    }

    /// Dónde está, si es remoto.
    pub fn host(&self) -> Option<String> {
        Some(web::host(self.url.as_deref()?))
    }

    /// Huella de lo que se aprueba: si cambia, hay que aprobarlo otra vez.
    pub fn fingerprint(&self) -> String {
        let text = serde_json::to_string(self).unwrap_or_default();
        format!("{:016x}-{}", exec::fnv(text.as_bytes()), text.len())
    }
}

/// Entiende lo que pega el usuario: el JSON de `mcpServers` (o una parte) o
/// una orden suelta.
pub fn parse(text: &str) -> Result<Vec<(String, Config)>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if !text.starts_with('{') {
        let words = split(text)?;
        let (command, args) = words.split_first().ok_or("Falta la orden")?;
        if command.starts_with("http://") || command.starts_with("https://") {
            if !args.is_empty() {
                return Err("Pon solo la dirección".into());
            }
            let c = Config { url: Some(command.clone()), ..Config::default() };
            return Ok(vec![(remote_name(command)?, check(c)?)]);
        }
        // El nombre sale del paquete: `@org/server-github` → `github`.
        let pkg = args.iter().find(|a| !a.starts_with('-')).unwrap_or(command);
        let base = pkg.rsplit('/').next().unwrap_or(pkg).split('@').next().unwrap_or(pkg);
        let name = base
            .trim_start_matches("server-")
            .trim_start_matches("mcp-server-")
            .trim_start_matches("mcp-")
            .trim_end_matches("-mcp")
            .trim_end_matches(".py")
            .trim_end_matches(".js");
        let c = Config { command: command.clone(), args: args.to_vec(), ..Config::default() };
        return Ok(vec![(name_ok(name)?, c)]);
    }
    let v: Value = serde_json::from_str(text).map_err(|e| format!("El JSON no es válido: {e}"))?;
    let map = v.get("mcpServers").or_else(|| v.get("servers")).unwrap_or(&v);
    if ["command", "url", "serverUrl", "httpUrl"].iter().any(|k| map.get(*k).is_some_and(Value::is_string)) {
        let c: Config = serde_json::from_value(map.clone()).map_err(|e| e.to_string())?;
        let c = check(c)?;
        let name = match (v.get("name").and_then(Value::as_str), &c.url) {
            (Some(n), _) => name_ok(n)?,
            (None, Some(u)) => remote_name(u)?,
            (None, None) => name_ok(c.command.rsplit('/').next().unwrap_or(&c.command))?,
        };
        return Ok(vec![(name, c)]);
    }
    let map = map.as_object().filter(|m| !m.is_empty()).ok_or("No hay ningún servidor en ese JSON")?;
    let mut out = Vec::new();
    for (name, c) in map {
        let c: Config = serde_json::from_value(c.clone()).map_err(|e| format!("{name}: {e}"))?;
        out.push((name_ok(name)?, check(c).map_err(|e| format!("{name}: {e}"))?));
    }
    Ok(out)
}

/// Que sea lanzable o alcanzable.
fn check(mut c: Config) -> Result<Config, String> {
    match c.url.as_mut().map(|u| u.trim().to_string()) {
        Some(u) => {
            web::check(&u)?;
            let bad = |s: &str| s.chars().any(|c| c.is_control());
            let name =
                |k: &str| !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b));
            if !c.headers.iter().all(|(k, v)| name(k) && !bad(v)) {
                return Err("hay una cabecera no válida".into());
            }
            // Lo demás no se usa: que no se apruebe sin verse.
            Ok(Config { url: Some(u), headers: c.headers, ..Config::default() })
        }
        None if c.command.trim().is_empty() => Err("falta «command» o «url»".into()),
        None => Ok(Config { headers: BTreeMap::new(), ..c }),
    }
}

/// `https://mcp.linear.app/mcp` → `linear`.
fn remote_name(url: &str) -> Result<String, String> {
    let host = web::host(url);
    let host = host.split(':').next().unwrap_or(&host);
    if host.parse::<std::net::IpAddr>().is_ok() {
        return name_ok("remoto");
    }
    let parts: Vec<&str> = host.split('.').filter(|p| !matches!(*p, "mcp" | "www" | "api")).collect();
    name_ok(match parts.as_slice() {
        [] => "remoto",
        [one] => one,
        [.., n, _] => n,
    })
}

/// Nombre usable como parte del de una herramienta.
pub(crate) fn name_ok(n: &str) -> Result<String, String> {
    let s: String = n
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let s: String = s.trim_matches('-').chars().take(24).collect();
    if s.is_empty() { Err("No sé qué nombre ponerle: usa el formato JSON".into()) } else { Ok(s) }
}

/// Parte una orden en palabras, con comillas.
fn split(s: &str) -> Result<Vec<String>, String> {
    let (mut out, mut cur, mut any) = (Vec::new(), String::new(), false);
    let mut quote = None;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match (quote, c) {
            (None, '\'' | '"') => (quote, any) = (Some(c), true),
            (Some(q), c) if c == q => quote = None,
            (None | Some('"'), '\\') => {
                cur.extend(it.next());
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            (None, '|' | ';' | '&' | '<' | '>' | '$' | '`') => {
                return Err("Pon solo la orden y sus argumentos, sin más cosas de la shell".into());
            }
            (_, c) => {
                cur.push(c);
                any = true;
            }
        }
    }
    if quote.is_some() {
        return Err("Hay unas comillas sin cerrar".into());
    }
    if any {
        out.push(cur);
    }
    Ok(out)
}

/// Un servidor que esta conversación puede usar.
#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    pub name: String,
    pub config: Config,
    /// Herramientas que no se le ofrecen al modelo.
    pub off: BTreeSet<String>,
    /// Herramientas que se usan sin preguntar.
    pub trusted: BTreeSet<String>,
}

// ---------- proceso ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

impl Tool {
    /// Lo que se le manda al modelo.
    pub fn definition(&self, server: &str) -> Value {
        json!({"type": "function", "function": {
            "name": full_name(server, &self.name),
            "description": self.description.chars().take(600).collect::<String>(),
            "parameters": simplify(&self.schema, 0),
        }})
    }

    /// Tokens aproximados que ocupa.
    pub fn cost(&self, server: &str) -> usize {
        self.definition(server).to_string().len() / 3
    }
}

/// `servidor__herramienta`, con los caracteres y el largo que admiten todos.
pub fn full_name(server: &str, tool: &str) -> String {
    let t: String =
        tool.chars().map(|c| if c.is_ascii_alphanumeric() || "_-".contains(c) { c } else { '_' }).collect();
    let mut s = format!("{server}__{t}");
    if s.len() > 64 {
        let h = format!("{:08x}", exec::fnv(tool.as_bytes()) as u32);
        s.truncate(64 - h.len() - 1);
        s.push_str(&format!("_{h}"));
    }
    s
}

/// Esquema reducido a lo que los servidores de modelos convierten en
/// gramática sin fallar.
fn simplify(v: &Value, depth: usize) -> Value {
    let any = || json!({"type": "object"});
    let Some(m) = v.as_object() else { return if depth == 0 { any() } else { json!({}) } };
    if depth > 6 {
        return json!({});
    }
    let mut out = serde_json::Map::new();
    for k in ["type", "description", "enum", "required", "minimum", "maximum"] {
        if let Some(x) = m.get(k) {
            out.insert(k.into(), x.clone());
        }
    }
    // `["string", "null"]`: opcional.
    if let Some(list) = out.get("type").and_then(Value::as_array) {
        let t = list.iter().find(|t| *t != "null").cloned().unwrap_or(json!("string"));
        out.insert("type".into(), t);
    }
    if let Some(Value::String(d)) = out.get_mut("description") {
        *d = d.chars().take(300).collect();
    }
    if let Some(p) = m.get("properties").and_then(Value::as_object) {
        let props: serde_json::Map<_, _> =
            p.iter().map(|(k, x)| (k.clone(), simplify(x, depth + 1))).collect();
        // Lo obligatorio tiene que existir.
        if let Some(r) = out.get_mut("required").and_then(Value::as_array_mut) {
            r.retain(|n| n.as_str().is_some_and(|n| props.contains_key(n)));
        }
        out.insert("properties".into(), props.into());
    }
    if let Some(i) = m.get("items") {
        out.insert("items".into(), simplify(i, depth + 1));
    }
    for k in ["anyOf", "oneOf"] {
        if let Some(list) = m.get(k).and_then(Value::as_array) {
            // La primera alternativa que no sea «nada».
            let pick = list.iter().find(|x| x.get("type").is_none_or(|t| t != "null"));
            if let Some(Value::Object(p)) = pick.map(|p| simplify(p, depth + 1)) {
                for (a, b) in p {
                    out.entry(a).or_insert(b);
                }
            }
        }
    }
    if depth == 0 {
        out.insert("type".into(), json!("object"));
        out.entry("properties").or_insert(json!({}));
    }
    Value::Object(out)
}

type Waiting = Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>;

/// Un servidor en marcha (o al otro lado de la red).
pub struct Client {
    pub tools: Vec<Tool>,
    link: Link,
}

enum Link {
    Local(Local),
    Remote(remote::Remote),
}

/// Proceso en este Mac.
struct Local {
    child: Mutex<Child>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    waiting: Arc<Waiting>,
    next: AtomicU64,
    alive: Arc<AtomicBool>,
    errors: Arc<Mutex<String>>,
}

impl Drop for Local {
    fn drop(&mut self) {
        // Al cerrar su entrada, un servidor educado termina.
        self.stdin.lock().unwrap_or_else(|e| e.into_inner()).take();
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(400) && matches!(child.try_wait(), Ok(None)) {
            std::thread::sleep(Duration::from_millis(20));
        }
        // Y con él, lo que haya lanzado (npx lanza node...).
        // SAFETY: kill(2) con el grupo de un proceso hijo nuestro.
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.wait();
    }
}

/// `PATH` de las terminales del usuario: una app del Dock no lo hereda.
fn user_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        const MARK: &str = "__KARBYX_PATH__";
        let out = Command::new(exec::shell())
            .args(["-l", "-i", "-c", &format!("printf '\\n{MARK}%s\\n' \"$PATH\"")])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        text.lines()
            .find_map(|l| l.strip_prefix(MARK))
            .map(str::to_string)
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_else(|| "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin".into())
    })
}

/// ¿Está instalado un programa, en el `PATH` del usuario?
pub fn installed(program: &str) -> bool {
    user_path().split(':').any(|d| !d.is_empty() && Path::new(d).join(program).is_file())
}

impl Client {
    /// Arranca (o conecta con) el servidor y le pregunta qué herramientas tiene.
    pub fn start(config: &Config, cwd: &Path, cancelled: &dyn Fn() -> bool) -> Result<Self, String> {
        let link = match &config.url {
            Some(u) => Link::Remote(remote::Remote::new(u, &config.headers)?),
            None => Link::Local(Local::start(config, cwd)?),
        };
        let mut c = Self { tools: Vec::new(), link };
        c.handshake(cancelled)?;
        Ok(c)
    }

    fn alive(&self) -> bool {
        match &self.link {
            Link::Local(l) => l.alive(),
            Link::Remote(_) => true,
        }
    }

    fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        match &self.link {
            Link::Local(l) => l.request(method, params, timeout, cancelled),
            Link::Remote(r) => r.request(method, params, timeout, cancelled),
        }
    }

    fn notify(&self, method: &str) -> Result<(), String> {
        let v = json!({"jsonrpc": "2.0", "method": method});
        match &self.link {
            Link::Local(l) => l.send(&v),
            Link::Remote(r) => r.notify(&v),
        }
    }

    fn handshake(&mut self, cancelled: &dyn Fn() -> bool) -> Result<(), String> {
        self.request("initialize", hello(), START_TIMEOUT, cancelled)?;
        self.notify("notifications/initialized")?;
        let mut cursor: Option<String> = None;
        // Paginado, con un tope por si el servidor no acaba nunca.
        for _ in 0..20 {
            let params = cursor.as_ref().map_or(json!({}), |c| json!({"cursor": c}));
            let r = self.request("tools/list", params, START_TIMEOUT, cancelled)?;
            for t in r.get("tools").and_then(Value::as_array).into_iter().flatten() {
                let Some(name) = t.get("name").and_then(Value::as_str) else { continue };
                if self.tools.len() < TOOLS_MAX && !self.tools.iter().any(|x| x.name == name) {
                    self.tools.push(Tool {
                        name: name.into(),
                        description: t.get("description").and_then(Value::as_str).unwrap_or_default().into(),
                        schema: t.get("inputSchema").cloned().unwrap_or_default(),
                    });
                }
            }
            cursor =
                r.get("nextCursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(())
    }

    /// Llama a una herramienta. (bien, texto)
    pub(crate) fn call(
        &self,
        tool: &str,
        args: &Value,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(bool, String), String> {
        let r =
            self.request("tools/call", json!({"name": tool, "arguments": args}), CALL_TIMEOUT, cancelled)?;
        let ok = !r.get("isError").and_then(Value::as_bool).unwrap_or(false);
        let mut out: Vec<String> = Vec::new();
        for c in r.get("content").and_then(Value::as_array).into_iter().flatten() {
            let text = |p: &str| c.pointer(p).and_then(Value::as_str).map(str::to_string);
            match c.get("type").and_then(Value::as_str) {
                Some("text") => out.extend(text("/text")),
                Some("resource") => {
                    out.extend(text("/resource/text").or_else(|| text("/resource/uri")));
                }
                Some("resource_link") => out.extend(text("/uri")),
                Some(t) => out.push(format!("[{t} omitted]")),
                None => {}
            }
        }
        if out.is_empty()
            && let Some(s) = r.get("structuredContent")
        {
            out.push(s.to_string());
        }
        Ok((ok, out.join("\n")))
    }
}

/// Presentación.
pub(crate) fn hello() -> Value {
    json!({"protocolVersion": PROTOCOL, "capabilities": {},
        "clientInfo": {"name": "karbyx", "version": env!("CARGO_PKG_VERSION")}})
}

impl Local {
    fn start(config: &Config, cwd: &Path) -> Result<Self, String> {
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .current_dir(cwd)
            .env("PATH", user_path())
            .env_remove("KARBYX_SOCKET")
            .env_remove("KARBYX_PANE")
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => format!("no encuentro «{}»", config.command),
                _ => e.to_string(),
            })?;
        let stdin = Arc::new(Mutex::new(child.stdin.take()));
        let waiting: Arc<Waiting> = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let errors: Arc<Mutex<String>> = Arc::default();
        if let Some(err) = child.stderr.take() {
            let errors = errors.clone();
            std::thread::spawn(move || keep_tail(err, &errors));
        }
        if let Some(out) = child.stdout.take() {
            let (stdin, waiting, alive) = (stdin.clone(), waiting.clone(), alive.clone());
            std::thread::spawn(move || {
                listen(out, &stdin, &waiting);
                alive.store(false, Ordering::Release);
                // Nadie más va a contestar.
                waiting.lock().unwrap_or_else(|e| e.into_inner()).clear();
            });
        }
        Ok(Self { child: Mutex::new(child), stdin, waiting, next: AtomicU64::new(1), alive, errors })
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    fn send(&self, v: &Value) -> Result<(), String> {
        write_line(&self.stdin, v).map_err(|_| self.died())
    }

    /// Por qué se acabó, con lo último que dijo.
    fn died(&self) -> String {
        std::thread::sleep(Duration::from_millis(60));
        let e = self.errors.lock().unwrap_or_else(|e| e.into_inner());
        match e.lines().map(str::trim).rfind(|l| !l.is_empty()) {
            Some(l) => format!("se ha cerrado: {}", l.chars().take(200).collect::<String>()),
            None => "se ha cerrado".into(),
        }
    }

    fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.waiting.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        let forget = || {
            self.waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        };
        // Si se cerró antes de apuntar la espera, nadie la quitaría.
        if !self.alive() {
            forget();
            return Err(self.died());
        }
        if let Err(e) = self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})) {
            forget();
            return Err(e);
        }
        let start = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(60)) {
                Ok(r) => return r,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(self.died()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let stop = cancelled();
            if stop || start.elapsed() > timeout {
                forget();
                let why = if stop { "user" } else { "timeout" };
                let _ = self.send(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                    "params": {"requestId": id, "reason": why}}));
                return Err(if stop { "parado".into() } else { "tarda demasiado".into() });
            }
        }
    }
}

fn write_line(stdin: &Mutex<Option<ChildStdin>>, v: &Value) -> std::io::Result<()> {
    let mut g = stdin.lock().unwrap_or_else(|e| e.into_inner());
    let w = g.as_mut().ok_or(std::io::ErrorKind::BrokenPipe)?;
    w.write_all(format!("{v}\n").as_bytes())?;
    w.flush()
}

/// Guarda el final de lo que dice por su salida de error.
fn keep_tail(mut err: impl Read, keep: &Mutex<String>) {
    let mut b = [0u8; 4096];
    while let Ok(n) = err.read(&mut b) {
        if n == 0 {
            break;
        }
        let mut s = keep.lock().unwrap_or_else(|e| e.into_inner());
        s.push_str(&exec::strip_ansi(&String::from_utf8_lossy(&b[..n])));
        if s.len() > 8192 {
            let mut cut = s.len() - 4096;
            while !s.is_char_boundary(cut) {
                cut += 1;
            }
            s.drain(..cut);
        }
    }
}

/// Reparte lo que llega: respuestas, y peticiones del servidor.
fn listen(out: impl Read, stdin: &Mutex<Option<ChildStdin>>, waiting: &Waiting) {
    let mut r = BufReader::new(out);
    let mut line = Vec::new();
    loop {
        line.clear();
        match r.by_ref().take(LINE_MAX as u64).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(n) if n >= LINE_MAX => return,
            Ok(_) => {}
        }
        // Lo que no sea JSON es ruido del arranque.
        let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
        let id = v.get("id").cloned().filter(|i| !i.is_null());
        match (v.get("method").and_then(Value::as_str), id) {
            (Some("ping"), Some(id)) => {
                let _ = write_line(stdin, &json!({"jsonrpc": "2.0", "id": id, "result": {}}));
            }
            // Karbyx no ofrece nada al servidor (ni su modelo ni sus carpetas).
            (Some(_), Some(id)) => {
                let e = json!({"code": -32601, "message": "Not supported"});
                let _ = write_line(stdin, &json!({"jsonrpc": "2.0", "id": id, "error": e}));
            }
            (Some(_), None) => {}
            (None, Some(id)) => {
                let Some(tx) =
                    id.as_u64().and_then(|i| waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&i))
                else {
                    continue;
                };
                let _ = tx.send(match v.get("error") {
                    Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
                    None => Ok(v.get("result").cloned().unwrap_or_default()),
                });
            }
            (None, None) => {}
        }
    }
}

/// Servidores en marcha, compartidos entre conversaciones de la misma carpeta.
/// Se cierran cuando nadie los usa.
pub fn shared(config: &Config, cwd: &Path, cancelled: &dyn Fn() -> bool) -> Result<Arc<Client>, String> {
    type Key = (String, PathBuf);
    static ALL: Mutex<Option<HashMap<Key, Weak<Client>>>> = Mutex::new(None);
    // Uno remoto no depende de la carpeta.
    let place = if config.url.is_some() { Path::new("/") } else { cwd };
    let key = (config.fingerprint(), place.to_path_buf());
    let find = || {
        let g = ALL.lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref()?.get(&key)?.upgrade().filter(|c| c.alive())
    };
    if let Some(c) = find() {
        return Ok(c);
    }
    let c = Arc::new(Client::start(config, cwd, cancelled)?);
    let mut g = ALL.lock().unwrap_or_else(|e| e.into_inner());
    let all = g.get_or_insert_default();
    all.retain(|_, w| w.strong_count() > 0);
    all.insert(key, Arc::downgrade(&c));
    Ok(c)
}

// ---------- en una conversación ----------

/// Los servidores que usa una conversación, ya en marcha.
#[derive(Default, Clone)]
pub(crate) struct Set {
    list: Vec<(Server, Arc<Client>)>,
}

/// Una llamada ya permitida.
#[derive(Clone)]
pub(crate) struct Job {
    client: Arc<Client>,
    tool: String,
    args: Value,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Job({} {})", self.tool, self.args)
    }
}

impl PartialEq for Job {
    fn eq(&self, o: &Self) -> bool {
        Arc::ptr_eq(&self.client, &o.client) && self.tool == o.tool && self.args == o.args
    }
}

impl Set {
    /// Arranca los servidores. `on` recibe el que se está arrancando; los
    /// que fallan se devuelven aparte.
    pub fn start(
        servers: &[Server],
        cwd: &Path,
        cancelled: &dyn Fn() -> bool,
        on: &mut dyn FnMut(&str),
    ) -> (Self, Vec<String>) {
        let (mut set, mut bad) = (Self::default(), Vec::new());
        for s in servers {
            if cancelled() {
                break;
            }
            on(&s.name);
            match shared(&s.config, cwd, cancelled) {
                Ok(c) => set.list.push((s.clone(), c)),
                Err(e) => bad.push(format!("El servidor MCP «{}» no está disponible: {e}.", s.name)),
            }
        }
        (set, bad)
    }

    pub fn definitions(&self) -> Vec<Value> {
        self.tools().map(|(s, _, t)| t.definition(&s.name)).collect()
    }

    fn tools(&self) -> impl Iterator<Item = (&Server, &Arc<Client>, &Tool)> {
        self.list
            .iter()
            .flat_map(|(s, c)| c.tools.iter().map(move |t| (s, c, t)))
            .filter(|(s, _, t)| !s.off.contains(&t.name))
    }

    /// Pone al día lo que el usuario cambió desde que se arrancaron.
    pub fn update(&mut self, servers: &[Server]) {
        for (s, _) in &mut self.list {
            if let Some(new) = servers.iter().find(|n| n.name == s.name && n.config == s.config) {
                *s = new.clone();
            }
        }
    }

    /// Servidor y herramienta de un nombre completo.
    pub fn find(&self, name: &str) -> Option<(String, String)> {
        self.tools()
            .find(|(s, _, t)| full_name(&s.name, &t.name) == name)
            .map(|(s, _, t)| (s.name.clone(), t.name.clone()))
    }

    /// El usuario ha dicho que esta herramienta no se pregunte más.
    pub fn trust(&mut self, name: &str) {
        let Some((server, tool)) = self.find(name) else { return };
        for (s, _) in self.list.iter_mut().filter(|(s, _)| s.name == server) {
            s.trusted.insert(tool.clone());
        }
    }

    /// Prepara la llamada, si es de alguno de estos servidores.
    pub fn prepare(&self, call: &Call) -> Option<Result<Action, String>> {
        let (s, c, t) = self.tools().find(|(s, _, t)| full_name(&s.name, &t.name) == call.name)?;
        let args: Value = match serde_json::from_str(&call.args) {
            Ok(v @ Value::Object(_)) => v,
            _ => return Some(Err("The arguments are not a valid JSON object.".into())),
        };
        let need = if s.trusted.contains(&t.name) {
            Need::Free
        } else {
            Need::Always(match s.config.host() {
                Some(h) => format!("estos datos salen de tu Mac hacia {h}"),
                None => format!("lo hace el servidor MCP «{}», fuera del sandbox", s.name),
            })
        };
        let shown = serde_json::to_string_pretty(&args).unwrap_or_default();
        let detail =
            if args.as_object().is_some_and(|m| m.is_empty()) { Detail::None } else { Detail::Call(shown) };
        Some(Ok(Action::mcp(
            format!("{} · {}", s.name, t.name),
            detail,
            need,
            Job { client: c.clone(), tool: t.name.clone(), args },
        )))
    }
}

pub(crate) fn run(j: &Job, max: usize, cancelled: &dyn Fn() -> bool) -> Outcome {
    match j.client.call(&j.tool, &j.args, cancelled) {
        Ok((ok, text)) => Outcome {
            ok,
            text: middle_out(if text.is_empty() { "(no output)" } else { &text }, max),
            summary: if ok { String::new() } else { "dio un error".into() },
        },
        Err(e) => Outcome { ok: false, text: format!("The MCP server failed: {e}"), summary: e },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{Needs, skip};

    /// Servidor de prueba: imprime ruido, pagina y contesta.
    pub const SERVER: &str = r#"
import json, sys, os, time
print("arrancando...", flush=True)
print("aviso por stderr", file=sys.stderr, flush=True)
def out(v): print(json.dumps(v), flush=True)
for line in sys.stdin:
    m = json.loads(line)
    i, k, p = m.get("id"), m.get("method"), m.get("params") or {}
    if k == "initialize":
        out({"jsonrpc": "2.0", "id": 900, "method": "sampling/createMessage", "params": {}})
        out({"jsonrpc": "2.0", "id": i, "result": {"protocolVersion": p["protocolVersion"], "capabilities": {"tools": {}}}})
    elif k == "tools/list" and not p.get("cursor"):
        out({"jsonrpc": "2.0", "id": i, "result": {"nextCursor": "2", "tools": [{"name": "suma", "description": "Suma a y b", "inputSchema": {"$schema": "x", "type": "object", "properties": {"a": {"type": ["number", "null"], "format": "raro"}, "b": {"anyOf": [{"type": "null"}, {"type": "number"}]}}, "required": ["a", "fantasma"]}}]}})
    elif k == "tools/list":
        out({"jsonrpc": "2.0", "id": i, "result": {"tools": [{"name": "falla"}, {"name": "duerme"}, {"name": "muere"}, {"name": "entorno"}]}})
    elif k == "tools/call":
        n, a = p["name"], p.get("arguments", {})
        if n == "suma": out({"jsonrpc": "2.0", "id": i, "result": {"content": [{"type": "text", "text": str(a["a"] + a.get("b", 0))}, {"type": "image", "data": ""}]}})
        elif n == "falla": out({"jsonrpc": "2.0", "id": i, "result": {"isError": True, "content": [{"type": "text", "text": "mal"}]}})
        elif n == "entorno": out({"jsonrpc": "2.0", "id": i, "result": {"structuredContent": {"x": os.environ.get("X"), "socket": os.environ.get("KARBYX_SOCKET"), "dir": os.path.basename(os.getcwd())}}})
        elif n == "muere":
            print("me caigo", file=sys.stderr, flush=True); sys.exit(3)
        elif n == "duerme": time.sleep(60)
    elif k and i: out({"jsonrpc": "2.0", "id": i, "error": {"code": -1, "message": "no sé"}})
"#;

    pub fn config() -> Config {
        Config {
            command: "/usr/bin/python3".into(),
            args: vec!["-c".into(), SERVER.into()],
            env: BTreeMap::from([("X".into(), "1".into())]),
            ..Config::default()
        }
    }

    fn server() -> Server {
        Server { name: "prueba".into(), config: config(), off: BTreeSet::new(), trusted: BTreeSet::new() }
    }

    fn call(name: &str, args: Value) -> Call {
        Call { id: "1".into(), name: name.into(), args: args.to_string() }
    }

    #[test]
    fn talks_to_a_server() {
        if skip(&[Needs::Python]) {
            return;
        }
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let (mut set, bad) = Set::start(&[server()], &dir, &|| false, &mut |_| {});
        assert!(bad.is_empty(), "{bad:?}");
        let defs = set.definitions();
        assert_eq!(defs.len(), 5);
        let p = &defs[0]["function"];
        assert_eq!(p["name"], "prueba__suma");
        assert_eq!(p["parameters"]["properties"]["a"], json!({"type": "number"}));
        assert_eq!(p["parameters"]["properties"]["b"], json!({"type": "number"}));
        assert_eq!(p["parameters"]["required"], json!(["a"]));
        let go = |set: &Set, name: &str, args: Value| {
            let a = set.prepare(&call(name, args)).unwrap().unwrap();
            (a.need.clone(), run(a.mcp_job().unwrap(), 1000, &|| false))
        };
        let (need, o) = go(&set, "prueba__suma", json!({"a": 2, "b": 3}));
        assert!(matches!(need, Need::Always(_)), "fuera del sandbox: se pregunta");
        assert_eq!((o.ok, o.text.as_str()), (true, "5\n[image omitted]"));
        assert!(!go(&set, "prueba__falla", json!({})).1.ok);
        let o = go(&set, "prueba__entorno", json!({})).1;
        assert!(o.text.contains(r#""x":"1""#) && o.text.contains(r#""socket":null"#), "{}", o.text);
        // Dos conversaciones, un solo proceso.
        let (other, _) = Set::start(&[server()], &dir, &|| false, &mut |_| {});
        assert!(Arc::ptr_eq(&set.list[0].1, &other.list[0].1));
        set.trust("prueba__suma");
        assert_eq!(go(&set, "prueba__suma", json!({"a": 1})).0, Need::Free);
        // Cancelar no espera a que conteste.
        let a = set.prepare(&call("prueba__duerme", json!({}))).unwrap().unwrap();
        let t = Instant::now();
        let o = run(a.mcp_job().unwrap(), 1000, &|| t.elapsed() > Duration::from_millis(200));
        assert!(!o.ok && t.elapsed() < Duration::from_secs(3));
        // Las que se apagan no existen; lo ajeno tampoco.
        let off = Server { off: BTreeSet::from(["suma".to_string()]), ..server() };
        set.update(&[off]);
        assert!(set.prepare(&call("prueba__suma", json!({"a": 1}))).is_none());
        assert!(set.prepare(&call("bash", json!({}))).is_none());
    }

    #[test]
    fn a_dead_server_says_why() {
        if skip(&[Needs::Python]) {
            return;
        }
        let dir = std::env::temp_dir();
        let c = Client::start(&config(), &dir, &|| false).unwrap();
        let e = c.call("muere", &json!({}), &|| false).unwrap_err();
        assert!(e.contains("me caigo"), "{e}");
        assert!(c.call("suma", &json!({"a": 1}), &|| false).is_err());
        let bad = Config { command: "/no/existe".into(), ..Config::default() };
        assert!(Client::start(&bad, &dir, &|| false).is_err());
        let quits = Config {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), "echo roto >&2".into()],
            ..Config::default()
        };
        assert!(Client::start(&quits, &dir, &|| false).err().unwrap().contains("roto"));
    }

    #[test]
    fn parses_what_people_paste() {
        let names = |t: &str| parse(t).map(|v| v.into_iter().map(|(n, c)| (n, c.line())).collect::<Vec<_>>());
        let one = |n: &str, l: &str| Ok(vec![(n.to_string(), l.to_string())]);
        assert_eq!(
            names("npx -y @modelcontextprotocol/server-github"),
            one("github", "npx -y @modelcontextprotocol/server-github")
        );
        assert_eq!(names("uvx mcp-server-fetch"), one("fetch", "uvx mcp-server-fetch"));
        assert_eq!(names("node 'mi carpeta/srv.js'"), one("srv", "node 'mi carpeta/srv.js'"));
        let j = r#"{"mcpServers": {"Mi Server": {"command": "npx", "args": ["-y", "x"], "env": {"K": "v"}}, "b": {"command": "b"}}}"#;
        let v = parse(j).unwrap();
        assert_eq!((v[0].0.as_str(), v[1].0.as_str()), ("mi-server", "b"));
        assert_eq!(parse(r#"{"command": "/usr/bin/cosa"}"#).unwrap()[0].0, "cosa");
        for bad in [
            "{mal",
            "{}",
            "http://a.com/mcp",
            "https://a.com/mcp otra",
            "https://usuario:clave@a.com",
            r#"{"mcpServers": {"x": {"url": "ftp://a"}}}"#,
            r#"{"url": "https://a.com", "headers": {"X": "a\r\nB: c"}}"#,
            "npx x; rm -rf y",
            "node 'sin cerrar",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert_ne!(v[0].1.fingerprint(), v[1].1.fingerprint());
        // Remotos.
        assert_eq!(names("https://mcp.linear.app/mcp"), one("linear", "https://mcp.linear.app/mcp"));
        assert_eq!(names("http://192.168.1.9:3000/mcp"), one("remoto", "http://192.168.1.9:3000/mcp"));
        let j = r#"{"mcpServers": {"gh": {"type": "http", "url": "https://api.github.com/mcp", "headers": {"Authorization": "Bearer x"}, "command": "malo"}}}"#;
        let c = &parse(j).unwrap()[0].1;
        assert_eq!((c.headers.len(), c.command.as_str()), (1, ""), "lo que no se ve no se guarda");
        // Añadir campos no invalida lo ya aprobado.
        let old = Config { command: "a".into(), ..Config::default() };
        assert_eq!(serde_json::to_string(&old).unwrap(), r#"{"command":"a","args":[]}"#);
    }

    #[test]
    fn names_fit() {
        let long = "a".repeat(90);
        assert!(full_name("servidor", &long).len() <= 64);
        assert_ne!(full_name("s", &format!("{long}1")), full_name("s", &format!("{long}2")));
        assert_eq!(full_name("s", "con.punto y espacio"), "s__con_punto_y_espacio");
    }
}

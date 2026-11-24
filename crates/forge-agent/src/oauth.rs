// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Inicio de sesión en servidores MCP remotos: OAuth 2.1 con PKCE y registro
//! automático, en el navegador del usuario. Las claves se guardan en un archivo
//! que solo él puede leer y van atadas a la dirección exacta del servidor.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::web;

pub use crate::remote::LOGIN;

/// Cuánto se espera a que el usuario inicie sesión.
const WAIT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Grant {
    access: String,
    refresh: Option<String>,
    /// Segundos desde 1970.
    expires: Option<u64>,
    token_endpoint: String,
    client_id: String,
    client_secret: Option<String>,
    resource: String,
}

struct Store {
    path: Option<PathBuf>,
    /// Dirección del servidor → concesión.
    grants: BTreeMap<String, Grant>,
    /// Lo que dijo cada servidor al negarse.
    challenges: BTreeMap<String, String>,
}

static STORE: Mutex<Store> =
    Mutex::new(Store { path: None, grants: BTreeMap::new(), challenges: BTreeMap::new() });

fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Dónde se guardan las claves. Se llama al arrancar.
pub fn set_path(p: PathBuf) {
    let text = std::fs::read_to_string(&p).unwrap_or_default();
    let mut s = store();
    s.grants = serde_json::from_str(&text).unwrap_or_default();
    s.path = Some(p);
}

impl Store {
    fn save(&self) {
        use std::os::unix::fs::OpenOptionsExt;
        let Some(p) = &self.path else { return };
        let write = || -> std::io::Result<()> {
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            let tmp = p.with_extension("tmp");
            let _ = std::fs::remove_file(&tmp);
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
            f.write_all(serde_json::to_string_pretty(&self.grants)?.as_bytes())?;
            std::fs::rename(&tmp, p)
        };
        let _ = write();
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// ¿Hay sesión iniciada en ese servidor?
pub fn signed_in(url: &str) -> bool {
    store().grants.contains_key(url)
}

/// Olvida la sesión (en este Mac).
pub fn sign_out(url: &str) {
    let mut s = store();
    s.grants.remove(url);
    s.save();
}

pub(crate) fn remember_challenge(url: &str, c: Option<String>) {
    if let Some(c) = c {
        store().challenges.insert(url.to_string(), c);
    }
}

/// Clave vigente, renovándola si está a punto de caducar.
pub(crate) fn token(url: &str) -> Option<String> {
    let g = store().grants.get(url).cloned()?;
    if g.expires.is_some_and(|e| e < now() + 30) && refresh(url) {
        return store().grants.get(url).map(|g| g.access.clone());
    }
    Some(g.access)
}

/// Renueva la clave. `false`: no se puede.
pub(crate) fn refresh(url: &str) -> bool {
    // Una a la vez: muchas claves de renovación solo valen una vez.
    static ONE: Mutex<()> = Mutex::new(());
    let Some(before) = store().grants.get(url).cloned() else { return false };
    let _guard = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(g) = store().grants.get(url).cloned() else { return false };
    if g.access != before.access {
        return true;
    }
    let Some(r) = g.refresh.clone() else { return false };
    let form = [("grant_type", "refresh_token"), ("refresh_token", &r), ("resource", &g.resource)];
    let Ok(new) = ask_token(&g, &form) else { return false };
    let mut s = store();
    // Pudo cerrar la sesión mientras tanto.
    if !s.grants.contains_key(url) {
        return false;
    }
    let refresh = new.refresh.clone().or(Some(r));
    s.grants.insert(url.to_string(), Grant { refresh, ..new });
    s.save();
    true
}

// ---------- utilidades ----------

fn random(n: usize) -> Result<String, String> {
    let mut b = vec![0u8; n];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).map_err(|e| e.to_string())?;
    Ok(b64(&b))
}

/// Base64 para direcciones, sin relleno.
fn b64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

fn challenge_of(verifier: &str) -> String {
    b64(&Sha256::digest(verifier.as_bytes()))
}

pub(crate) fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            b => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let hex = |x: Option<&u8>| x.and_then(|c| (*c as char).to_digit(16));
        match b[i] {
            b'+' => out.push(b' '),
            b'%' => match (hex(b.get(i + 1)), hex(b.get(i + 2))) {
                (Some(h), Some(l)) => {
                    out.push((h * 16 + l) as u8);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query(q: &str) -> HashMap<String, String> {
    q.split('&').filter_map(|p| p.split_once('=')).map(|(k, v)| (decode(k), decode(v))).collect()
}

fn form(fields: &[(&str, &str)]) -> String {
    fields.iter().map(|(k, v)| format!("{}={}", encode(k), encode(v))).collect::<Vec<_>>().join("&")
}

/// Solo por conexión segura (o en este mismo Mac).
fn safe(url: &str) -> Result<String, String> {
    web::check(url)?;
    if url.starts_with("https://") || web::is_loopback(url) {
        Ok(url.to_string())
    } else {
        Err("el inicio de sesión tiene que ser por https://".into())
    }
}

/// JSON de una dirección; `None` si no está.
fn get_json(url: &str) -> Result<Option<Value>, String> {
    safe(url)?;
    let mut r = web::agent()
        .get(url)
        .header("Accept", "application/json")
        .call()
        .map_err(|e| web::explain(&e, url))?;
    if !r.status().is_success() {
        return Ok(None);
    }
    let text = r.body_mut().with_config().limit(web::BODY_MAX).read_to_string();
    Ok(text.ok().and_then(|t| serde_json::from_str(&t).ok()))
}

// ---------- descubrimiento ----------

#[derive(Debug, Clone, PartialEq)]
struct Plan {
    issuer: String,
    authorize: String,
    token: String,
    register: Option<String>,
    scope: Option<String>,
    resource: String,
}

/// `clave="valor"` de una cabecera `WWW-Authenticate`.
fn param(header: &str, key: &str) -> Option<String> {
    let i = header.find(&format!("{key}=\""))? + key.len() + 2;
    Some(header[i..].split('"').next()?.to_string())
}

fn discover(url: &str) -> Result<Plan, String> {
    let base = web::origin(url).ok_or("la dirección no es válida")?;
    let path = web::path(url);
    let challenge = store().challenges.get(url).cloned().unwrap_or_default();
    // Qué protege el servidor y quién da las claves.
    let mut places = Vec::new();
    // Solo se hace caso a una dirección del propio servidor.
    if let Some(m) = param(&challenge, "resource_metadata").filter(|m| web::origin(m).as_ref() == Some(&base))
    {
        places.push(m);
    }
    if !path.is_empty() {
        places.push(format!("{base}/.well-known/oauth-protected-resource{path}"));
    }
    places.push(format!("{base}/.well-known/oauth-protected-resource"));
    let mut meta = Value::Null;
    for p in &places {
        if let Some(v) = get_json(p)?.filter(|v| v.get("authorization_servers").is_some()) {
            meta = v;
            break;
        }
    }
    // El recurso tiene que ser este servidor: si no, la clave serviría en otro.
    let resource = match meta.get("resource").and_then(Value::as_str) {
        Some(r) if web::origin(r).as_ref() == Some(&base) => r.to_string(),
        Some(_) => return Err("su inicio de sesión dice ser de otro sitio".into()),
        None => url.split('?').next().unwrap_or(url).to_string(),
    };
    let issuer = meta
        .pointer("/authorization_servers/0")
        .and_then(Value::as_str)
        .unwrap_or(&base)
        .trim_end_matches('/')
        .to_string();
    safe(&issuer)?;
    let (ib, ip) = (web::origin(&issuer).ok_or("la dirección no es válida")?, web::path(&issuer));
    let mut found = None;
    for p in [
        format!("{ib}/.well-known/oauth-authorization-server{ip}"),
        format!("{ib}/.well-known/openid-configuration{ip}"),
        format!("{ib}{ip}/.well-known/openid-configuration"),
    ] {
        if let Some(v) = get_json(&p)?.filter(|v| v.get("authorization_endpoint").is_some()) {
            found = Some(v);
            break;
        }
    }
    let m = found.ok_or("no sé cómo iniciar sesión aquí: usa «Poner clave»")?;
    if m.get("issuer").and_then(Value::as_str).is_some_and(|i| i.trim_end_matches('/') != issuer) {
        return Err("su inicio de sesión dice ser de otro sitio".into());
    }
    if let Some(list) = m.get("code_challenge_methods_supported").and_then(Value::as_array)
        && !list.iter().any(|x| x == "S256")
    {
        return Err("su inicio de sesión no es lo bastante seguro (sin PKCE)".into());
    }
    let end = |k: &str| m.get(k).and_then(Value::as_str);
    let scopes = |v: &Value| {
        let l: Vec<&str> = v.get("scopes_supported")?.as_array()?.iter().filter_map(Value::as_str).collect();
        (!l.is_empty()).then(|| l.join(" "))
    };
    Ok(Plan {
        authorize: safe(end("authorization_endpoint").unwrap_or_default())?,
        token: safe(end("token_endpoint").unwrap_or_default())?,
        register: end("registration_endpoint").map(safe).transpose()?,
        scope: param(&challenge, "scope").or_else(|| scopes(&meta)),
        resource,
        issuer,
    })
}

fn register(plan: &Plan, redirect: &str) -> Result<(String, Option<String>), String> {
    let url = plan.register.as_ref().ok_or("no deja iniciar sesión así: usa «Poner clave»")?;
    let body = json!({"client_name": "forge", "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"],
        "token_endpoint_auth_method": "none"});
    let mut r = web::agent()
        .post(url)
        .header("Content-Type", "application/json")
        .send(body.to_string())
        .map_err(|e| web::explain(&e, url))?;
    let text = r.body_mut().with_config().limit(web::BODY_MAX).read_to_string().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or_default();
    match v.get("client_id").and_then(Value::as_str) {
        Some(id) if r.status().is_success() => {
            Ok((id.to_string(), v.get("client_secret").and_then(Value::as_str).map(str::to_string)))
        }
        _ => Err(format!("no ha dejado registrar forge (error {})", r.status().as_u16())),
    }
}

/// Pide una clave.
fn ask_token(g: &Grant, fields: &[(&str, &str)]) -> Result<Grant, String> {
    let mut all = fields.to_vec();
    all.push(("client_id", &g.client_id));
    if let Some(s) = &g.client_secret {
        all.push(("client_secret", s));
    }
    let url = safe(&g.token_endpoint)?;
    let mut r = web::agent()
        .post(&url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .send(form(&all))
        .map_err(|e| web::explain(&e, &url))?;
    let text = r.body_mut().with_config().limit(web::BODY_MAX).read_to_string().unwrap_or_default();
    let v: Value = serde_json::from_str(&text).unwrap_or_default();
    let get = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let access = get("access_token")
        .filter(|t| r.status().is_success() && !t.chars().any(|c| c.is_control() || c.is_whitespace()))
        .ok_or_else(|| {
            format!("no ha dado la clave: {}", get("error").unwrap_or_else(|| r.status().to_string()))
        })?;
    if get("token_type").is_some_and(|t| !t.eq_ignore_ascii_case("bearer")) {
        return Err("da un tipo de clave que forge no admite".into());
    }
    Ok(Grant {
        access,
        refresh: get("refresh_token"),
        expires: v.get("expires_in").and_then(Value::as_u64).map(|s| now() + s),
        ..g.clone()
    })
}

// ---------- inicio de sesión ----------

/// Inicia sesión en el navegador (`open` abre la dirección) y guarda la clave.
pub fn sign_in(
    url: &str,
    open: &dyn Fn(&str) -> Result<(), String>,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    web::check(url)?;
    safe(url)?;
    let plan = discover(url)?;
    // Una puerta de vuelta solo para esto, en este Mac.
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let (client_id, client_secret) = register(&plan, &redirect)?;
    let (verifier, state) = (random(48)?, random(24)?);
    let challenge = challenge_of(&verifier);
    let mut q = vec![
        ("response_type", "code"),
        ("client_id", client_id.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("state", state.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("resource", plan.resource.as_str()),
    ];
    if let Some(s) = &plan.scope {
        q.push(("scope", s));
    }
    let sep = if plan.authorize.contains('?') { '&' } else { '?' };
    open(&format!("{}{sep}{}", plan.authorize, form(&q)))?;
    let code = wait(&listener, &state, &plan.issuer, cancelled)?;
    drop(listener);
    let g = Grant {
        token_endpoint: plan.token.clone(),
        client_id,
        client_secret,
        resource: plan.resource.clone(),
        ..Grant::default()
    };
    let grant = ask_token(
        &g,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect),
            ("code_verifier", &verifier),
            ("resource", &plan.resource),
        ],
    )?;
    let mut s = store();
    s.grants.insert(url.to_string(), grant);
    s.save();
    Ok(())
}

/// Espera a que el navegador vuelva con el código.
fn wait(l: &TcpListener, state: &str, issuer: &str, cancelled: &dyn Fn() -> bool) -> Result<String, String> {
    let start = Instant::now();
    loop {
        if cancelled() {
            return Err("cancelado".into());
        }
        if start.elapsed() > WAIT {
            return Err("no has iniciado sesión a tiempo".into());
        }
        let Ok((mut s, _)) = l.accept() else {
            std::thread::sleep(Duration::from_millis(60));
            continue;
        };
        let _ = s.set_nonblocking(false);
        let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
        let mut b = Vec::new();
        let mut chunk = [0u8; 2048];
        while b.len() < 16 * 1024 && !b.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut chunk) {
                Ok(n) if n > 0 => b.extend_from_slice(&chunk[..n]),
                _ => break,
            }
        }
        let text = String::from_utf8_lossy(&b);
        let target =
            text.lines().next().unwrap_or_default().split(' ').nth(1).unwrap_or_default().to_string();
        let mut page = |status: &str, msg: &str| {
            let html = format!(
                "<!doctype html><meta charset=utf-8><title>forge</title><body style=\"background:#12100e;color:#ece4da;font:17px -apple-system;display:grid;place-items:center;height:100vh;margin:0\"><p>{msg}"
            );
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{html}",
                html.len()
            );
        };
        let Some(q) = target.strip_prefix("/callback?").map(query) else {
            page("404 Not Found", "Nada por aquí.");
            continue;
        };
        // Solo vale la vuelta de este inicio de sesión.
        if q.get("state").map(String::as_str) != Some(state) {
            page("400 Bad Request", "Esta página no es de forge.");
            continue;
        }
        if q.get("iss").is_some_and(|i| i.trim_end_matches('/') != issuer) {
            page("400 Bad Request", "La respuesta viene de otro sitio.");
            return Err("la respuesta viene de otro sitio".into());
        }
        return match (q.get("code"), q.get("error")) {
            (Some(c), None) => {
                page("200 OK", "Listo. Ya puedes volver a forge.");
                Ok(c.clone())
            }
            (_, e) => {
                page("200 OK", "No se ha iniciado sesión. Puedes cerrar esta página.");
                Err(match e.map(String::as_str) {
                    Some("access_denied") => "no has dado permiso".into(),
                    e => format!("no se ha iniciado sesión ({})", e.unwrap_or("sin código")),
                })
            }
        };
    }
}

/// Abre una dirección en el navegador del usuario.
pub fn open_browser(url: &str) -> Result<(), String> {
    safe(url)?;
    std::process::Command::new("/usr/bin/open")
        .arg(url)
        .status()
        .map_err(|e| e.to_string())
        .and_then(|s| if s.success() { Ok(()) } else { Err("no se ha podido abrir el navegador".into()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings() {
        // Ejemplo de la RFC 7636.
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!((b64(b"f"), b64(b"fo"), b64(b"foo")), ("Zg".into(), "Zm8".into(), "Zm9v".into()));
        assert_eq!(decode(&encode("a b&c=ñ/+")), "a b&c=ñ/+");
        assert_eq!(query("a=1&b=x%20y+z")["b"], "x y z");
        assert_eq!(
            param(r#"Bearer realm="x", resource_metadata="https://a/b""#, "resource_metadata").unwrap(),
            "https://a/b"
        );
        assert_ne!(random(24).unwrap(), random(24).unwrap());
    }

    #[test]
    fn only_secure_places() {
        assert!(safe("https://a.com/x").is_ok() && safe("http://127.0.0.1:9/x").is_ok());
        for bad in ["http://a.com", "http://192.168.1.2/x", "file:///x", ""] {
            assert!(safe(bad).is_err(), "{bad}");
        }
    }
}

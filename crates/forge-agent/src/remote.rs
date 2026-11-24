// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Servidores MCP remotos: JSON-RPC por HTTP («Streamable HTTP»), con las
//! respuestas en JSON o en streaming, sesión, y la clave del inicio de sesión.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use ureq::http::{Response, StatusCode};
use ureq::{Body, RequestBuilder};

use crate::mcp::{PROTOCOL, hello};
use crate::{oauth, web};

/// El servidor pide iniciar sesión.
pub const LOGIN: &str = "pide que inicies sesión";
const LINE_MAX: u64 = 16 << 20;

pub struct Remote {
    inner: Arc<Inner>,
}

struct Inner {
    url: String,
    headers: BTreeMap<String, String>,
    session: Mutex<Option<String>>,
    next: AtomicU64,
}

impl Drop for Remote {
    fn drop(&mut self) {
        // Se despide, sin esperar a que le contesten.
        if self.inner.session().is_some() {
            let i = self.inner.clone();
            std::thread::spawn(move || {
                let r = i.dress(web::agent().delete(&i.url));
                let _ = r.config().timeout_global(Some(Duration::from_secs(3))).build().call();
            });
        }
    }
}

enum Failed {
    /// La sesión caducó: hay que presentarse otra vez.
    Session,
    Login,
    Other(String),
}

impl Remote {
    pub fn new(url: &str, headers: &BTreeMap<String, String>) -> Result<Self, String> {
        web::check(url)?;
        Ok(Self {
            inner: Arc::new(Inner {
                url: url.to_string(),
                headers: headers.clone(),
                session: Mutex::default(),
                next: AtomicU64::new(1),
            }),
        })
    }

    /// Pregunta y espera en otro hilo, para poder cancelar.
    pub fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Value, String> {
        let (tx, rx) = mpsc::channel();
        let (inner, m) = (self.inner.clone(), method.to_string());
        let id = inner.next.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || drop(tx.send(inner.ask(id, &m, params))));
        let start = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(60)) {
                Ok(r) => return r,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("se ha interrumpido".into()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let stop = cancelled();
            if stop || start.elapsed() > timeout {
                let inner = self.inner.clone();
                let why = if stop { "user" } else { "timeout" };
                let v = json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                    "params": {"requestId": id, "reason": why}});
                std::thread::spawn(move || drop(inner.post(&v)));
                return Err(if stop { "parado".into() } else { "tarda demasiado".into() });
            }
        }
    }

    pub fn notify(&self, v: &Value) -> Result<(), String> {
        self.inner.post(v).map(drop).map_err(|e| self.inner.say(e))
    }
}

impl Inner {
    fn session(&self) -> Option<String> {
        self.session.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_session(&self, s: Option<String>) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }

    fn say(&self, f: Failed) -> String {
        match f {
            Failed::Session => "la sesión ha caducado".into(),
            Failed::Login => LOGIN.into(),
            Failed::Other(e) => e,
        }
    }

    /// Cabeceras comunes.
    fn dress<T>(&self, mut r: RequestBuilder<T>) -> RequestBuilder<T> {
        for (k, v) in &self.headers {
            r = r.header(k, v);
        }
        r = r.header("MCP-Protocol-Version", PROTOCOL);
        if let Some(s) = self.session() {
            r = r.header("Mcp-Session-Id", s);
        }
        // Una cabecera puesta a mano manda sobre la sesión iniciada.
        if !self.headers.keys().any(|k| k.eq_ignore_ascii_case("authorization"))
            && let Some(t) = oauth::token(&self.url)
        {
            r = r.header("Authorization", format!("Bearer {t}"));
        }
        r
    }

    fn post(&self, v: &Value) -> Result<Response<Body>, Failed> {
        let send = || {
            self.dress(web::agent().post(&self.url))
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .send(v.to_string())
                .map_err(|e| Failed::Other(web::explain(&e, &self.url)))
        };
        let mut r = send()?;
        // La clave caducó: se renueva y se repite una vez.
        if r.status() == StatusCode::UNAUTHORIZED && oauth::refresh(&self.url) {
            r = send()?;
        }
        let status = r.status();
        if status.is_success() {
            return Ok(r);
        }
        let challenge = r.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).map(str::to_string);
        Err(match status.as_u16() {
            401 => {
                oauth::remember_challenge(&self.url, challenge);
                Failed::Login
            }
            403 => Failed::Other("no te deja pasar (403): revisa la clave o los permisos".into()),
            404 if self.session().is_some() => Failed::Session,
            404 => Failed::Other("no hay ningún servidor MCP en esa dirección (404)".into()),
            405 => Failed::Other("usa el transporte antiguo (SSE), que forge no admite".into()),
            s @ 300..=399 => {
                let to =
                    r.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("otra dirección");
                Failed::Other(format!("redirige a {to} ({s}): usa esa dirección"))
            }
            s => {
                let body = r.body_mut().with_config().limit(8 << 10).read_to_string().unwrap_or_default();
                let msg = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|v| {
                        v.pointer("/error/message").or(v.get("message"))?.as_str().map(str::to_string)
                    })
                    .unwrap_or_default();
                Failed::Other(format!("error {s} {msg}").trim().chars().take(200).collect())
            }
        })
    }

    fn ask(&self, id: u64, method: &str, params: Value) -> Result<Value, String> {
        let v = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        match self.exchange(id, &v) {
            Err(Failed::Session) if method != "initialize" => {
                // Se presenta otra vez y repite.
                self.set_session(None);
                let n = self.next.fetch_add(1, Ordering::Relaxed);
                let h = json!({"jsonrpc": "2.0", "id": n, "method": "initialize", "params": hello()});
                let again = self.exchange(n, &h).and_then(|_| {
                    self.post(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                });
                again.map_err(|e| self.say(e))?;
                self.exchange(id, &v).map_err(|e| self.say(e))?
            }
            r => r.map_err(|e| self.say(e))?,
        }
    }

    /// Manda y lee hasta la respuesta a `id`.
    fn exchange(&self, id: u64, v: &Value) -> Result<Result<Value, String>, Failed> {
        let mut r = self.post(v)?;
        if let Some(s) = r.headers().get("mcp-session-id").and_then(|s| s.to_str().ok())
            && s.len() <= 512
        {
            self.set_session(Some(s.to_string()));
        }
        let kind =
            r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        if !kind.starts_with("text/event-stream") {
            let text = r.body_mut().with_config().limit(LINE_MAX).read_to_string();
            let text = text.map_err(|e| Failed::Other(web::explain(&e, &self.url)))?;
            let v: Value = serde_json::from_str(&text)
                .map_err(|_| Failed::Other("no contesta como un servidor MCP".into()))?;
            let list = v.as_array().cloned().unwrap_or_else(|| vec![v]);
            return list
                .iter()
                .find_map(|m| answer(m, id))
                .ok_or(Failed::Other("no ha contestado a lo preguntado".into()));
        }
        let mut lines = BufReader::new(r.body_mut().as_reader());
        let mut data = String::new();
        loop {
            let mut line = Vec::new();
            let n = lines
                .by_ref()
                .take(LINE_MAX)
                .read_until(b'\n', &mut line)
                .map_err(|e| Failed::Other(format!("se ha cortado: {e}")))?;
            let text = String::from_utf8_lossy(&line);
            let text = text.trim_end_matches(['\n', '\r']);
            if let Some(d) = text.strip_prefix("data:") {
                if data.len() + d.len() > LINE_MAX as usize {
                    return Err(Failed::Other("la respuesta es demasiado grande".into()));
                }
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(d.strip_prefix(' ').unwrap_or(d));
            }
            // Línea en blanco (o el final): evento completo.
            if (text.is_empty() || n == 0)
                && !data.is_empty()
                && let Ok(m) = serde_json::from_str::<Value>(&std::mem::take(&mut data))
            {
                for m in m.as_array().cloned().unwrap_or_else(|| vec![m]) {
                    if let Some(a) = answer(&m, id) {
                        return Ok(a);
                    }
                    self.reply(&m);
                }
            }
            if n == 0 {
                return Err(Failed::Other("ha cerrado sin contestar".into()));
            }
        }
    }

    /// Peticiones del servidor: forge no le ofrece nada.
    fn reply(&self, m: &Value) {
        let (Some(method), Some(id)) = (m.get("method").and_then(Value::as_str), m.get("id")) else { return };
        let v = if method == "ping" {
            json!({"jsonrpc": "2.0", "id": id, "result": {}})
        } else {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Not supported"}})
        };
        let _ = self.post(&v);
    }
}

/// Respuesta a `id`, si lo es.
fn answer(m: &Value, id: u64) -> Option<Result<Value, String>> {
    if m.get("method").is_some() || m.get("id")?.as_u64()? != id {
        return None;
    }
    Some(match m.get("error") {
        Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
        None => Ok(m.get("result").cloned().unwrap_or_default()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{Client, Config};
    use std::process::{Child, Command, Stdio};

    /// Servidor MCP remoto con inicio de sesión, streaming y sesiones que caducan.
    const SERVER: &str = r#"
import json, sys, hashlib, base64, urllib.parse as U
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import socketserver
MODE = sys.argv[1]
S = {"code": None, "challenge": None, "session": 0, "valid": set(), "access": "t1", "seen": []}
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(s, *a): pass
    def out(s, code, body=b"", kind="application/json", **h):
        if not isinstance(body, bytes): body = json.dumps(body).encode()
        s.send_response(code); s.send_header("Content-Type", kind); s.send_header("Content-Length", str(len(body)))
        for k, v in h.items(): s.send_header(k.replace("_", "-"), v)
        s.end_headers(); s.wfile.write(body)
    def base(s): return "http://127.0.0.1:%d" % s.server.server_address[1]
    def do_GET(s):
        u = U.urlparse(s.path); b = s.base()
        if u.path == "/.well-known/oauth-protected-resource/mcp":
            r = "https://otro.example/mcp" if MODE == "trampa" else b + "/mcp"
            s.out(200, {"resource": r, "authorization_servers": [b], "scopes_supported": ["leer"]})
        elif u.path == "/.well-known/oauth-authorization-server":
            s.out(200, {"issuer": b, "authorization_endpoint": b + "/authorize", "token_endpoint": b + "/token", "registration_endpoint": b + "/register", "code_challenge_methods_supported": ["S256"]})
        elif u.path == "/authorize":
            q = dict(U.parse_qsl(u.query)); assert q["scope"] == "leer" and q["resource"] == b + "/mcp"
            S["code"], S["challenge"] = "c0d", q["code_challenge"]
            if MODE == "negar": to = "?error=access_denied&state=" + U.quote(q["state"])
            else: to = "?code=c0d&state=" + U.quote(q["state"])
            s.out(302, Location=q["redirect_uri"] + to)
        else: s.out(404)
    def do_DELETE(s): S["seen"].append("DELETE"); s.out(200)
    def do_POST(s):
        raw = s.rfile.read(int(s.headers.get("Content-Length", 0))); b = s.base()
        if s.path == "/register": return s.out(201, {"client_id": "cli"})
        if s.path == "/token":
            q = dict(U.parse_qsl(raw.decode()))
            if q["grant_type"] == "refresh_token":
                assert q["refresh_token"] == "r1"; S["access"] = "t2"
                return s.out(200, {"access_token": "t2", "token_type": "Bearer", "expires_in": 3600})
            ok = base64.urlsafe_b64encode(hashlib.sha256(q["code_verifier"].encode()).digest()).rstrip(b"=").decode() == S["challenge"]
            if not ok or q["code"] != "c0d" or q["client_id"] != "cli": return s.out(400, {"error": "invalid_grant"})
            return s.out(200, {"access_token": "t1", "refresh_token": "r1", "token_type": "bearer", "expires_in": 3600})
        if s.path == "/salta": return s.out(307, Location="https://otro.example/mcp")
        if s.path != "/mcp": return s.out(404)
        if MODE == "clave":
            if s.headers.get("X-Clave") != "abc": return s.out(403)
        elif s.headers.get("Authorization") != "Bearer " + S["access"]:
            return s.out(401, WWW_Authenticate='Bearer resource_metadata="%s/.well-known/oauth-protected-resource/mcp"' % b)
        m = json.loads(raw); k = m.get("method"); i = m.get("id")
        if k == "initialize":
            S["session"] += 1; S["valid"] = {str(S["session"])}
            return s.out(200, {"jsonrpc": "2.0", "id": i, "result": {"capabilities": {}}}, Mcp_Session_Id=str(S["session"]))
        if s.headers.get("Mcp-Session-Id") not in S["valid"]: return s.out(404)
        if i is None or not k: return s.out(202)
        if k == "tools/list":
            return s.out(200, {"jsonrpc": "2.0", "id": i, "result": {"tools": [{"name": n} for n in ("hora", "caduca", "renueva", "lento", "corta")]}})
        n = m["params"]["name"]
        r = {"jsonrpc": "2.0", "id": i, "result": {"content": [{"type": "text", "text": n + " ok"}]}}
        if n == "hora":
            ev = ": hola\n\nevent: message\ndata: " + json.dumps({"jsonrpc": "2.0", "id": 77, "method": "sampling/createMessage"}) + "\n\n"
            ev += "data: " + json.dumps({"jsonrpc": "2.0", "method": "notifications/progress"}) + "\r\n\r\n"
            t = json.dumps(r); ev += "data: " + t[:20] + "\ndata: " + t[20:] + "\n\n" if MODE == "x" else "data: " + t + "\n\n"
            return s.out(200, ev.encode(), "text/event-stream")
        if n == "caduca" and S["session"] == 1: S["valid"] = set(); return s.out(404)
        if n == "renueva" and S["access"] == "t1": S["access"] = "t2"; return s.out(401)
        if n == "lento": import time; time.sleep(20)
        if n == "corta": return s.out(200, b"data: {\"a\": 1}\n\n", "text/event-stream")
        s.out(200, r)
class T(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True; daemon_threads = True
srv = T(("127.0.0.1", 0), H)
print(srv.server_address[1], flush=True); srv.serve_forever()
"#;

    struct Server(Child, String);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn serve(mode: &str) -> Option<Server> {
        if !crate::mcp::tests::available() {
            return None;
        }
        let mut c = Command::new("/usr/bin/python3")
            .args(["-c", SERVER, mode])
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let mut line = String::new();
        BufReader::new(c.stdout.take()?).read_line(&mut line).ok()?;
        Some(Server(c, format!("http://127.0.0.1:{}/mcp", line.trim())))
    }

    fn config(url: &str) -> Config {
        Config { url: Some(url.into()), ..Config::default() }
    }

    fn start(c: &Config) -> Result<Client, String> {
        Client::start(c, std::path::Path::new("/"), &|| false)
    }

    /// Lo que haría el navegador: ir a la página y seguir hasta forge.
    fn browser(url: &str) -> Result<(), String> {
        let url = url.to_string();
        std::thread::spawn(move || {
            let r = web::agent().get(&url).call().unwrap();
            let to = r.headers()["location"].to_str().unwrap().to_string();
            // Una página cualquiera no puede colarse.
            let bad = to.replace("state=", "state=x");
            assert_eq!(web::agent().get(&bad).call().unwrap().status(), 400);
            let mut r = web::agent().get(&to).call().unwrap();
            assert!(r.body_mut().read_to_string().unwrap().contains("forge"));
        });
        Ok(())
    }

    #[test]
    fn signs_in_and_talks() {
        let Some(s) = serve("oauth") else { return };
        let c = config(&s.1);
        assert_eq!(start(&c).err().unwrap(), LOGIN);
        oauth::sign_in(&s.1, &browser, &|| false).unwrap();
        assert!(oauth::signed_in(&s.1));
        let client = start(&c).unwrap();
        assert_eq!(client.tools.len(), 5);
        let call = |n: &str| client.call(n, &json!({}), &|| false);
        assert_eq!(call("hora").unwrap(), (true, "hora ok".into()), "en streaming, con ruido");
        assert_eq!(call("caduca").unwrap().1, "caduca ok", "se presenta otra vez");
        assert_eq!(call("renueva").unwrap().1, "renueva ok", "renueva la clave");
        assert!(call("corta").unwrap_err().contains("sin contestar"));
        let t = Instant::now();
        let e = client.call("lento", &json!({}), &|| t.elapsed() > Duration::from_millis(200)).unwrap_err();
        assert!(e == "parado" && t.elapsed() < Duration::from_secs(3));
        oauth::sign_out(&s.1);
        assert_eq!(client.call("hora", &json!({}), &|| false).unwrap_err(), LOGIN);
    }

    #[test]
    fn refuses_tricks_and_explains() {
        let Some(s) = serve("trampa") else { return };
        let e = oauth::sign_in(&s.1, &browser, &|| false).unwrap_err();
        assert!(e.contains("otro sitio"), "{e}");
        let e = start(&config(&s.1.replace("/mcp", "/salta"))).err().unwrap();
        assert!(e.contains("redirige"), "{e}");
        assert!(start(&config(&s.1.replace("/mcp", "/nada"))).err().unwrap().contains("404"));
        let Some(n) = serve("negar") else { return };
        let go = |u: &str| {
            let u = u.to_string();
            std::thread::spawn(move || {
                let r = web::agent().get(&u).call().unwrap();
                let _ = web::agent().get(r.headers()["location"].to_str().unwrap()).call();
            });
            Ok(())
        };
        assert_eq!(oauth::sign_in(&n.1, &go, &|| false).unwrap_err(), "no has dado permiso");
        // Nadie vuelve: se puede cancelar.
        let t = Instant::now();
        let c = || t.elapsed() > Duration::from_millis(400);
        assert_eq!(oauth::sign_in(&n.1, &|_| Ok(()), &c).unwrap_err(), "cancelado");
    }

    #[test]
    fn keys_in_headers() {
        let Some(s) = serve("clave") else { return };
        assert!(start(&config(&s.1)).err().unwrap().contains("403"));
        let c = Config { headers: BTreeMap::from([("X-Clave".into(), "abc".into())]), ..config(&s.1) };
        let client = start(&c).unwrap();
        assert!(client.call("hora", &json!({}), &|| false).unwrap().0);
        assert!(start(&config("http://127.0.0.1:1/mcp")).err().unwrap().contains("no responde"));
    }
}

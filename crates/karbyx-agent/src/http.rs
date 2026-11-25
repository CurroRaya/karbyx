// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! Cliente HTTP/1.1 mínimo para hablar con un servidor de modelos de la red.
//!
//! Es propio por una razón: un modelo local puede tardar minutos en leer la
//! conversación antes de su primera palabra, y hay que poder cancelar en
//! cualquier momento cerrando la conexión (así el servidor deja de calcular).

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub enum HttpError {
    Cancelled,
    /// No se llegó al servidor; el texto es para el usuario.
    Connect(String),
    /// La conexión se cortó o la respuesta no se entiende.
    Broken(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            HttpError::Cancelled => write!(f, "cancelado"),
            HttpError::Connect(m) | HttpError::Broken(m) => write!(f, "{m}"),
        }
    }
}

/// Dirección base de un servidor: `http://host:puerto/prefijo`.
#[derive(Debug, Clone, PartialEq)]
pub struct Base {
    pub host: String,
    pub port: u16,
    /// Sin barra final ni `/v1`.
    pub prefix: String,
}

impl Base {
    /// Admite «192.168.1.20», «mi-pc:8080», «http://mi-pc:8080/v1»...
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let rest = match s.split_once("://") {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
            Some((scheme, _)) if scheme.eq_ignore_ascii_case("https") => {
                return Err("De momento solo http:// (servidores de tu red)".into());
            }
            Some(_) => return Err("La dirección no es válida".into()),
            None => s,
        };
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        if host.contains('@') {
            return Err("Pon la clave en su campo, no en la dirección".into());
        }
        // IPv6 entre corchetes.
        let (name, port) = if let Some(v6) = host.strip_prefix('[') {
            let (name, after) = v6.split_once(']').ok_or("La dirección no es válida")?;
            (name, after.strip_prefix(':'))
        } else {
            match host.split_once(':') {
                Some((n, p)) => (n, Some(p)),
                None => (host, None),
            }
        };
        let ok = |c: char| c.is_alphanumeric() || "-._:%".contains(c);
        if name.is_empty() || !name.chars().all(ok) {
            return Err("La dirección no es válida".into());
        }
        let port = match port {
            Some(p) => p.parse().map_err(|_| "El puerto no es válido")?,
            None => 8080,
        };
        if path.contains(|c: char| c.is_whitespace() || c.is_control() || "?#".contains(c)) {
            return Err("La dirección no es válida".into());
        }
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix("v1").map(|p| p.trim_end_matches('/')).unwrap_or(path);
        let prefix = if path.is_empty() { String::new() } else { format!("/{path}") };
        Ok(Self { host: name.to_string(), port, prefix })
    }

    fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}{}", self.authority(), self.prefix)
    }
}

pub struct Request<'a> {
    pub base: &'a Base,
    pub method: &'a str,
    /// Sin el prefijo de la base: `/v1/models`.
    pub path: &'a str,
    pub key: Option<&'a str>,
    pub body: Option<&'a str>,
    /// Límite total (`None`: sin límite, se cancela a mano).
    pub timeout: Option<Duration>,
}

enum Framing {
    Chunked { left: usize, done: bool },
    Length(usize),
    Eof,
}

/// Respuesta con el cuerpo aún por leer. Al soltarla se cierra la conexión.
pub struct Response<'a> {
    pub status: u16,
    stream: TcpStream,
    buf: Vec<u8>,
    framing: Framing,
    deadline: Option<Instant>,
    cancelled: &'a dyn Fn() -> bool,
}

impl Drop for Response<'_> {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn connect(base: &Base, cancelled: &dyn Fn() -> bool) -> Result<TcpStream, HttpError> {
    let place = format!("{}:{}", base.host, base.port);
    let addrs: Vec<_> = (base.host.as_str(), base.port)
        .to_socket_addrs()
        .map_err(|_| {
            HttpError::Connect(format!("No encuentro «{}» en la red. Prueba con su IP.", base.host))
        })?
        .collect();
    let mut last = None;
    for a in &addrs {
        if cancelled() {
            return Err(HttpError::Cancelled);
        }
        match TcpStream::connect_timeout(a, Duration::from_secs(4)) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                let _ = s.set_read_timeout(Some(Duration::from_millis(120)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(20)));
                // Si el PC se apaga o se suspende a medias, la conexión muerta se detecta.
                // SAFETY: setsockopt sobre un socket nuestro con un entero de su tamaño.
                unsafe {
                    let set = |level, name, v: i32| {
                        libc::setsockopt(
                            s.as_raw_fd(),
                            level,
                            name,
                            (&raw const v).cast(),
                            size_of::<i32>() as libc::socklen_t,
                        )
                    };
                    set(libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
                    #[cfg(target_os = "macos")]
                    set(libc::IPPROTO_TCP, libc::TCP_KEEPALIVE, 20);
                }
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
    }
    use std::io::ErrorKind as K;
    let msg = match last {
        Some(e) if e.kind() == K::ConnectionRefused => {
            format!(
                "{} responde, pero no hay nada en el puerto {}. ¿Está arrancado el servidor?",
                base.host, base.port
            )
        }
        Some(e) if e.kind() == K::TimedOut => {
            format!("{place} no responde. ¿Está encendido? ¿Deja pasar el puerto el cortafuegos de Windows?")
        }
        Some(e) if matches!(e.kind(), K::HostUnreachable | K::NetworkUnreachable) => format!(
            "No llego a {place}. Revisa que esté en la misma red y, en Ajustes del Sistema › Privacidad › Red local, que Karbyx tenga permiso."
        ),
        Some(e) => format!("No llego a {place}: {e}"),
        None => format!("No encuentro «{}» en la red", base.host),
    };
    Err(HttpError::Connect(msg))
}

/// Envía la petición y lee las cabeceras.
pub fn send<'a>(req: &Request, cancelled: &'a dyn Fn() -> bool) -> Result<Response<'a>, HttpError> {
    let deadline = req.timeout.map(|t| Instant::now() + t);
    let stream = connect(req.base, cancelled)?;
    let mut head = format!(
        "{} {}{} HTTP/1.1\r\nHost: {}\r\nUser-Agent: karbyx\r\nAccept: */*\r\nConnection: close\r\n",
        req.method,
        req.base.prefix,
        req.path,
        req.base.authority()
    );
    if let Some(k) = req.key.map(str::trim).filter(|k| !k.is_empty()) {
        if k.chars().any(|c| c.is_control()) {
            return Err(HttpError::Connect("La clave no es válida".into()));
        }
        head.push_str(&format!("Authorization: Bearer {k}\r\n"));
    }
    let body = req.body.unwrap_or_default();
    if req.body.is_some() {
        head.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    let mut r = Response { status: 0, stream, buf: Vec::new(), framing: Framing::Eof, deadline, cancelled };
    let broken = |e: std::io::Error| HttpError::Broken(format!("Se cortó la conexión: {e}"));
    r.stream.write_all(head.as_bytes()).map_err(broken)?;
    r.stream.write_all(body.as_bytes()).map_err(broken)?;
    r.read_head()?;
    Ok(r)
}

impl Response<'_> {
    /// Añade a `buf` lo que llegue. `false`: el otro lado cerró.
    fn fill(&mut self) -> Result<bool, HttpError> {
        let mut chunk = [0u8; 16 * 1024];
        loop {
            if (self.cancelled)() {
                return Err(HttpError::Cancelled);
            }
            if self.deadline.is_some_and(|d| Instant::now() > d) {
                return Err(HttpError::Broken("El servidor tarda demasiado en responder".into()));
            }
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    self.buf.extend_from_slice(&chunk[..n]);
                    return Ok(true);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(HttpError::Broken(format!("Se cortó la conexión: {e}"))),
            }
        }
    }

    fn read_head(&mut self) -> Result<(), HttpError> {
        let bad = || HttpError::Broken("El servidor no responde como un servidor de modelos".into());
        loop {
            let end = loop {
                if let Some(i) = find(&self.buf, b"\r\n\r\n") {
                    break i;
                }
                if self.buf.len() > 64 * 1024 || !self.fill()? {
                    return Err(bad());
                }
            };
            let head: Vec<u8> = self.buf.drain(..end + 4).collect();
            let head = String::from_utf8_lossy(&head);
            let mut lines = head.lines();
            let status = lines.next().filter(|l| l.starts_with("HTTP/1.")).ok_or_else(bad)?;
            self.status = status.split(' ').nth(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?;
            // Respuestas provisionales (100 Continue).
            if (100..200).contains(&self.status) {
                continue;
            }
            self.framing = Framing::Eof;
            for l in lines {
                let Some((k, v)) = l.split_once(':') else { continue };
                let v = v.trim();
                if k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked") {
                    self.framing = Framing::Chunked { left: 0, done: false };
                    break;
                }
                if k.eq_ignore_ascii_case("content-length") {
                    self.framing = Framing::Length(v.parse().map_err(|_| bad())?);
                }
            }
            return Ok(());
        }
    }

    /// Siguiente trozo del cuerpo, o `None` al final.
    pub fn chunk(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        let cut = || HttpError::Broken("El servidor cortó la respuesta a medias".into());
        loop {
            match &mut self.framing {
                Framing::Eof => {
                    if self.buf.is_empty() && !self.fill()? {
                        return Ok(None);
                    }
                    return Ok(Some(std::mem::take(&mut self.buf)));
                }
                Framing::Length(0) => return Ok(None),
                Framing::Length(left) => {
                    if !self.buf.is_empty() {
                        let n = self.buf.len().min(*left);
                        *left -= n;
                        return Ok(Some(self.buf.drain(..n).collect()));
                    }
                    if !self.fill()? {
                        return Err(cut());
                    }
                }
                Framing::Chunked { done: true, .. } => return Ok(None),
                Framing::Chunked { left, done } if *left == 0 => {
                    // Línea con el tamaño del trozo (tras el \r\n del anterior).
                    let skip = if self.buf.starts_with(b"\r\n") { 2 } else { 0 };
                    if self.buf.len() > skip + 1
                        && let Some(i) = find(&self.buf[skip..], b"\r\n")
                    {
                        let line = String::from_utf8_lossy(&self.buf[skip..skip + i]).into_owned();
                        let hex = line.split(';').next().unwrap_or_default().trim();
                        let n = usize::from_str_radix(hex, 16)
                            .ok()
                            .filter(|n| *n < 1 << 30)
                            .ok_or_else(|| HttpError::Broken("Respuesta mal formada".into()))?;
                        self.buf.drain(..skip + i + 2);
                        *left = n;
                        *done = n == 0;
                        continue;
                    }
                    if self.buf.len() > 4096 {
                        return Err(HttpError::Broken("Respuesta mal formada".into()));
                    }
                    if !self.fill()? {
                        return Err(cut());
                    }
                }
                Framing::Chunked { left, .. } => {
                    if !self.buf.is_empty() {
                        let n = self.buf.len().min(*left);
                        *left -= n;
                        return Ok(Some(self.buf.drain(..n).collect()));
                    }
                    if !self.fill()? {
                        return Err(cut());
                    }
                }
            }
        }
    }

    /// Cuerpo entero (hasta `max` bytes).
    pub fn text(&mut self, max: usize) -> Result<String, HttpError> {
        let mut out = Vec::new();
        while let Some(b) = self.chunk()? {
            out.extend_from_slice(&b);
            if out.len() > max {
                return Err(HttpError::Broken("Respuesta demasiado grande".into()));
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Needs, skip};

    #[test]
    fn addresses() {
        let b = |s: &str| Base::parse(s).map(|b| b.url());
        assert_eq!(b("192.168.1.20").unwrap(), "http://192.168.1.20:8080");
        assert_eq!(b(" mi-pc:1234 ").unwrap(), "http://mi-pc:1234");
        assert_eq!(b("http://mi-pc.local:8080/v1/").unwrap(), "http://mi-pc.local:8080");
        assert_eq!(b("http://x:1/llm/v1").unwrap(), "http://x:1/llm");
        assert_eq!(b("[fe80::1%en0]:9000").unwrap(), "http://[fe80::1%en0]:9000");
        for bad in ["", "https://x", "http://u:p@x", "a b", "x:puerto", "x/a b", "x\r\ny"] {
            assert!(b(bad).is_err(), "{bad}");
        }
    }

    /// Servidor de un solo uso que contesta `reply` a trozos.
    fn serve(reply: &'static [&'static [u8]]) -> Base {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut b = [0u8; 4096];
            let _ = s.read(&mut b);
            for part in reply {
                let _ = s.write_all(part);
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        Base { host: "127.0.0.1".into(), port, prefix: String::new() }
    }

    fn get(base: &Base) -> Result<String, HttpError> {
        let req = Request {
            base,
            method: "GET",
            path: "/",
            key: None,
            body: None,
            timeout: Some(Duration::from_secs(3)),
        };
        send(&req, &|| false)?.text(1000)
    }

    #[test]
    fn framings() {
        if skip(&[Needs::Ports]) {
            return;
        }
        let b = serve(&[
            b"HTTP/1.1 200 OK\r\nTransfer-Enc",
            b"oding: chunked\r\n\r\n4\r",
            b"\nhola\r\n6;x=1\r\n, qu",
            b"\xc3\xa9\r\n0\r\n\r\n",
        ]);
        assert_eq!(get(&b).unwrap(), "hola, qué");
        let b = serve(&[b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nho", b"laXXX"]);
        assert_eq!(get(&b).unwrap(), "hola");
        let b = serve(&[b"HTTP/1.0 200 OK\r\n\r\nho", b"la"]);
        assert_eq!(get(&b).unwrap(), "hola");
        // Cortada a medias: error, no un texto incompleto.
        let b = serve(&[b"HTTP/1.1 200 OK\r\nContent-Length: 40\r\n\r\nho"]);
        assert!(matches!(get(&b), Err(HttpError::Broken(_))));
        let b = serve(&[b"SSH-2.0-OpenSSH\r\n\r\n"]);
        assert!(matches!(get(&b), Err(HttpError::Broken(_))));
    }

    #[test]
    fn cancel_while_waiting() {
        if skip(&[Needs::Ports]) {
            return;
        }
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = Base { host: "127.0.0.1".into(), port: l.local_addr().unwrap().port(), prefix: "".into() };
        let t = Instant::now();
        let req = Request { base: &base, method: "GET", path: "/", key: None, body: None, timeout: None };
        let late = || t.elapsed() > Duration::from_millis(300);
        let r = send(&req, &late);
        assert!(matches!(r, Err(HttpError::Cancelled)));
        assert!(t.elapsed() < Duration::from_secs(2));
    }
}

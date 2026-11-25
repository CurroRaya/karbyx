// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! HTTP con TLS para lo que está en internet (servidores MCP remotos y su
//! inicio de sesión), con las raíces de confianza del sistema.

use std::sync::OnceLock;
use std::time::Duration;

use ureq::Agent;
use ureq::http::Uri;
use ureq::tls::{RootCerts, TlsConfig};

/// Tamaño máximo de una respuesta corriente.
pub(crate) const BODY_MAX: u64 = 4 << 20;

/// Nunca sigue redirecciones: podrían llevar una clave a otro sitio.
pub(crate) fn agent() -> &'static Agent {
    static A: OnceLock<Agent> = OnceLock::new();
    A.get_or_init(|| {
        Agent::config_builder()
            .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent("karbyx")
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(300)))
            .timeout_recv_body(Some(Duration::from_secs(300)))
            .build()
            .into()
    })
}

/// `host` o `host:puerto` de una dirección.
pub fn host(url: &str) -> String {
    url.parse::<Uri>().ok().and_then(|u| u.authority().map(|a| a.as_str().to_string())).unwrap_or_default()
}

/// Esquema, host y puerto: lo que identifica a un sitio.
pub(crate) fn origin(url: &str) -> Option<String> {
    let u: Uri = url.parse().ok()?;
    Some(format!("{}://{}", u.scheme_str()?.to_lowercase(), u.authority()?.as_str().to_lowercase()))
}

/// Ruta sin barra final.
pub(crate) fn path(url: &str) -> String {
    url.parse::<Uri>().map(|u| u.path().trim_end_matches('/').to_string()).unwrap_or_default()
}

pub(crate) fn is_loopback(url: &str) -> bool {
    let h = host(url);
    let name =
        h.rsplit_once(':').filter(|(_, p)| p.bytes().all(|b| b.is_ascii_digit())).map_or(h.as_str(), |x| x.0);
    let name = name.trim_matches(['[', ']']);
    name == "localhost" || name.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// ¿Es una dirección a la que se puede hablar? Sin cifrar, solo en tu red.
pub fn check(url: &str) -> Result<(), String> {
    let u: Uri = url.parse().map_err(|_| "la dirección no es válida")?;
    let auth = u.authority().ok_or("la dirección no es válida")?;
    if auth.as_str().contains('@') || url.contains('#') || url.len() > 400 {
        return Err("la dirección no es válida".into());
    }
    match u.scheme_str() {
        Some("https") => Ok(()),
        Some("http") => {
            let ip = auth.host().trim_matches(['[', ']']).parse::<std::net::IpAddr>();
            let private = match ip {
                Ok(std::net::IpAddr::V4(v)) => v.is_private() || v.is_loopback() || v.is_link_local(),
                Ok(std::net::IpAddr::V6(v)) => v.is_loopback() || (v.segments()[0] & 0xfe00) == 0xfc00,
                Err(_) => auth.host() == "localhost" || auth.host().ends_with(".local"),
            };
            if private { Ok(()) } else { Err("fuera de tu red tiene que ser https://".into()) }
        }
        _ => Err("la dirección tiene que empezar por https://".into()),
    }
}

/// Error de red, en castellano.
pub(crate) fn explain(e: &ureq::Error, url: &str) -> String {
    let h = host(url);
    match e {
        ureq::Error::HostNotFound => format!("no encuentro {h}"),
        ureq::Error::Timeout(_) => format!("{h} tarda demasiado"),
        ureq::Error::ConnectionFailed => format!("{h} no responde"),
        ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => {
            format!("{h} no responde")
        }
        // Un certificado malo llega como error de lectura.
        ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::InvalidData => {
            format!("la conexión segura con {h} no es de fiar")
        }
        ureq::Error::Rustls(_) | ureq::Error::Tls(_) => format!("la conexión segura con {h} no es de fiar"),
        e => format!("no llego a {h}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Con red: `cargo test -p karbyx-agent tls -- --ignored`.
    #[test]
    #[ignore]
    fn tls_is_checked() {
        let get = |u: &str| agent().get(u).header("User-Agent", "Mozilla/5.0").call();
        assert_eq!(get("https://example.com/").unwrap().status(), 200);
        for bad in [
            "https://expired.badssl.com/",
            "https://wrong.host.badssl.com/",
            "https://self-signed.badssl.com/",
        ] {
            let e = get(bad).unwrap_err();
            assert!(explain(&e, bad).contains("no es de fiar"), "{bad}: {e:?}");
        }
    }

    #[test]
    fn addresses() {
        for ok in [
            "https://a.com/mcp",
            "http://localhost:3000",
            "http://127.0.0.1/x",
            "http://192.168.1.2:8",
            "http://mi-pc.local/mcp",
            "http://[::1]:9",
        ] {
            assert!(check(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "a.com",
            "http://a.com",
            "http://8.8.8.8",
            "https://u:p@a.com",
            "file:///etc",
            "https://a.com/#x",
            "http://localhost.malo.com",
        ] {
            assert!(check(bad).is_err(), "{bad}");
        }
        assert!(
            is_loopback("http://127.0.0.1:99/x")
                && is_loopback("http://[::1]:9")
                && !is_loopback("https://localhost.com")
        );
        assert_eq!(origin("https://A.com:8443/x?y"), Some("https://a.com:8443".into()));
        assert_eq!(path("https://a.com/x/y/"), "/x/y");
    }
}

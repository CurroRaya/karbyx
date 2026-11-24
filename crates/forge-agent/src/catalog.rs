// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Buscador de habilidades en skills.sh. Solo da nombres y repositorios: lo
//! que se instala se baja del repositorio de GitHub y se enseña antes.

use std::collections::HashSet;

use serde_json::Value;

use crate::install::From;
use crate::web;

const BASE: &str = "https://skills.sh/api/search";

/// Una habilidad del catálogo.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub name: String,
    /// `dueño/repositorio` de GitHub.
    pub source: String,
    /// Cuántas veces se ha instalado, según el catálogo.
    pub installs: u64,
}

/// ¿Es una dirección o una carpeta, y no algo que buscar?
pub fn is_address(s: &str) -> bool {
    let s = s.trim();
    s.contains('/') || s.starts_with('~')
}

fn hit(v: &Value) -> Option<Hit> {
    let text = |k: &str| v.get(k).and_then(Value::as_str);
    if v.get("isDuplicate").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let source = text("source")?;
    // Solo lo que está en GitHub: es lo que forge sabe bajar y enseñar.
    let part = |p: &str| {
        !p.is_empty()
            && !p.starts_with(['.', '-'])
            && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    let (owner, repo) = source.split_once('/')?;
    if !part(owner) || !part(repo) || From::parse(source).is_err() {
        return None;
    }
    let name = text("skillId").or(text("name"))?;
    let ok = !name.is_empty()
        && name.len() <= 80
        && name.chars().all(|c| c.is_alphanumeric() || "-_.: ".contains(c));
    ok.then(|| Hit {
        name: name.to_string(),
        source: source.to_string(),
        installs: v.get("installs").and_then(Value::as_u64).unwrap_or(0),
    })
}

fn parse(text: &str) -> Result<Vec<Hit>, String> {
    let v: Value = serde_json::from_str(text).map_err(|_| "el catálogo no contesta como se esperaba")?;
    let list = v.get("skills").and_then(Value::as_array).ok_or("el catálogo no contesta como se esperaba")?;
    let mut seen = HashSet::new();
    Ok(list.iter().filter_map(hit).filter(|h| seen.insert((h.name.clone(), h.source.clone()))).collect())
}

/// Busca. Solo se llama cuando el usuario lo pide: lo que escribe se envía a
/// su web.
pub fn search(query: &str) -> Result<Vec<Hit>, String> {
    let q = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if q.chars().count() < 2 {
        return Err("Escribe al menos dos letras".into());
    }
    if q.chars().count() > 60 || q.chars().any(char::is_control) {
        return Err("Busca con unas pocas palabras".into());
    }
    let url = format!("{BASE}?q={}&limit=60", crate::oauth::encode(&q));
    let mut r = web::agent()
        .get(&url)
        .header("Accept", "application/json")
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(25)))
        .build()
        .call()
        .map_err(|e| web::explain(&e, &url))?;
    if !r.status().is_success() {
        return Err(format!("el catálogo contesta con un error {}", r.status().as_u16()));
    }
    let t = r.body_mut().with_config().limit(web::BODY_MAX).read_to_string();
    parse(&t.map_err(|e| web::explain(&e, &url))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_what_can_be_installed() {
        let t = r#"{"skills": [
            {"source": "anthropics/skills", "skillId": "pdf", "name": "pdf", "installs": 236630},
            {"source": "anthropics/skills", "skillId": "pdf", "installs": 1},
            {"source": "otro/copia", "skillId": "pdf", "isDuplicate": true},
            {"source": "open.feishu.cn", "skillId": "lark"},
            {"source": "a/../b", "skillId": "x"},
            {"source": "-a/b", "skillId": "x"},
            {"source": "a/b;rm", "skillId": "x"},
            {"source": "a/b", "skillId": "x\u202ey"},
            {"source": "a/b", "skillId": "con espacio"},
            {"nada": 1}]}"#;
        let h = parse(t).unwrap();
        let ids: Vec<_> = h.iter().map(|h| format!("{} {}", h.source, h.name)).collect();
        assert_eq!(ids, ["anthropics/skills pdf", "a/b con espacio"]);
        assert_eq!(h[0].installs, 236_630);
        assert!(parse("<html>").is_err() && parse("{}").is_err());
    }

    #[test]
    fn tells_addresses_from_words() {
        for a in ["a/b", "https://github.com/a/b", "/tmp/x", "~/x", "~"] {
            assert!(is_address(a), "{a}");
        }
        for w in ["pdf", "code review", "c++", ""] {
            assert!(!is_address(w), "{w}");
        }
    }

    /// Con red: `cargo test -p forge-agent catalog_live -- --ignored`.
    #[test]
    #[ignore]
    fn catalog_live() {
        let h = search("pdf").unwrap();
        eprintln!("{:?}", h.iter().take(5).collect::<Vec<_>>());
        assert_eq!((h[0].source.as_str(), h[0].name.as_str()), ("anthropics/skills", "pdf"));
        assert!(search("code review").unwrap().len() > 5);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! Buscador de servidores MCP: el registro oficial, donde cada autor publica
//! cómo se conecta el suyo. El registro comprueba quién publica (su dominio o
//! su cuenta de GitHub), **no** que lo publicado sea de fiar.

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

use crate::mcp::{self, Config};
use crate::web;

const BASE: &str = "https://registry.modelcontextprotocol.io/v0.1/servers";
const RESULTS: usize = 60;

/// Un dato que hay que rellenar.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub label: String,
    pub about: String,
    pub secret: bool,
    pub required: bool,
    pub default: String,
    pub choices: Vec<String>,
    /// Va dentro de una dirección: solo letras, números y poco más.
    strict: bool,
}

/// Texto con huecos: `Err(n)` es el dato número `n`.
type Template = Vec<Result<String, usize>>;

/// Una forma de conectarlo.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// «En internet», «npm», «Python», «Docker».
    pub label: &'static str,
    /// Programa que hace falta (`npx`, `uvx`, `docker`).
    pub needs: Option<&'static str>,
    pub inputs: Vec<Input>,
    command: &'static str,
    /// Grupos de argumentos que van (o faltan) juntos.
    args: Vec<Vec<Template>>,
    env: Vec<(String, Template)>,
    url: Option<Template>,
    headers: Vec<(String, Template)>,
}

/// Una ficha del registro.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Nombre en el registro: `com.notion/mcp`.
    pub id: String,
    pub title: String,
    pub about: String,
    /// Quién lo publica, según comprobó el registro.
    pub publisher: String,
    pub repo: Option<String>,
    /// Nombre para Karbyx.
    pub name: String,
    pub choices: Vec<Choice>,
}

/// Texto de un desconocido: sin caracteres que escondan o desordenen.
fn clean(s: &str, max: usize) -> String {
    let t: String = s
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| {
            !c.is_control()
                && !matches!(*c as u32, 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x206f | 0xfeff | 0xe0000..=0xe007f)
        })
        .collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= max { t } else { t.chars().take(max - 1).chain(['…']).collect() }
}

/// `io.github.ana/x` → `github.com/ana`; `com.notion/mcp` → `notion.com`.
fn publisher(id: &str) -> String {
    let ns = id.split('/').next().unwrap_or(id);
    match ns.strip_prefix("io.github.") {
        Some(user) => format!("github.com/{user}"),
        None => ns.split('.').rev().collect::<Vec<_>>().join("."),
    }
}

/// Nombre corto: `io.github.github/github-mcp-server` → `github`.
fn short_name(id: &str) -> String {
    let (ns, server) = id.split_once('/').unwrap_or(("", id));
    let mut s = server.to_lowercase();
    for p in ["mcp-server-", "server-", "mcp-"] {
        s = s.strip_prefix(p).unwrap_or(&s).to_string();
    }
    for p in ["-mcp-server", "-server", "-mcp"] {
        s = s.strip_suffix(p).unwrap_or(&s).to_string();
    }
    if matches!(s.as_str(), "" | "mcp" | "server") {
        s = ns.rsplit('.').next().unwrap_or("mcp").to_lowercase();
    }
    mcp::name_ok(&s).unwrap_or_else(|_| "mcp".into())
}

fn text<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn list<'a>(v: &'a Value, k: &str) -> impl Iterator<Item = &'a Value> {
    v.get(k).and_then(Value::as_array).into_iter().flatten()
}

impl Input {
    fn from(v: &Value, label: &str, strict: bool) -> Self {
        let on = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        let choices: Vec<String> =
            list(v, "choices").filter_map(Value::as_str).map(|c| clean(c, 60)).take(12).collect();
        Input {
            label: clean(label, 40),
            about: clean(text(v, "description").unwrap_or_default(), 240),
            secret: on("isSecret"),
            required: on("isRequired"),
            default: text(v, "default").map(|d| clean(d, 200)).unwrap_or_default(),
            choices,
            strict,
        }
    }
}

/// Valor de un argumento, variable o cabecera: fijo, con `{variables}`, o un
/// dato que rellenar.
fn value(v: &Value, label: &str, strict: bool, inputs: &mut Vec<Input>) -> Template {
    let Some(fixed) = text(v, "value") else {
        inputs.push(Input::from(v, label, strict));
        return vec![Err(inputs.len() - 1)];
    };
    template(fixed, v, strict, inputs)
}

/// Sustituye `{nombre}` por un hueco si es una variable declarada.
fn template(s: &str, v: &Value, strict: bool, inputs: &mut Vec<Input>) -> Template {
    let mut out: Template = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find('{') {
        let Some(len) = rest[i..].find('}') else { break };
        let name = &rest[i + 1..i + len];
        let var = v.get("variables").and_then(|x| x.get(name));
        if let Some(var) = var {
            out.push(Ok(rest[..i].to_string()));
            let mut input = Input::from(var, name, strict);
            // Sin ella no se puede formar el valor.
            input.required |= v.get("isRequired").and_then(Value::as_bool).unwrap_or(false) || strict;
            inputs.push(input);
            out.push(Err(inputs.len() - 1));
        } else {
            out.push(Ok(rest[..i + len + 1].to_string()));
        }
        rest = &rest[i + len + 1..];
    }
    out.push(Ok(rest.to_string()));
    out.retain(|p| !matches!(p, Ok(s) if s.is_empty()));
    out
}

/// Argumentos: cada uno es un grupo que va o falta entero.
fn arguments(v: &Value, key: &str, inputs: &mut Vec<Input>) -> Option<Vec<Vec<Template>>> {
    let mut out = Vec::new();
    for a in list(v, key) {
        let name = text(a, "name").unwrap_or_default();
        match text(a, "type") {
            Some("positional") => {
                let label = text(a, "valueHint").unwrap_or("valor");
                out.push(vec![value(a, label, false, inputs)]);
            }
            Some("named") if name.starts_with('-') => {
                let val = value(a, name, false, inputs);
                if let Some(n) = name.strip_suffix('=') {
                    let mut t = vec![Ok(format!("{n}="))];
                    t.extend(val);
                    out.push(vec![t]);
                } else {
                    out.push(vec![vec![Ok(name.to_string())], val]);
                }
            }
            // Lo que no se entiende no se adivina.
            _ => return None,
        }
    }
    Some(out)
}

fn plain(s: &str) -> Vec<Template> {
    vec![vec![Ok(s.to_string())]]
}

fn package(p: &Value) -> Option<Choice> {
    if p.pointer("/transport/type").and_then(Value::as_str) != Some("stdio") {
        return None;
    }
    let id = text(p, "identifier")?;
    let version = text(p, "version");
    let fits = |s: &str, extra: &str| {
        !s.starts_with('-')
            && s.len() < 200
            && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
    };
    if !fits(id, "@._/:+-") || !version.is_none_or(|v| fits(v, ".+_-")) {
        return None;
    }
    // Un repositorio de paquetes distinto del habitual no se puede expresar.
    let base = text(p, "registryBaseUrl").unwrap_or_default();
    let usual = |b: &[&str]| base.is_empty() || b.iter().any(|u| base.trim_end_matches('/') == *u);
    let hint = text(p, "runtimeHint");
    let mut inputs = Vec::new();
    let runtime = arguments(p, "runtimeArguments", &mut inputs)?;
    let tail = arguments(p, "packageArguments", &mut inputs)?;
    let mut env = Vec::new();
    for e in list(p, "environmentVariables") {
        let name = text(e, "name").filter(|n| n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))?;
        // Variables que cambian qué programa se ejecuta.
        if name.starts_with("DYLD_") || matches!(name, "PATH" | "NODE_OPTIONS" | "PYTHONPATH" | "LD_PRELOAD")
        {
            return None;
        }
        env.push((name.to_string(), value(e, name, false, &mut inputs)));
    }
    let mut args: Vec<Vec<Template>> = Vec::new();
    let (label, command, needs) = match text(p, "registryType")? {
        "npm" if usual(&["https://registry.npmjs.org"]) && hint.is_none_or(|h| h == "npx") => {
            args.push(plain("-y"));
            args.extend(runtime);
            args.push(plain(&version.map_or(id.to_string(), |v| format!("{id}@{v}"))));
            ("npm", "npx", "npx")
        }
        "pypi"
            if usual(&["https://pypi.org", "https://pypi.org/simple"]) && hint.is_none_or(|h| h == "uvx") =>
        {
            args.extend(runtime);
            args.push(plain(&version.map_or(id.to_string(), |v| format!("{id}@{v}"))));
            ("Python", "uvx", "uvx")
        }
        "oci" if hint.is_none_or(|h| h == "docker") => {
            args.push(plain("run"));
            args.push(plain("-i"));
            args.push(plain("--rm"));
            args.extend(runtime);
            // El contenedor solo ve las variables que se le pasan.
            for (n, t) in &env {
                let mut g = plain("-e");
                g.push(vec![Ok(n.clone())]);
                // Falta junto con su variable.
                if t.iter().any(|x| x.is_err()) {
                    g.push(t.iter().filter(|x| x.is_err()).cloned().collect());
                }
                args.push(g);
            }
            let image = match version {
                Some(v) if !id.rsplit('/').next().unwrap_or(id).contains([':', '@']) => format!("{id}:{v}"),
                _ => id.to_string(),
            };
            args.push(plain(&image));
            ("Docker", "docker", "docker")
        }
        _ => return None,
    };
    args.extend(tail);
    Some(Choice { label, needs: Some(needs), inputs, command, args, env, url: None, headers: Vec::new() })
}

fn remote(r: &Value) -> Option<Choice> {
    if !matches!(text(r, "type"), Some("streamable-http" | "http")) {
        return None;
    }
    let mut inputs = Vec::new();
    let url = template(text(r, "url")?, r, true, &mut inputs);
    let mut headers = Vec::new();
    for h in list(r, "headers") {
        let name =
            text(h, "name").filter(|n| n.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)))?;
        let label = if name.eq_ignore_ascii_case("authorization") { "Clave" } else { name };
        headers.push((name.to_string(), value(h, label, false, &mut inputs)));
    }
    Some(Choice {
        label: "En internet",
        needs: None,
        inputs,
        command: "",
        args: Vec::new(),
        env: Vec::new(),
        url: Some(url),
        headers,
    })
}

impl Choice {
    /// Configuración con los datos rellenos (uno por `inputs`, en orden).
    pub fn build(&self, values: &[String]) -> Result<Config, String> {
        let get = |i: usize| values.get(i).map(|s| s.trim()).unwrap_or_default();
        for (i, x) in self.inputs.iter().enumerate() {
            let v = get(i);
            if x.required && v.is_empty() {
                return Err(format!("Falta «{}»", x.label));
            }
            if v.chars().any(char::is_control) {
                return Err(format!("«{}» no es válido", x.label));
            }
            let simple = |c: char| c.is_ascii_alphanumeric() || "-_.".contains(c);
            if x.strict && !v.chars().all(simple) {
                return Err(format!("«{}» solo puede llevar letras, números, guiones y puntos", x.label));
            }
        }
        // `None`: le falta un dato opcional.
        let fill = |t: &Template| -> Option<String> {
            t.iter()
                .map(|p| match p {
                    Ok(s) => Some(s.as_str()),
                    Err(i) => Some(get(*i)).filter(|v| !v.is_empty()),
                })
                .collect::<Option<Vec<_>>>()
                .map(|v| v.concat())
        };
        if let Some(u) = &self.url {
            let url = fill(u).ok_or("Falta un dato de la dirección")?;
            if !url.starts_with("https://") {
                return Err("La dirección publicada no es segura (https)".into());
            }
            web::check(&url)?;
            let mut headers = BTreeMap::new();
            for (n, t) in &self.headers {
                let Some(mut v) = fill(t) else { continue };
                // «Bearer» se suele olvidar al pegar una clave.
                if n.eq_ignore_ascii_case("authorization") && !v.contains(' ') {
                    v = format!("Bearer {v}");
                }
                headers.insert(n.clone(), v);
            }
            return Ok(Config { url: Some(url), headers, ..Config::default() });
        }
        let mut args = Vec::new();
        for group in &self.args {
            let Some(g) = group.iter().map(&fill).collect::<Option<Vec<_>>>() else { continue };
            // En Docker, `-e NOMBRE` lleva detrás su valor solo para saber si falta.
            let keep = if self.needs == Some("docker") && g.len() == 3 && g[0] == "-e" { 2 } else { g.len() };
            args.extend(g.into_iter().take(keep));
        }
        let env = self.env.iter().filter_map(|(n, t)| Some((n.clone(), fill(t)?))).collect();
        Ok(Config { command: self.command.into(), args, env, ..Config::default() })
    }
}

fn entry(v: &Value) -> Option<Entry> {
    let official = v.pointer("/_meta/io.modelcontextprotocol.registry~1official");
    if official.and_then(|o| text(o, "status")).is_some_and(|s| s != "active") {
        return None;
    }
    let s = v.get("server")?;
    let id = text(s, "name").filter(|n| n.len() < 200 && n.contains('/'))?;
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c)) {
        return None;
    }
    let mut choices: Vec<Choice> = list(s, "remotes").filter_map(remote).collect();
    choices.truncate(1);
    for p in list(s, "packages").filter_map(package) {
        if !choices.iter().any(|c| c.label == p.label) {
            choices.push(p);
        }
    }
    let repo = s
        .pointer("/repository/url")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("https://") && web::check(u).is_ok())
        .map(str::to_string);
    let server = id.rsplit('/').next().unwrap_or(id);
    Some(Entry {
        id: id.to_string(),
        title: clean(text(s, "title").unwrap_or(server), 60),
        about: clean(text(s, "description").unwrap_or_default(), 400),
        publisher: publisher(id),
        repo,
        name: short_name(id),
        choices,
    })
}

/// Cuánto encaja con lo buscado.
fn score(e: &Entry, q: &str) -> i32 {
    let (ns, server) = e.id.split_once('/').unwrap_or(("", &e.id));
    let (ns, server) = (ns.to_lowercase(), server.to_lowercase());
    let words: Vec<&str> = server.split(['-', '_', '.']).collect();
    let mut s = 0;
    // Publicado por el propio servicio: `com.notion`, `io.github.github`.
    if ns.rsplit('.').next() == Some(q) {
        s += 100;
    }
    if e.name == q {
        s += 40;
    } else if words.contains(&q) {
        s += 25;
    } else if server.contains(q) {
        s += 10;
    }
    if e.title.to_lowercase().contains(q) {
        s += 8;
    }
    if e.about.to_lowercase().contains(q) {
        s += 4;
    }
    if e.choices.is_empty() {
        s -= 60;
    }
    s
}

/// Busca en el registro. Solo se llama cuando el usuario lo pide: lo que
/// escribe se envía a su web.
pub fn search(query: &str) -> Result<Vec<Entry>, String> {
    let q = query.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join("-");
    if q.chars().count() < 3 {
        return Err("Escribe al menos tres letras".into());
    }
    if q.len() > 60 || !q.chars().all(|c| c.is_alphanumeric() || "-_./".contains(c)) {
        return Err("Busca por el nombre: letras, números y guiones".into());
    }
    // El registro ordena por nombre: se pregunta primero por lo más parecido.
    let asks: Vec<String> = [format!(".{q}/"), format!("/{q}"), format!("-{q}"), q.clone()]
        .iter()
        .map(|s| format!("{BASE}?search={}&limit=100&version=latest", crate::oauth::encode(s)))
        .collect();
    let handles: Vec<_> = asks.into_iter().map(|u| std::thread::spawn(move || page(&u))).collect();
    let (mut found, mut error): (Vec<Entry>, _) = (Vec::new(), None);
    let mut seen = HashSet::new();
    for h in handles {
        match h.join().unwrap_or_else(|_| Err("ha fallado la búsqueda".into())) {
            Ok(list) => found.extend(list.into_iter().filter(|e| seen.insert(e.id.clone()))),
            Err(e) => error = Some(e),
        }
    }
    if let (true, Some(e)) = (found.is_empty(), error) {
        return Err(e);
    }
    let last = q.rsplit('/').next().unwrap_or(&q).to_string();
    found.sort_by_key(|e| (-score(e, &last), e.id.clone()));
    // `io.github` está en casi todos: lo que no encaja en nada, fuera.
    found.retain(|e| score(e, &last) > 0);
    found.truncate(RESULTS);
    Ok(found)
}

fn page(url: &str) -> Result<Vec<Entry>, String> {
    let mut r = web::agent()
        .get(url)
        .header("Accept", "application/json")
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(25)))
        .build()
        .call()
        .map_err(|e| web::explain(&e, url))?;
    if !r.status().is_success() {
        return Err(format!("el registro contesta con un error {}", r.status().as_u16()));
    }
    let t = r.body_mut().with_config().limit(web::BODY_MAX * 2).read_to_string();
    let v: Value = t
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or("el registro no contesta como se esperaba")?;
    Ok(list(&v, "servers").filter_map(entry).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn server(v: Value) -> Entry {
        entry(&json!({"server": v})).unwrap()
    }

    #[test]
    fn who_and_names() {
        assert_eq!(publisher("io.github.ana/x"), "github.com/ana");
        assert_eq!(publisher("com.notion/mcp"), "notion.com");
        for (id, n) in [
            ("io.github.github/github-mcp-server", "github"),
            ("com.notion/mcp", "notion"),
            ("app.linear/linear", "linear"),
            ("io.github.x/mcp-server-fetch", "fetch"),
        ] {
            assert_eq!(short_name(id), n);
        }
        assert_eq!(clean("a\u{202e}b\n c\u{200b}", 20), "ab c");
    }

    #[test]
    fn remote_with_key() {
        let e = server(json!({"name": "io.github.github/github-mcp-server", "title": "GitHub", "remotes": [
            {"type": "sse", "url": "https://a/sse"},
            {"type": "streamable-http", "url": "https://api.x.com/mcp/", "headers": [
                {"name": "Authorization", "isSecret": true, "description": "Token"}, {"name": "X-Modo", "value": "lectura"}]}]}));
        let c = &e.choices[0];
        assert_eq!((c.label, c.inputs.len(), c.inputs[0].label.as_str()), ("En internet", 1, "Clave"));
        assert!(c.inputs[0].secret);
        let cfg = c.build(&["abc".into()]).unwrap();
        assert_eq!(cfg.headers["Authorization"], "Bearer abc");
        assert_eq!(cfg.headers["X-Modo"], "lectura");
        // Opcional: sin ella no va.
        assert!(!c.build(&["".into()]).unwrap().headers.contains_key("Authorization"));
        assert_eq!(c.build(&["Basic x".into()]).unwrap().headers["Authorization"], "Basic x");
    }

    #[test]
    fn variables_in_the_address_are_tight() {
        let e = server(json!({"name": "a.b/c", "remotes": [{"type": "streamable-http",
            "url": "https://{region}.x.com/{otra}", "variables": {"region": {"description": "Zona", "default": "eu"}}}]}));
        let c = &e.choices[0];
        assert_eq!(c.inputs[0].default, "eu");
        assert_eq!(c.build(&["eu".into()]).unwrap().url.unwrap(), "https://eu.x.com/{otra}");
        for bad in ["malo.com/", "a@b", "a b", "", "x#"] {
            assert!(c.build(&[bad.into()]).is_err(), "{bad}");
        }
        let http =
            server(json!({"name": "a.b/c", "remotes": [{"type": "streamable-http", "url": "http://x.com"}]}));
        assert!(http.choices[0].build(&[]).is_err());
    }

    #[test]
    fn packages() {
        let e = server(json!({"name": "a.b/pg", "packages": [
            {"registryType": "npm", "identifier": "@k/pg", "version": "0.1.1", "transport": {"type": "stdio"},
             "packageArguments": [
                {"type": "positional", "valueHint": "database_url", "isRequired": true, "isSecret": true},
                {"type": "named", "name": "--modo", "value": "{m}", "variables": {"m": {"choices": ["a", "b"]}}},
                {"type": "named", "name": "--fijo", "value": "1"}],
             "environmentVariables": [{"name": "API_KEY", "isSecret": true, "isRequired": true}, {"name": "OPC"}]},
            {"registryType": "oci", "identifier": "ghcr.io/k/pg:1", "transport": {"type": "stdio"},
             "environmentVariables": [{"name": "API_KEY", "isRequired": true}, {"name": "OPC"}]},
            {"registryType": "pypi", "identifier": "pg", "version": "2", "runtimeHint": "uvx", "transport": {"type": "stdio"}},
            {"registryType": "mcpb", "identifier": "https://x/y.mcpb", "transport": {"type": "stdio"}}]}));
        let labels: Vec<_> = e.choices.iter().map(|c| c.label).collect();
        assert_eq!(labels, ["npm", "Docker", "Python"]);
        let npm = &e.choices[0];
        let names: Vec<_> = npm.inputs.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(names, ["database_url", "m", "API_KEY", "OPC"]);
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let c = npm.build(&v(&["postgres://x y", "", "k", ""])).unwrap();
        assert_eq!(c.line(), "npx -y @k/pg@0.1.1 'postgres://x y' --fijo 1");
        assert_eq!(c.env.len(), 1);
        assert!(npm.build(&v(&["", "", "k", ""])).unwrap_err().contains("database_url"));
        let d = e.choices[1].build(&v(&["k", ""])).unwrap();
        assert_eq!(d.line(), "docker run -i --rm -e API_KEY ghcr.io/k/pg:1");
        assert_eq!(e.choices[2].build(&[]).unwrap().line(), "uvx pg@2");
    }

    #[test]
    fn refuses_what_it_does_not_understand() {
        let pk = |extra: Value| {
            let mut p = json!({"registryType": "npm", "identifier": "x", "transport": {"type": "stdio"}});
            for (k, v) in extra.as_object().unwrap() {
                p[k] = v.clone();
            }
            package(&p).is_some()
        };
        assert!(pk(json!({})));
        assert!(!pk(json!({"identifier": "--malo"})));
        assert!(!pk(json!({"identifier": "x; rm"})));
        assert!(!pk(json!({"version": "1 --registry=https://malo"})));
        assert!(!pk(json!({"registryBaseUrl": "https://malo.com"})));
        assert!(!pk(json!({"runtimeHint": "/tmp/malo"})));
        assert!(!pk(json!({"transport": {"type": "streamable-http"}})));
        assert!(!pk(json!({"runtimeArguments": [{"type": "raro"}]})));
        assert!(!pk(
            json!({"environmentVariables": [{"name": "NODE_OPTIONS", "value": "--require /tmp/x"}]})
        ));
        assert!(entry(&json!({"server": {"name": "a.b/c"}, "_meta": {"io.modelcontextprotocol.registry/official": {"status": "deprecated"}}})).is_none());
        assert!(entry(&json!({"server": {"name": "sin barra"}})).is_none());
    }

    #[test]
    fn the_service_itself_comes_first() {
        let mut v: Vec<Entry> = [
            "com.thing/github-mcp",
            "io.github.ana/github",
            "io.github.github/github-mcp-server",
            "io.github.bob/otra",
        ]
        .iter()
        .map(|n| server(json!({"name": n, "remotes": [{"type": "streamable-http", "url": "https://x"}]})))
        .collect();
        v.sort_by_key(|e| -score(e, "github"));
        assert_eq!(v[0].id, "io.github.github/github-mcp-server");
        assert!(score(&v[3], "github") <= 0, "{}", v[3].id);
    }

    /// Con red: `cargo test -p karbyx-agent registry_live -- --ignored`.
    #[test]
    #[ignore]
    fn registry_live() {
        for (q, first) in [
            ("github", "io.github.github/github-mcp-server"),
            ("notion", "com.notion/mcp"),
            ("linear", "app.linear/linear"),
        ] {
            let r = search(q).unwrap();
            eprintln!("{q}: {:?}", r.iter().take(5).map(|e| (&e.id, e.choices.len())).collect::<Vec<_>>());
            assert_eq!(r[0].id, first);
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! API de los servidores de modelos (la de OpenAI, que hablan llama.cpp,
//! Strata, vLLM, LM Studio...): descubrir modelos y conversar en streaming.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::http::{self, Base, HttpError, Request};

/// Un servidor de modelos del usuario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Server {
    pub name: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub id: String,
    /// Nombre legible, sin ruta ni `.gguf`.
    pub name: String,
    /// Tokens de contexto, si el servidor lo dice.
    pub ctx: Option<u32>,
    /// Si la plantilla del modelo admite herramientas (`None`: no se sabe).
    pub tools: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    Cancelled,
    /// Mensaje para el usuario.
    Failed(String),
    /// La conversación no cabe en el contexto del modelo.
    TooLong(String),
}

impl From<HttpError> for ApiError {
    fn from(e: HttpError) -> Self {
        match e {
            HttpError::Cancelled => ApiError::Cancelled,
            e => ApiError::Failed(e.to_string()),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ApiError::Cancelled => write!(f, "cancelado"),
            ApiError::Failed(m) | ApiError::TooLong(m) => write!(f, "{m}"),
        }
    }
}

/// `../models/Qwen3.8-Q4_K_M.gguf` → `Qwen3.8-Q4_K_M`.
pub fn pretty_model(id: &str) -> String {
    let base = id.rsplit(['/', '\\']).next().unwrap_or(id);
    base.strip_suffix(".gguf").unwrap_or(base).to_string()
}

fn error_text(status: u16, body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or_default();
    let msg = v
        .pointer("/error/message")
        .or_else(|| v.get("error"))
        .or_else(|| v.get("message"))
        .or_else(|| v.get("detail"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match status {
        401 | 403 => "El servidor pide una clave (o la que tienes no vale)".into(),
        503 if msg.to_lowercase().contains("loading") => "El servidor aún está cargando el modelo".into(),
        _ if !msg.is_empty() => msg,
        _ => format!("El servidor contesta con un error {status}"),
    }
}

fn get(base: &Base, key: Option<&str>, path: &str, secs: u64) -> Result<Value, ApiError> {
    let req =
        Request { base, method: "GET", path, key, body: None, timeout: Some(Duration::from_secs(secs)) };
    let mut r = http::send(&req, &|| false)?;
    let body = r.text(4 << 20)?;
    if r.status != 200 {
        return Err(ApiError::Failed(error_text(r.status, &body)));
    }
    serde_json::from_str(&body).map_err(|_| ApiError::Failed("Hay un servidor, pero no es de modelos".into()))
}

/// Pregunta al servidor qué modelos tiene y cuánto contexto.
pub fn probe(server: &Server) -> Result<Vec<Model>, ApiError> {
    let base = Base::parse(&server.url).map_err(ApiError::Failed)?;
    let key = server.key.as_deref();
    let list = get(&base, key, "/v1/models", 8)?;
    let data = list
        .get("data")
        .and_then(Value::as_array)
        .ok_or(ApiError::Failed("Hay un servidor, pero no es de modelos".into()))?;
    // Propio de llama.cpp: el contexto real y si admite herramientas.
    let props = get(&base, key, "/props", 8).unwrap_or_default();
    let num = |v: Option<&Value>| v.and_then(Value::as_u64).map(|n| n as u32).filter(|n| *n > 0);
    let ctx = num(props.pointer("/default_generation_settings/n_ctx"));
    let tools = props.pointer("/chat_template_caps/supports_tools").and_then(Value::as_bool);
    let one = data.len() == 1;
    let mut models: Vec<Model> = data
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_string();
            let own = num(m.pointer("/meta/n_ctx"))
                .or_else(|| num(m.get("max_model_len")))
                .or_else(|| num(m.get("context_length")))
                .or_else(|| num(m.get("context_window")));
            Some(Model {
                name: pretty_model(&id),
                id,
                ctx: if one { ctx.or(own) } else { own },
                tools: if one { tools } else { None },
            })
        })
        .collect();
    models.dedup_by(|a, b| a.id == b.id);
    if models.is_empty() {
        return Err(ApiError::Failed("El servidor no tiene ningún modelo cargado".into()));
    }
    Ok(models)
}

/// Petición de herramienta del modelo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Call {
    pub id: String,
    pub name: String,
    /// JSON tal cual lo escribió el modelo (puede no ser válido).
    pub args: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Msg {
    System(String),
    User(String),
    Assistant { text: String, reasoning: String, calls: Vec<Call> },
    Tool { id: String, name: String, text: String },
}

impl Msg {
    fn json(&self) -> Value {
        match self {
            Msg::System(t) => json!({"role": "system", "content": t}),
            Msg::User(t) => json!({"role": "user", "content": t}),
            Msg::Tool { id, name, text } => {
                json!({"role": "tool", "tool_call_id": id, "name": name, "content": text})
            }
            Msg::Assistant { text, reasoning, calls } => {
                let mut m = json!({"role": "assistant", "content": text});
                // Los modelos que razonan siguen el hilo entre herramientas.
                if !reasoning.is_empty() && !calls.is_empty() {
                    m["reasoning_content"] = json!(reasoning);
                }
                if !calls.is_empty() {
                    m["tool_calls"] = calls
                        .iter()
                        .map(|c| {
                            // Un JSON inválido haría fallar la plantilla del servidor.
                            let ok = serde_json::from_str::<Value>(&c.args).is_ok_and(|v| v.is_object());
                            let args = if ok { c.args.as_str() } else { "{}" };
                            json!({"id": c.id, "type": "function",
                                   "function": {"name": c.name, "arguments": args}})
                        })
                        .collect();
                }
                m
            }
        }
    }

    /// Tamaño aproximado, en caracteres.
    pub fn size(&self) -> usize {
        match self {
            Msg::System(t) | Msg::User(t) => t.len(),
            Msg::Tool { text, .. } => text.len(),
            Msg::Assistant { text, reasoning, calls } => {
                text.len()
                    + reasoning.len()
                    + calls.iter().map(|c| c.args.len() + c.name.len()).sum::<usize>()
            }
        }
    }
}

/// Lo que va llegando.
#[derive(Debug, PartialEq)]
pub enum Delta<'a> {
    Text(&'a str),
    Reasoning(&'a str),
    /// El modelo está escribiendo una llamada a herramienta: nombre y bytes.
    Calling(&'a str, usize),
}

/// Cifras de una respuesta.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Stats {
    pub prompt: u32,
    pub output: u32,
    /// Tokens por segundo al escribir, según el servidor.
    pub speed: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reply {
    pub text: String,
    pub reasoning: String,
    pub calls: Vec<Call>,
    /// Se cortó por llegar al límite de tokens.
    pub cut: bool,
    pub stats: Option<Stats>,
}

/// Separa `<think>…</think>` del texto cuando el servidor no lo hace.
#[derive(Default)]
struct Think {
    inside: bool,
    /// Ya salió texto normal: un `<think>` posterior es literal.
    started: bool,
    held: String,
}

impl Think {
    /// Devuelve (razonamiento, texto) listos; retiene una etiqueta a medias.
    fn feed(&mut self, s: &str, end: bool) -> (String, String) {
        self.held.push_str(s);
        let (mut think, mut text) = (String::new(), String::new());
        loop {
            let tag = if self.inside { "</think>" } else { "<think>" };
            if !self.inside && self.started {
                text.push_str(&std::mem::take(&mut self.held));
                break;
            }
            if !self.inside {
                // Solo cuenta al principio de la respuesta.
                let t = self.held.trim_start();
                if let Some(rest) = t.strip_prefix(tag) {
                    self.held = rest.to_string();
                    self.inside = true;
                    continue;
                }
                if !end && tag.starts_with(t) {
                    break;
                }
                self.started = true;
                continue;
            }
            if let Some(i) = self.held.find(tag) {
                think.push_str(&self.held[..i]);
                self.held = self.held[i + tag.len()..].trim_start().to_string();
                self.inside = false;
                self.started = true;
                continue;
            }
            // Retiene un posible comienzo de `</think>`.
            let keep = if end {
                0
            } else {
                (1..tag.len()).rev().find(|n| self.held.ends_with(&tag[..*n])).unwrap_or(0)
            };
            let cut = self.held.len() - keep;
            think.push_str(&self.held[..cut]);
            self.held.drain(..cut);
            break;
        }
        (think, text)
    }
}

pub struct Chat<'a> {
    pub server: &'a Server,
    pub model: &'a str,
    pub messages: &'a [Msg],
    /// Definiciones de herramientas (lista vacía: ninguna).
    pub tools: &'a [Value],
    /// Tokens máximos de la respuesta (`None`: los que quepan).
    pub max_tokens: Option<u32>,
}

/// Pide la siguiente respuesta; `on` recibe lo que va llegando.
pub fn chat(c: &Chat, cancelled: &dyn Fn() -> bool, on: &mut dyn FnMut(Delta)) -> Result<Reply, ApiError> {
    let base = Base::parse(&c.server.url).map_err(ApiError::Failed)?;
    let mut body = json!({
        "model": c.model,
        "messages": c.messages.iter().map(Msg::json).collect::<Vec<_>>(),
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !c.tools.is_empty() {
        body["tools"] = json!(c.tools);
    }
    if let Some(n) = c.max_tokens {
        body["max_tokens"] = json!(n);
    }
    let body = body.to_string();
    let req = Request {
        base: &base,
        method: "POST",
        path: "/v1/chat/completions",
        key: c.server.key.as_deref(),
        body: Some(&body),
        timeout: None,
    };
    let mut r = http::send(&req, cancelled)?;
    if r.status != 200 {
        let body = r.text(1 << 20).unwrap_or_default();
        return Err(classify(error_text(r.status, &body)));
    }
    let mut st = Stream::default();
    let mut line = Vec::new();
    while let Some(bytes) = r.chunk()? {
        for b in bytes {
            if b != b'\n' {
                line.push(b);
                continue;
            }
            let l = String::from_utf8_lossy(&line).into_owned();
            line.clear();
            if st.line(l.trim_end(), on)? {
                return Ok(st.finish(on));
            }
        }
    }
    let l = String::from_utf8_lossy(&line).into_owned();
    st.line(l.trim_end(), on)?;
    if !st.any {
        return Err(ApiError::Failed("El servidor no ha contestado nada".into()));
    }
    Ok(st.finish(on))
}

fn classify(msg: String) -> ApiError {
    let l = msg.to_lowercase();
    if (l.contains("context") && (l.contains("exceed") || l.contains("size") || l.contains("length")))
        || l.contains("too long")
    {
        ApiError::TooLong(msg)
    } else {
        ApiError::Failed(msg)
    }
}

/// Estado de una respuesta en streaming (SSE).
#[derive(Default)]
struct Stream {
    reply: Reply,
    think: Think,
    any: bool,
}

impl Stream {
    /// Procesa una línea SSE. `true`: se acabó.
    fn line(&mut self, l: &str, on: &mut dyn FnMut(Delta)) -> Result<bool, ApiError> {
        let Some(data) = l.strip_prefix("data:").map(str::trim) else {
            // Algunos servidores mandan el error sin envolver.
            if let Some(e) = l.strip_prefix("error:") {
                return Err(classify(error_text(500, e.trim())));
            }
            return Ok(false);
        };
        if data == "[DONE]" {
            return Ok(true);
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else { return Ok(false) };
        if v.get("error").is_some_and(|e| !e.is_null()) {
            return Err(classify(error_text(500, data)));
        }
        self.any = true;
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
            let s = self.reply.stats.get_or_insert_default();
            s.prompt = n("prompt_tokens");
            s.output = n("completion_tokens");
        }
        if let Some(t) = v.get("timings").and_then(|t| t.get("predicted_per_second")).and_then(Value::as_f64)
        {
            self.reply.stats.get_or_insert_default().speed = Some(t as f32);
        }
        let Some(choice) = v.pointer("/choices/0") else { return Ok(false) };
        if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
            self.reply.cut = f == "length";
        }
        let Some(d) = choice.get("delta").or_else(|| choice.get("message")) else { return Ok(false) };
        for k in ["reasoning_content", "reasoning"] {
            if let Some(t) = d.get(k).and_then(Value::as_str).filter(|t| !t.is_empty()) {
                self.reply.reasoning.push_str(t);
                on(Delta::Reasoning(t));
                break;
            }
        }
        if let Some(t) = d.get("content").and_then(Value::as_str).filter(|t| !t.is_empty()) {
            let parts = self.think.feed(t, false);
            self.push(parts, on);
        }
        for c in d.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
            self.call(c, on);
        }
        Ok(false)
    }

    fn push(&mut self, (think, text): (String, String), on: &mut dyn FnMut(Delta)) {
        if !think.is_empty() {
            self.reply.reasoning.push_str(&think);
            on(Delta::Reasoning(&think));
        }
        if !text.is_empty() {
            self.reply.text.push_str(&text);
            on(Delta::Text(&text));
        }
    }

    fn call(&mut self, c: &Value, on: &mut dyn FnMut(Delta)) {
        let calls = &mut self.reply.calls;
        let id = c.get("id").and_then(Value::as_str).filter(|s| !s.is_empty());
        let name = c.pointer("/function/name").and_then(Value::as_str).filter(|s| !s.is_empty());
        // Sin índice: un id nuevo empieza otra llamada.
        let i = match c.get("index").and_then(Value::as_u64) {
            Some(i) => (i as usize).min(calls.len()),
            None if calls.is_empty() => 0,
            None if id.is_some_and(|id| calls.last().is_some_and(|l| l.id != id)) => calls.len(),
            None => calls.len() - 1,
        };
        if i == calls.len() {
            calls.push(Call::default());
        }
        let call = &mut calls[i];
        if let Some(id) = id {
            call.id = id.to_string();
        }
        if let Some(n) = name
            && call.name.is_empty()
        {
            call.name = n.to_string();
        }
        match c.pointer("/function/arguments") {
            Some(Value::String(s)) => call.args.push_str(s),
            Some(v @ Value::Object(_)) => call.args = v.to_string(),
            _ => {}
        }
        on(Delta::Calling(&call.name, call.args.len()));
    }

    fn finish(mut self, on: &mut dyn FnMut(Delta)) -> Reply {
        let parts = self.think.feed("", true);
        self.push(parts, on);
        let mut r = self.reply;
        for (i, c) in r.calls.iter_mut().enumerate() {
            if c.id.is_empty() {
                c.id = format!("call_{i}");
            }
            if c.args.trim().is_empty() {
                c.args = "{}".into();
            }
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> (Result<Reply, ApiError>, Vec<String>) {
        let mut seen = Vec::new();
        let mut st = Stream::default();
        let mut on = |d: Delta| seen.push(format!("{d:?}"));
        for l in lines {
            match st.line(l, &mut on) {
                Ok(true) => break,
                Ok(false) => {}
                Err(e) => return (Err(e), seen),
            }
        }
        let r = st.finish(&mut on);
        (Ok(r), seen)
    }

    #[test]
    fn text_reasoning_and_stats() {
        let (r, _) = run(&[
            r#"data: {"choices":[{"delta":{"role":"assistant","content":null}}]}"#,
            "",
            r#"data: {"choices":[{"delta":{"reasoning_content":"pienso"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"ho"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"la"},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3},"timings":{"predicted_per_second":41.5}}"#,
            "data: [DONE]",
        ]);
        let r = r.unwrap();
        assert_eq!((r.text.as_str(), r.reasoning.as_str(), r.cut), ("hola", "pienso", false));
        assert_eq!(r.stats, Some(Stats { prompt: 12, output: 3, speed: Some(41.5) }));
    }

    #[test]
    fn tool_calls_in_pieces_and_whole() {
        let (r, _) = run(&[
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"read","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"x\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"list"}}]}}]}"#,
        ]);
        let r = r.unwrap();
        assert_eq!(r.calls[0], Call { id: "a".into(), name: "read".into(), args: r#"{"path":"x"}"#.into() });
        assert_eq!(r.calls[1], Call { id: "call_1".into(), name: "list".into(), args: "{}".into() });
        // Sin índice y con los argumentos como objeto.
        let (r, _) = run(&[
            r#"data: {"choices":[{"delta":{"tool_calls":[{"id":"a","function":{"name":"x","arguments":{"k":1}}},{"id":"b","function":{"name":"y","arguments":"{}"}}]}}]}"#,
        ]);
        let r = r.unwrap();
        assert_eq!(r.calls.len(), 2);
        assert_eq!(r.calls[0].args, r#"{"k":1}"#);
    }

    #[test]
    fn think_tags() {
        let (r, _) = run(&[
            r#"data: {"choices":[{"delta":{"content":"<th"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"ink>a</thi"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"nk>\n\nhola <think> literal"}}]}"#,
        ]);
        let r = r.unwrap();
        assert_eq!((r.reasoning.as_str(), r.text.as_str()), ("a", "hola <think> literal"));
        let (r, _) = run(&[r#"data: {"choices":[{"delta":{"content":"a < b"}}]}"#]);
        assert_eq!(r.unwrap().text, "a < b");
        let (r, _) = run(&[r#"data: {"choices":[{"delta":{"content":"<"}}]}"#]);
        assert_eq!(r.unwrap().text, "<");
    }

    #[test]
    fn errors() {
        let e = r#"data: {"error":{"code":400,"message":"the request exceeds the available context size"}}"#;
        assert!(matches!(run(&[e]).0, Err(ApiError::TooLong(_))));
        assert!(matches!(run(&[r#"data: {"error":{"message":"boom"}}"#]).0, Err(ApiError::Failed(_))));
        assert_eq!(
            error_text(503, r#"{"error":{"message":"Loading model"}}"#),
            "El servidor aún está cargando el modelo"
        );
    }

    #[test]
    fn history_json() {
        let m = Msg::Assistant {
            text: String::new(),
            reasoning: "r".into(),
            calls: vec![Call { id: "a".into(), name: "read".into(), args: "{mal".into() }],
        };
        let j = m.json();
        assert_eq!(j.pointer("/tool_calls/0/function/arguments").unwrap(), "{}");
        assert_eq!(j["reasoning_content"], "r");
        assert_eq!(pretty_model("..\\models\\Qwen-Q4.gguf"), "Qwen-Q4");
    }
}

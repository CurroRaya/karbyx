// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 CurroRaya y los colaboradores de forge

//! Gemini CLI (Google): historial en `~/.gemini/tmp/<proyecto>/chats/session-*.json`.
//!
//! Cada archivo es un JSON completo con `sessionId` y una lista `messages`;
//! las respuestas del modelo llevan `model` y `tokens`
//! (`input`, `output`, `cached`, `thoughts`, `tool`, `total`).
//! El archivo se reescribe entero, así que se relee cuando cambia y se
//! deduplica por id de mensaje.
//!
//! En la API de Gemini el prompt *incluye* el contenido cacheado, y los
//! "thoughts" se facturan como salida.
//!
//! Experimental: implementado a partir de la estructura conocida del formato.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::provider::Provider;
use crate::record::{Tokens, UsageRecord};

pub(crate) fn parse_file(file_key: &str, content: &str) -> Vec<UsageRecord> {
    let Ok(v) = serde_json::from_str::<Value>(content) else { return Vec::new() };
    let session = v.get("sessionId").and_then(Value::as_str).unwrap_or(file_key).to_string();
    let project = v.get("projectHash").and_then(Value::as_str).unwrap_or("").to_string();
    let Some(messages) = v.get("messages").and_then(Value::as_array) else { return Vec::new() };

    messages
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let t = m.get("tokens").filter(|t| t.is_object())?;
            let n = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
            let ts = m
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())?
                .with_timezone(&Utc);
            let cached = n("cached");
            let tokens = Tokens {
                input: n("input").saturating_sub(cached) + n("tool"),
                output: n("output") + n("thoughts"),
                cache_write_5m: 0,
                cache_write_1h: 0,
                cache_read: cached,
            };
            if tokens.total() == 0 {
                return None;
            }
            let id = m.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| i.to_string());
            Some(UsageRecord {
                provider: Provider::Gemini,
                timestamp: ts,
                model: m.get("model").and_then(Value::as_str).unwrap_or("gemini").to_string(),
                project: project.clone(),
                session_id: session.clone(),
                tokens,
                dedupe_key: format!("gemini:{session}:{id}"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_model_messages() {
        let json = r#"{
          "sessionId": "abc", "projectHash": "h1",
          "messages": [
            {"id":"u1","timestamp":"2026-11-22T10:00:00Z","type":"user","content":"hola"},
            {"id":"g1","timestamp":"2026-11-22T10:00:05Z","type":"gemini","model":"gemini-2.5-pro",
             "tokens":{"input":1000,"output":50,"cached":400,"thoughts":30,"tool":5,"total":1085}}
          ]}"#;
        let r = parse_file("f", json);
        assert_eq!(r.len(), 1);
        let r = &r[0];
        assert_eq!(r.provider, Provider::Gemini);
        assert_eq!(r.model, "gemini-2.5-pro");
        assert_eq!(r.tokens.input, 605); // 1000 − 400 + 5
        assert_eq!(r.tokens.output, 80); // 50 + 30
        assert_eq!(r.tokens.cache_read, 400);
        assert_eq!(r.dedupe_key, "gemini:abc:g1");
    }

    #[test]
    fn tolerates_garbage() {
        assert!(parse_file("f", "{").is_empty());
        assert!(parse_file("f", r#"{"messages": 3}"#).is_empty());
    }
}

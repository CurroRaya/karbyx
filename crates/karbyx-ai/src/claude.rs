// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! Claude Code: parser tolerante de las líneas JSONL de `~/.claude/projects`.
//!
//! El formato no es una API pública y evoluciona entre versiones, así que todo
//! es opcional: una línea desconocida o corrupta se ignora, nunca falla.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::provider::Provider;
use crate::record::{Tokens, UsageRecord};

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<DateTime<Utc>>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation: Option<CacheCreation>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CacheCreation {
    ephemeral_5m_input_tokens: u64,
    ephemeral_1h_input_tokens: u64,
}

/// Convierte una línea JSONL en un [`UsageRecord`] si es una respuesta del
/// asistente con datos de uso. Devuelve `None` en cualquier otro caso.
pub fn parse_line(line: &str) -> Option<UsageRecord> {
    let line = line.trim();
    if line.is_empty() || !line.contains("\"usage\"") {
        return None; // atajo barato: la mayoría de líneas no tienen uso
    }
    let l: Line = serde_json::from_str(line).ok()?;
    if l.kind.as_deref() != Some("assistant") {
        return None;
    }
    let msg = l.message?;
    let u = msg.usage?;
    let model = msg.model.unwrap_or_else(|| "unknown".into());
    if model == "<synthetic>" {
        return None; // mensajes generados localmente, sin coste
    }

    // Desglose de escritura en caché 5m / 1h. Versiones antiguas solo traen el total.
    let (w5, w1h) = match u.cache_creation {
        Some(c) if c.ephemeral_5m_input_tokens + c.ephemeral_1h_input_tokens > 0 => {
            (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens)
        }
        _ => (u.cache_creation_input_tokens, 0),
    };

    let tokens = Tokens {
        input: u.input_tokens,
        output: u.output_tokens,
        cache_write_5m: w5,
        cache_write_1h: w1h,
        cache_read: u.cache_read_input_tokens,
    };
    if tokens.total() == 0 {
        return None;
    }

    let msg_id = msg.id.unwrap_or_default();
    let req_id = l.request_id.unwrap_or_default();
    if msg_id.is_empty() && req_id.is_empty() {
        return None; // sin forma de deduplicar: mejor no contar dos veces
    }

    Some(UsageRecord {
        provider: Provider::Claude,
        timestamp: l.timestamp?,
        model,
        project: l.cwd.unwrap_or_default(),
        session_id: l.session_id.unwrap_or_default(),
        tokens,
        dedupe_key: format!("{msg_id}:{req_id}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"type":"assistant","timestamp":"2026-11-22T19:00:01.097Z","requestId":"req_1","sessionId":"s1","cwd":"/Users/x/Rust","message":{"model":"claude-opus-5-5","id":"msg_1","usage":{"input_tokens":2,"cache_creation_input_tokens":23212,"cache_read_input_tokens":45241,"output_tokens":7056,"cache_creation":{"ephemeral_1h_input_tokens":23212,"ephemeral_5m_input_tokens":0}}}}"#;

    #[test]
    fn parses_assistant_usage() {
        let r = parse_line(SAMPLE).expect("record");
        assert_eq!(r.model, "claude-opus-5-5");
        assert_eq!(r.tokens.input, 2);
        assert_eq!(r.tokens.output, 7056);
        assert_eq!(r.tokens.cache_write_1h, 23212);
        assert_eq!(r.tokens.cache_write_5m, 0);
        assert_eq!(r.tokens.cache_read, 45241);
        assert_eq!(r.dedupe_key, "msg_1:req_1");
    }

    #[test]
    fn legacy_cache_total_goes_to_5m() {
        let line = SAMPLE.replace(
            r#","cache_creation":{"ephemeral_1h_input_tokens":23212,"ephemeral_5m_input_tokens":0}"#,
            "",
        );
        let r = parse_line(&line).unwrap();
        assert_eq!(r.tokens.cache_write_5m, 23212);
        assert_eq!(r.tokens.cache_write_1h, 0);
    }

    #[test]
    fn ignores_garbage_and_user_lines() {
        assert!(parse_line("").is_none());
        assert!(parse_line("{not json \"usage\"").is_none());
        assert!(parse_line(&SAMPLE.replace("\"assistant\"", "\"user\"")).is_none());
        assert!(parse_line(&SAMPLE.replace("claude-opus-5-5", "<synthetic>")).is_none());
    }
}

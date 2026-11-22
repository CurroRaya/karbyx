// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Codex CLI (OpenAI): historial en `~/.codex/sessions/AAAA/MM/DD/rollout-*.jsonl`.
//!
//! Cada archivo es una sesión. Las líneas relevantes:
//! - `session_meta` / `turn_context`: id de sesión, `cwd` y modelo en uso.
//! - `event_msg` con `payload.type == "token_count"`: uso acumulado
//!   (`total_token_usage`), uso del último turno (`last_token_usage`) y los
//!   límites oficiales (`rate_limits.primary/secondary` con el % usado).
//!
//! En la API de OpenAI `input_tokens` *incluye* los tokens cacheados, así que
//! se separan: input fresco = input − cached.
//!
//! Experimental: el formato no es público y se ha implementado a partir de su
//! estructura conocida; cualquier campo ausente se ignora.

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::provider::Provider;
use crate::record::{RateLimits, RateWindow, Tokens, UsageRecord};

/// Estado por archivo: el modelo y el cwd llegan en líneas anteriores al uso.
#[derive(Debug, Default, Clone)]
pub(crate) struct FileCtx {
    model: Option<String>,
    cwd: Option<String>,
    session: Option<String>,
    last_total: Option<u64>,
    prev: Option<RawUsage>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct RawUsage {
    input: u64,
    cached: u64,
    output: u64,
    total: u64,
}

impl RawUsage {
    fn from(v: &Value) -> Option<Self> {
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        if !v.is_object() {
            return None;
        }
        let input = n("input_tokens");
        let output = n("output_tokens");
        let total = v.get("total_tokens").and_then(Value::as_u64).unwrap_or(input + output);
        Some(Self { input, cached: n("cached_input_tokens"), output, total })
    }

    fn minus(self, o: Self) -> Self {
        Self {
            input: self.input.saturating_sub(o.input),
            cached: self.cached.saturating_sub(o.cached),
            output: self.output.saturating_sub(o.output),
            total: self.total.saturating_sub(o.total),
        }
    }

    fn tokens(self) -> Tokens {
        Tokens {
            input: self.input.saturating_sub(self.cached),
            output: self.output,
            cache_write_5m: 0,
            cache_write_1h: 0,
            cache_read: self.cached,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct LineOut {
    pub record: Option<UsageRecord>,
    pub limits: Option<RateLimits>,
}

fn rate_window(v: &Value, now: DateTime<Utc>) -> Option<RateWindow> {
    let used = v.get("used_percent").and_then(Value::as_f64)?;
    let minutes = v.get("window_minutes").and_then(Value::as_u64).unwrap_or(0);
    let resets_at =
        v.get("resets_at").and_then(Value::as_i64).and_then(|s| Utc.timestamp_opt(s, 0).single()).or_else(
            || v.get("resets_in_seconds").and_then(Value::as_i64).map(|s| now + chrono::Duration::seconds(s)),
        );
    Some(RateWindow { window_minutes: minutes, used_percent: used, resets_at })
}

pub(crate) fn parse_line(ctx: &mut FileCtx, file_key: &str, line: &str) -> LineOut {
    let mut out = LineOut::default();
    let line = line.trim();
    if line.is_empty() {
        return out;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else { return out };
    let payload = v.get("payload").unwrap_or(&Value::Null);
    let str_of = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).map(str::to_string);

    match v.get("type").and_then(Value::as_str) {
        Some("session_meta") => {
            ctx.session = str_of(payload, "id").or(ctx.session.take());
            ctx.cwd = str_of(payload, "cwd").or(ctx.cwd.take());
        }
        Some("turn_context") => {
            ctx.model = str_of(payload, "model").or(ctx.model.take());
            ctx.cwd = str_of(payload, "cwd").or(ctx.cwd.take());
        }
        Some("event_msg") if payload.get("type").and_then(Value::as_str) == Some("token_count") => {
            let Some(ts) = v
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
            else {
                return out;
            };

            if let Some(rl) = payload.get("rate_limits").filter(|r| r.is_object()) {
                let windows: Vec<RateWindow> = ["primary", "secondary"]
                    .iter()
                    .filter_map(|k| rl.get(*k).and_then(|w| rate_window(w, ts)))
                    .collect();
                if !windows.is_empty() {
                    out.limits = Some(RateLimits { observed_at: ts, windows });
                }
            }

            let info = payload.get("info").unwrap_or(&Value::Null);
            let total = info.get("total_token_usage").and_then(RawUsage::from);
            // Codex repite el mismo evento a veces: si el acumulado no cambia, no hay uso nuevo.
            if let Some(t) = total
                && ctx.last_total == Some(t.total)
            {
                return out;
            }
            let turn = info
                .get("last_token_usage")
                .and_then(RawUsage::from)
                .or_else(|| total.map(|t| t.minus(ctx.prev.unwrap_or_default())));
            if let Some(t) = total {
                ctx.last_total = Some(t.total);
                ctx.prev = Some(t);
            }
            let Some(turn) = turn else { return out };
            let tokens = turn.tokens();
            if tokens.total() == 0 {
                return out;
            }
            let marker = total.map(|t| t.total.to_string()).unwrap_or_else(|| ts.to_rfc3339());
            out.record = Some(UsageRecord {
                provider: Provider::Codex,
                timestamp: ts,
                model: ctx.model.clone().unwrap_or_else(|| "codex".into()),
                project: ctx.cwd.clone().unwrap_or_default(),
                session_id: ctx.session.clone().unwrap_or_default(),
                tokens,
                dedupe_key: format!("codex:{file_key}:{marker}"),
            });
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: &str = r#"{"timestamp":"2026-11-22T10:00:00Z","type":"session_meta","payload":{"id":"s1","cwd":"/Users/x/proj"}}"#;
    const CTX: &str = r#"{"timestamp":"2026-11-22T10:00:01Z","type":"turn_context","payload":{"model":"gpt-5-codex","cwd":"/Users/x/proj"}}"#;

    fn token_count(total_in: u64, cached: u64, out: u64, last: Option<(u64, u64, u64)>) -> String {
        let last = last
            .map(|(i, c, o)| {
                format!(r#","last_token_usage":{{"input_tokens":{i},"cached_input_tokens":{c},"output_tokens":{o},"total_tokens":{}}}"#, i + o)
            })
            .unwrap_or_default();
        format!(
            r#"{{"timestamp":"2026-11-22T10:01:00Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{total_in},"cached_input_tokens":{cached},"output_tokens":{out},"total_tokens":{}}}{last}}},"rate_limits":{{"primary":{{"used_percent":42.5,"window_minutes":300,"resets_in_seconds":600}},"secondary":{{"used_percent":10.0,"window_minutes":10080}}}}}}}}"#,
            total_in + out
        )
    }

    #[test]
    fn parses_turns_limits_and_dedups() {
        let mut ctx = FileCtx::default();
        assert!(parse_line(&mut ctx, "f", META).record.is_none());
        assert!(parse_line(&mut ctx, "f", CTX).record.is_none());

        let o = parse_line(&mut ctx, "f", &token_count(1000, 600, 200, Some((1000, 600, 200))));
        let r = o.record.expect("record");
        assert_eq!(r.provider, Provider::Codex);
        assert_eq!(r.model, "gpt-5-codex");
        assert_eq!(r.project, "/Users/x/proj");
        assert_eq!(r.tokens.input, 400); // 1000 − 600 cacheados
        assert_eq!(r.tokens.cache_read, 600);
        assert_eq!(r.tokens.output, 200);
        let l = o.limits.expect("limits");
        assert_eq!(l.windows.len(), 2);
        assert_eq!(l.windows[0].label(), "5 h");
        assert_eq!(l.windows[1].label(), "semana");
        assert!((l.windows[0].used_percent - 42.5).abs() < 1e-9);

        // Mismo acumulado repetido: sin registro nuevo.
        assert!(
            parse_line(&mut ctx, "f", &token_count(1000, 600, 200, Some((1000, 600, 200)))).record.is_none()
        );

        // Sin last_token_usage: se calcula la diferencia con el acumulado anterior.
        let r = parse_line(&mut ctx, "f", &token_count(1500, 600, 300, None)).record.unwrap();
        assert_eq!(r.tokens.input, 500);
        assert_eq!(r.tokens.output, 100);
        assert_eq!(r.tokens.cache_read, 0);
    }

    #[test]
    fn ignores_garbage() {
        let mut ctx = FileCtx::default();
        assert!(parse_line(&mut ctx, "f", "no json").record.is_none());
        assert!(
            parse_line(&mut ctx, "f", r#"{"type":"event_msg","payload":{"type":"agent_message"}}"#)
                .record
                .is_none()
        );
    }
}

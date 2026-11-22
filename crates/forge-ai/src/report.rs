// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 CurroRaya y los colaboradores de forge

//! Agregaciones sobre los registros de uso: totales, días, modelos, proyectos,
//! ventanas de 5 horas y semana en curso, con estimación de lo que queda.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Timelike, Utc, Weekday};
use serde::Serialize;

use crate::config::Limits;
use crate::pricing::PriceTable;
use crate::record::{Tokens, UsageRecord};

/// Duración de una ventana de uso de Claude.
pub const BLOCK_HOURS: i64 = 5;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Bucket {
    pub tokens: Tokens,
    pub cost_usd: f64,
    pub requests: u64,
    /// Hay peticiones de modelos sin precio conocido (coste infravalorado).
    pub has_unpriced: bool,
}

impl Bucket {
    fn add(&mut self, r: &UsageRecord, prices: &PriceTable) {
        self.tokens += r.tokens;
        self.requests += 1;
        match prices.cost(&r.model, &r.tokens) {
            Some(c) => self.cost_usd += c,
            None => self.has_unpriced = true,
        }
    }
}

/// Una ventana de 5 horas: empieza en la hora en punto del primer mensaje.
#[derive(Debug, Clone, Serialize)]
pub struct SessionBlock {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub first_activity: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub models: BTreeSet<String>,
    pub usage: Bucket,
}

impl SessionBlock {
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        now < self.end && now - self.last_activity < Duration::hours(BLOCK_HOURS)
    }
}

/// Estado de la ventana activa, con ritmo de consumo y proyección.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveBlock {
    pub block: SessionBlock,
    pub remaining: Duration,
    /// Tokens frescos (input + output + escritura de caché) por minuto.
    pub tokens_per_min: f64,
    pub cost_per_hour: f64,
    /// Tokens frescos proyectados al final de la ventana al ritmo actual.
    pub projected_tokens: u64,
    pub projected_cost: f64,
    pub limit: Option<LimitStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LimitStatus {
    pub limit: u64,
    pub used: u64,
    pub percent: f64,
    /// Cuándo se agotaría al ritmo actual (si ocurre antes del reinicio).
    pub exhausted_at: Option<DateTime<Utc>>,
    pub resets_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub generated_at: DateTime<Utc>,
    pub total: Bucket,
    pub today: Bucket,
    pub week: Bucket,
    pub week_start: DateTime<Utc>,
    pub week_limit: Option<LimitStatus>,
    pub by_day: BTreeMap<NaiveDate, Bucket>,
    pub by_model: BTreeMap<String, Bucket>,
    pub by_project: BTreeMap<String, Bucket>,
    pub blocks: Vec<SessionBlock>,
    pub active: Option<ActiveBlock>,
}

fn floor_hour(t: DateTime<Utc>) -> DateTime<Utc> {
    t.with_minute(0).and_then(|t| t.with_second(0)).and_then(|t| t.with_nanosecond(0)).unwrap_or(t)
}

/// Agrupa registros (ordenados por fecha) en ventanas de 5 horas.
pub fn build_blocks(records: &[UsageRecord], prices: &PriceTable) -> Vec<SessionBlock> {
    let span = Duration::hours(BLOCK_HOURS);
    let mut blocks: Vec<SessionBlock> = Vec::new();
    for r in records {
        let need_new = match blocks.last() {
            None => true,
            Some(b) => r.timestamp >= b.end || r.timestamp - b.last_activity >= span,
        };
        if need_new {
            let start = floor_hour(r.timestamp);
            blocks.push(SessionBlock {
                start,
                end: start + span,
                first_activity: r.timestamp,
                last_activity: r.timestamp,
                models: BTreeSet::new(),
                usage: Bucket::default(),
            });
        }
        let b = blocks.last_mut().expect("bloque recién creado");
        b.last_activity = r.timestamp;
        b.models.insert(r.model.clone());
        b.usage.add(r, prices);
    }
    blocks
}

/// Inicio (UTC) de la semana actual según el día de reinicio, en hora local.
pub fn week_start(now: DateTime<Utc>, reset: Weekday) -> DateTime<Utc> {
    let local = now.with_timezone(&Local);
    let back = (7 + local.weekday().num_days_from_monday() as i64 - reset.num_days_from_monday() as i64) % 7;
    let day = local.date_naive() - Duration::days(back);
    Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0).expect("medianoche válida"))
        .earliest()
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or(now)
}

/// Nombre corto de proyecto a partir del `cwd`: los dos últimos componentes.
pub fn project_label(cwd: &str) -> String {
    if cwd.is_empty() {
        return "(desconocido)".into();
    }
    let parts: Vec<&str> = cwd.trim_end_matches('/').rsplit('/').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

fn limit_status(
    limit: u64,
    used: u64,
    rate_per_min: f64,
    now: DateTime<Utc>,
    resets_at: DateTime<Utc>,
) -> LimitStatus {
    let percent = if limit == 0 { 100.0 } else { used as f64 * 100.0 / limit as f64 };
    let exhausted_at = if used >= limit {
        Some(now)
    } else if rate_per_min > 0.0 {
        let mins = (limit - used) as f64 / rate_per_min;
        let at = now + Duration::seconds((mins * 60.0) as i64);
        (at < resets_at).then_some(at)
    } else {
        None
    };
    LimitStatus { limit, used, percent, exhausted_at, resets_at }
}

/// Calcula el resumen completo.
pub fn summarize(
    records: &[UsageRecord],
    prices: &PriceTable,
    limits: &Limits,
    now: DateTime<Utc>,
) -> Summary {
    let today = now.with_timezone(&Local).date_naive();
    let wstart = week_start(now, limits.weekly_reset_day());

    let mut s = Summary {
        generated_at: now,
        total: Bucket::default(),
        today: Bucket::default(),
        week: Bucket::default(),
        week_start: wstart,
        week_limit: None,
        by_day: BTreeMap::new(),
        by_model: BTreeMap::new(),
        by_project: BTreeMap::new(),
        blocks: build_blocks(records, prices),
        active: None,
    };

    for r in records {
        let day = r.timestamp.with_timezone(&Local).date_naive();
        s.total.add(r, prices);
        s.by_day.entry(day).or_default().add(r, prices);
        s.by_model.entry(r.model.clone()).or_default().add(r, prices);
        s.by_project.entry(project_label(&r.project)).or_default().add(r, prices);
        if day == today {
            s.today.add(r, prices);
        }
        if r.timestamp >= wstart {
            s.week.add(r, prices);
        }
    }

    // Ventana activa y proyección.
    if let Some(b) = s.blocks.last().filter(|b| b.is_active(now)) {
        let elapsed_min = ((now - b.first_activity).num_seconds() as f64 / 60.0).max(1.0);
        let fresh = b.usage.tokens.billable_weighted();
        let tpm = fresh as f64 / elapsed_min;
        let cph = b.usage.cost_usd / elapsed_min * 60.0;
        let remaining = b.end - now;
        let rem_min = remaining.num_seconds() as f64 / 60.0;
        s.active = Some(ActiveBlock {
            block: b.clone(),
            remaining,
            tokens_per_min: tpm,
            cost_per_hour: cph,
            projected_tokens: fresh + (tpm * rem_min) as u64,
            projected_cost: b.usage.cost_usd + cph * rem_min / 60.0,
            limit: limits.block_tokens.map(|l| limit_status(l, fresh, tpm, now, b.end)),
        });
    }

    if let Some(l) = limits.weekly_tokens {
        let used = s.week.tokens.billable_weighted();
        let mins = ((now - wstart).num_seconds() as f64 / 60.0).max(1.0);
        s.week_limit = Some(limit_status(l, used, used as f64 / mins, now, wstart + Duration::days(7)));
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ts: &str, out: u64) -> UsageRecord {
        UsageRecord {
            provider: crate::provider::Provider::Claude,
            timestamp: ts.parse().unwrap(),
            model: "claude-opus-5-5".into(),
            project: "/Users/x/Rust/forge".into(),
            session_id: "s".into(),
            tokens: Tokens { output: out, ..Default::default() },
            dedupe_key: ts.into(),
        }
    }

    #[test]
    fn blocks_split_on_five_hours() {
        let p = PriceTable::default();
        let rs = vec![
            rec("2026-11-22T10:20:00Z", 1),
            rec("2026-11-22T14:59:00Z", 1), // misma ventana (10:00-15:00)
            rec("2026-11-22T15:00:00Z", 1), // nueva ventana
        ];
        let b = build_blocks(&rs, &p);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].start.to_rfc3339(), "2026-11-22T10:00:00+00:00");
        assert_eq!(b[0].usage.requests, 2);
        assert_eq!(b[1].start.to_rfc3339(), "2026-11-22T15:00:00+00:00");
    }

    #[test]
    fn active_block_and_limit() {
        let p = PriceTable::default();
        let rs = vec![rec("2026-11-22T10:00:00Z", 600)];
        let limits = Limits { block_tokens: Some(1200), ..Default::default() };
        let now: DateTime<Utc> = "2026-11-22T10:10:00Z".parse().unwrap();
        let s = summarize(&rs, &p, &limits, now);
        let a = s.active.expect("activa");
        assert!((a.tokens_per_min - 60.0).abs() < 1e-9);
        let l = a.limit.unwrap();
        assert!((l.percent - 50.0).abs() < 1e-9);
        // Quedan 600 tokens a 60/min: se agota a las 10:20.
        assert_eq!(l.exhausted_at.unwrap().to_rfc3339(), "2026-11-22T10:20:00+00:00");
    }

    #[test]
    fn project_labels() {
        assert_eq!(project_label("/Users/x/Rust/forge"), "Rust/forge");
        assert_eq!(project_label(""), "(desconocido)");
    }
}

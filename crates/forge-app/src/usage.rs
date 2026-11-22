// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Hilo en segundo plano que mantiene actualizado el consumo de los
//! asistentes de IA para el panel derecho. Solo tokens y tiempos: sin importes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use forge_ai::{LimitStatus, Provider, RateLimits, Scanner, UsageConfig, UsageRecord, summarize};

#[derive(Debug, Clone)]
pub struct LimitView {
    pub percent: f64,
    /// Tokens que quedan (solo con límites propios; los oficiales dan solo el %).
    pub left: Option<u64>,
    pub exhausted_at: Option<DateTime<Utc>>,
    /// Viene del propio proveedor (no de un límite configurado a mano).
    pub official: bool,
}

impl From<&LimitStatus> for LimitView {
    fn from(l: &LimitStatus) -> Self {
        Self {
            percent: l.percent,
            left: Some(l.limit.saturating_sub(l.used)),
            exhausted_at: l.exhausted_at,
            official: false,
        }
    }
}

/// Ventana de límite publicada por el proveedor (p. ej. Codex).
#[derive(Debug, Clone)]
pub struct OfficialLimit {
    pub label: String,
    pub percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct ActiveView {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub remaining: Duration,
    /// Tokens frescos: input + output + escritura de caché.
    pub tokens: u64,
    pub requests: u64,
    pub tokens_per_min: f64,
    pub projected: u64,
    pub limit: Option<LimitView>,
    pub models: Vec<(String, u64)>,
}

#[derive(Debug, Clone)]
pub struct BlockBar {
    pub start: DateTime<Utc>,
    pub tokens: u64,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct UsageView {
    pub provider: Provider,
    pub has_data: bool,
    pub active: Option<ActiveView>,
    pub today: u64,
    pub today_requests: u64,
    pub today_cache_read: u64,
    pub week: u64,
    pub week_start: Option<DateTime<Utc>>,
    pub week_limit: Option<LimitView>,
    pub has_limits: bool,
    pub official: Vec<OfficialLimit>,
    pub recent: Vec<BlockBar>,
}

/// Estado compartido con la UI.
#[derive(Debug, Clone, Default)]
pub struct UsageState {
    pub loaded: bool,
    pub views: Vec<UsageView>,
}

pub type SharedUsage = Arc<Mutex<UsageState>>;

/// Nombre corto del modelo: `claude-opus-5-5` → `opus 5.5`; el resto, tal cual.
pub fn short_model(m: &str) -> String {
    match m.strip_prefix("claude-") {
        Some(rest) => match rest.split_once('-') {
            Some((family, ver)) => format!("{family} {}", ver.replace('-', ".")),
            None => rest.to_string(),
        },
        None => m.to_string(),
    }
}

fn models_since(records: &[UsageRecord], since: DateTime<Utc>) -> Vec<(String, u64)> {
    let mut by: BTreeMap<String, u64> = BTreeMap::new();
    for r in records.iter().rev().take_while(|r| r.timestamp >= since) {
        *by.entry(short_model(&r.model)).or_default() += r.tokens.billable_weighted();
    }
    let mut v: Vec<_> = by.into_iter().collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.1));
    v
}

fn official_limits(rl: Option<&RateLimits>, now: DateTime<Utc>) -> Vec<OfficialLimit> {
    let Some(rl) = rl else { return Vec::new() };
    rl.windows
        .iter()
        .map(|w| {
            // Si la ventana ya se reinició desde la última observación, el uso es 0.
            let expired = w.resets_at.is_some_and(|t| t <= now);
            OfficialLimit {
                label: w.label(),
                percent: if expired { 0.0 } else { w.used_percent },
                resets_at: w.resets_at.filter(|_| !expired),
            }
        })
        .collect()
}

pub fn build_view(
    provider: Provider,
    records: &[UsageRecord],
    limits: Option<&RateLimits>,
    cfg: &UsageConfig,
    now: DateTime<Utc>,
) -> UsageView {
    let lim = cfg.limits_for(provider);
    let s = summarize(records, &cfg.price_table(), &lim, now);
    let official = official_limits(limits, now);
    // La ventana corta oficial (5 h) sirve como límite de la ventana actual si
    // no hay uno configurado a mano.
    let official_block = official.iter().find(|o| o.label == "5 h").map(|o| LimitView {
        percent: o.percent,
        left: None,
        exhausted_at: None,
        official: true,
    });
    let active = s.active.as_ref().map(|a| ActiveView {
        start: a.block.start,
        end: a.block.end,
        remaining: a.remaining,
        tokens: a.block.usage.tokens.billable_weighted(),
        requests: a.block.usage.requests,
        tokens_per_min: a.tokens_per_min,
        projected: a.projected_tokens,
        limit: a.limit.as_ref().map(LimitView::from).or(official_block),
        models: models_since(records, a.block.start),
    });
    let active_start = s.active.as_ref().map(|a| a.block.start);
    let recent = s
        .blocks
        .iter()
        .rev()
        .take(8)
        .rev()
        .map(|b| BlockBar {
            start: b.start,
            tokens: b.usage.tokens.billable_weighted(),
            active: Some(b.start) == active_start,
        })
        .collect();
    UsageView {
        provider,
        has_data: !records.is_empty(),
        active,
        today: s.today.tokens.billable_weighted(),
        today_requests: s.today.requests,
        today_cache_read: s.today.tokens.cache_read,
        week: s.week.tokens.billable_weighted(),
        week_start: Some(s.week_start),
        week_limit: s.week_limit.as_ref().map(LimitView::from),
        has_limits: lim.is_set() || !official.is_empty(),
        official,
        recent,
    }
}

fn publish(out: &SharedUsage, selected: &[Provider], scanner: &Scanner, cfg: &UsageConfig) {
    let now = Utc::now();
    let views =
        selected.iter().map(|p| build_view(*p, scanner.records(*p), scanner.limits(*p), cfg, now)).collect();
    if let Ok(mut g) = out.lock() {
        *g = UsageState { loaded: true, views };
    }
}

/// Arranca el hilo; llama a `wake` cada vez que hay datos nuevos.
pub fn spawn(wake: Arc<dyn Fn() + Send + Sync>) -> SharedUsage {
    let shared: SharedUsage = Arc::new(Mutex::new(UsageState::default()));
    let out = shared.clone();
    std::thread::Builder::new()
        .name("ai-usage".into())
        .spawn(move || {
            let mut selected: Vec<Provider> = Vec::new();
            let mut scanner = Scanner::new(&[]);
            let mut providers_cfg: Option<Vec<String>> = None;
            let mut detected_at = std::time::Instant::now();
            loop {
                // La config se relee en cada vuelta: cambiar proveedores o
                // límites no exige reiniciar forge.
                let cfg = UsageConfig::load(None).unwrap_or_default();
                // Detectar recorre el historial: solo al arrancar, si cambia la
                // config o una vez por minuto.
                let stale = detected_at.elapsed() > StdDuration::from_secs(60);
                if providers_cfg.as_ref() != Some(&cfg.providers) || stale {
                    providers_cfg = Some(cfg.providers.clone());
                    detected_at = std::time::Instant::now();
                    let now_selected = cfg.selected();
                    if now_selected != selected {
                        scanner = Scanner::new(&now_selected);
                        selected = now_selected;
                    }
                }
                scanner.scan();
                publish(&out, &selected, &scanner, &cfg);
                wake();
                std::thread::sleep(StdDuration::from_secs(5));
            }
        })
        .expect("hilo de consumo");
    shared
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_ai::RateWindow;

    #[test]
    fn short_model_names() {
        assert_eq!(short_model("claude-opus-5-5"), "opus 5.5");
        assert_eq!(short_model("claude-sonnet-5"), "sonnet 5");
        assert_eq!(short_model("gpt-5-codex"), "gpt-5-codex");
    }

    #[test]
    fn expired_official_window_resets() {
        let now: DateTime<Utc> = "2026-11-22T12:00:00Z".parse().unwrap();
        let rl = RateLimits {
            observed_at: now,
            windows: vec![
                RateWindow {
                    window_minutes: 300,
                    used_percent: 40.0,
                    resets_at: Some(now - Duration::minutes(1)),
                },
                RateWindow {
                    window_minutes: 10_080,
                    used_percent: 15.0,
                    resets_at: Some(now + Duration::days(2)),
                },
            ],
        };
        let o = official_limits(Some(&rl), now);
        assert_eq!(o[0].label, "5 h");
        assert_eq!(o[0].percent, 0.0);
        assert_eq!(o[1].label, "semana");
        assert_eq!(o[1].percent, 15.0);
    }
}

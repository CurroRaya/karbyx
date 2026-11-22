// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Registro normalizado de uso de tokens de una petición a Claude.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::provider::Provider;

/// Tokens consumidos por una petición (o agregados de varias).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
}

impl Tokens {
    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m + self.cache_write_1h
    }

    /// Todos los tokens, incluyendo caché.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_write() + self.cache_read
    }

    /// Tokens "frescos" (sin lecturas de caché), métrica más cercana a lo que
    /// pesa en los límites de uso.
    pub fn billable_weighted(&self) -> u64 {
        self.input + self.output + self.cache_write()
    }
}

impl std::ops::AddAssign for Tokens {
    fn add_assign(&mut self, o: Self) {
        self.input += o.input;
        self.output += o.output;
        self.cache_write_5m += o.cache_write_5m;
        self.cache_write_1h += o.cache_write_1h;
        self.cache_read += o.cache_read;
    }
}

impl std::ops::Add for Tokens {
    type Output = Tokens;
    fn add(mut self, o: Self) -> Self {
        self += o;
        self
    }
}

/// Una petición al modelo extraída del historial de un asistente.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub provider: Provider,
    pub timestamp: DateTime<Utc>,
    pub model: String,
    /// Directorio de trabajo de la sesión (`cwd`), si existe.
    pub project: String,
    pub session_id: String,
    pub tokens: Tokens,
    /// Clave para deduplicar: un mismo mensaje puede aparecer en varias líneas.
    pub dedupe_key: String,
}

/// Una ventana de límite tal como la publica el propio proveedor
/// (p. ej. Codex escribe el % usado de su ventana de 5 h y de la semanal).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateWindow {
    pub window_minutes: u64,
    pub used_percent: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

impl RateWindow {
    /// Etiqueta legible: "5 h", "semana", "24 h"...
    pub fn label(&self) -> String {
        match self.window_minutes {
            10_080 => "semana".into(),
            m if m % 1440 == 0 && m > 0 => format!("{} días", m / 1440),
            m if m % 60 == 0 && m > 0 => format!("{} h", m / 60),
            m => format!("{m} min"),
        }
    }
}

/// Últimos límites oficiales observados de un proveedor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimits {
    pub observed_at: DateTime<Utc>,
    pub windows: Vec<RateWindow>,
}

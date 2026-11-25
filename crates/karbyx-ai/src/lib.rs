// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! `karbyx-ai`: consumo de tokens de asistentes de IA (Claude Code, Codex CLI,
//! Gemini CLI).
//!
//! Lee el historial que cada asistente guarda en disco, lo normaliza a
//! [`UsageRecord`]s comunes, deduplica, estima coste y calcula ventanas de
//! 5 horas, semana en curso y proyecciones. No usa la red.

mod claude;
mod codex;
pub mod config;
pub mod format;
mod gemini;
pub mod pricing;
pub mod provider;
pub mod record;
pub mod report;
pub mod scanner;

pub use config::{Limits, UsageConfig};
pub use pricing::{ModelPrice, PriceTable};
pub use provider::Provider;
pub use record::{RateLimits, RateWindow, Tokens, UsageRecord};
pub use report::{ActiveBlock, Bucket, LimitStatus, SessionBlock, Summary, summarize};
pub use scanner::Scanner;

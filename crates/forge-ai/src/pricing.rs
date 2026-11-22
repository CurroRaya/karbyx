// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Tabla de precios (USD por millón de tokens) para estimar el coste.
//!
//! Si usas una suscripción (Pro/Max), el coste es el *equivalente en API*: lo
//! que habrías pagado con API key. Sirve para comparar, no es tu factura.
//!
//! Los valores por defecto son los precios públicos de la API de Anthropic a
//! noviembre de 2026. Se pueden sobrescribir en `~/.config/forge/usage.toml`.

use serde::{Deserialize, Serialize};

use crate::record::Tokens;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    /// Prefijo del id del modelo (`claude-opus-5-5`). Gana el prefijo más largo.
    pub prefix: String,
    pub input: f64,
    pub output: f64,
    /// Escritura en caché de 5 min. Por defecto 1.25 × input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_5m: Option<f64>,
    /// Escritura en caché de 1 h. Por defecto 2 × input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<f64>,
    /// Lectura de caché. Por defecto 0.1 × input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
}

impl ModelPrice {
    fn new(prefix: &str, input: f64, output: f64, cache_read: Option<f64>) -> Self {
        Self { prefix: prefix.into(), input, output, cache_write_5m: None, cache_write_1h: None, cache_read }
    }

    /// Coste en USD de un conjunto de tokens.
    pub fn cost(&self, t: &Tokens) -> f64 {
        let w5 = self.cache_write_5m.unwrap_or(self.input * 1.25);
        let w1h = self.cache_write_1h.unwrap_or(self.input * 2.0);
        let rd = self.cache_read.unwrap_or(self.input * 0.1);
        (t.input as f64 * self.input
            + t.output as f64 * self.output
            + t.cache_write_5m as f64 * w5
            + t.cache_write_1h as f64 * w1h
            + t.cache_read as f64 * rd)
            / 1_000_000.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceTable {
    pub models: Vec<ModelPrice>,
}

impl Default for PriceTable {
    fn default() -> Self {
        let m = ModelPrice::new;
        Self {
            models: vec![
                m("claude-fable-5-2", 8.0, 40.0, Some(0.20)),
                m("claude-mythos-5-2", 8.0, 40.0, Some(0.20)),
                m("claude-fable-5", 10.0, 50.0, None),
                m("claude-mythos-5", 10.0, 50.0, None),
                m("claude-opus-5-5", 4.0, 20.0, Some(0.20)),
                m("claude-opus-5", 5.0, 25.0, None),
                m("claude-opus-4", 5.0, 25.0, None),
                m("claude-sonnet-5-5", 2.0, 10.0, Some(0.10)),
                m("claude-sonnet-5", 2.0, 10.0, None),
                m("claude-sonnet-4", 3.0, 15.0, None),
                m("claude-haiku-5-5", 0.10, 0.50, None),
                m("claude-haiku-4", 1.0, 5.0, None),
            ],
        }
    }
}

impl PriceTable {
    /// Busca el precio del modelo por el prefijo más largo que encaje.
    pub fn lookup(&self, model: &str) -> Option<&ModelPrice> {
        self.models.iter().filter(|p| model.starts_with(&p.prefix)).max_by_key(|p| p.prefix.len())
    }

    /// Coste estimado; `None` si el modelo no está en la tabla.
    pub fn cost(&self, model: &str, t: &Tokens) -> Option<f64> {
        self.lookup(model).map(|p| p.cost(t))
    }

    /// Fusiona precios del usuario: sustituyen a los de igual prefijo.
    pub fn merge(&mut self, overrides: Vec<ModelPrice>) {
        for o in overrides {
            match self.models.iter_mut().find(|m| m.prefix == o.prefix) {
                Some(m) => *m = o,
                None => self.models.push(o),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins() {
        let t = PriceTable::default();
        assert_eq!(t.lookup("claude-opus-5-5").unwrap().input, 4.0);
        assert_eq!(t.lookup("claude-opus-5").unwrap().input, 5.0);
        assert_eq!(t.lookup("claude-opus-4-8").unwrap().input, 5.0);
        assert_eq!(t.lookup("claude-fable-5-1").unwrap().input, 10.0);
        assert!(t.lookup("gpt-9").is_none());
    }

    #[test]
    fn cost_math() {
        let t = PriceTable::default();
        let tok = Tokens {
            input: 1_000_000,
            output: 1_000_000,
            cache_write_5m: 0,
            cache_write_1h: 1_000_000,
            cache_read: 1_000_000,
        };
        // opus-5-5: 4 + 20 + 8 (2×4) + 0.20
        let c = t.cost("claude-opus-5-5", &tok).unwrap();
        assert!((c - 32.20).abs() < 1e-9, "{c}");
    }
}

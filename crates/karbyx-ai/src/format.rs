// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! Cómo se enseñan los números: igual en la app y en el comando.

use chrono::{DateTime, Duration, Local, Utc};

/// Tokens: «950», «1.2k», «33k», «1.5M», «12M», «1.25B».
pub fn tok(n: u64) -> String {
    // Una decimal hasta 9.9; a partir de ahí, redondo.
    let short = |v: f64, unit: &str| if v < 9.95 { format!("{v:.1}{unit}") } else { format!("{v:.0}{unit}") };
    match n {
        n if n >= 999_500_000 => format!("{:.2}B", n as f64 / 1e9),
        n if n >= 999_500 => short(n as f64 / 1e6, "M"),
        n if n >= 1_000 => short(n as f64 / 1e3, "k"),
        n => n.to_string(),
    }
}

/// «45 min», «2h 05m».
pub fn dur(d: Duration) -> String {
    let m = d.num_minutes().max(0);
    if m >= 60 { format!("{}h {:02}m", m / 60, m % 60) } else { format!("{m} min") }
}

/// Hora local: «16:05».
pub fn hm(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        for (n, t) in [
            (0, "0"),
            (999, "999"),
            (1_000, "1.0k"),
            (1_250, "1.2k"),
            (9_949, "9.9k"),
            (9_950, "10k"),
            (33_400, "33k"),
            (999_499, "999k"),
            (999_500, "1.0M"),
            (12_300_000, "12M"),
            (999_499_999, "999M"),
            (999_500_000, "1.00B"),
            (1_250_000_000, "1.25B"),
        ] {
            assert_eq!(tok(n), t, "{n}");
        }
    }

    #[test]
    fn durations() {
        assert_eq!(dur(Duration::minutes(-3)), "0 min");
        assert_eq!(dur(Duration::minutes(59)), "59 min");
        assert_eq!(dur(Duration::minutes(125)), "2h 05m");
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! `karbyx-usage`: consumo de tokens de asistentes de IA (Claude Code, Codex
//! CLI, Gemini CLI) desde la terminal.

use std::path::PathBuf;
use std::time::Duration as StdDuration;

use anyhow::Result;
use chrono::{Local, Utc};
use clap::{Parser, Subcommand};
use karbyx_ai::format::{dur, hm, tok};
use karbyx_ai::{Bucket, LimitStatus, Provider, Scanner, Summary, UsageConfig, summarize};
use owo_colors::OwoColorize;

#[derive(Parser)]
#[command(name = "karbyx-usage", version, about = "Consumo de tokens de asistentes de IA")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Solo este asistente: claude, codex o gemini (por defecto, los configurados o detectados).
    #[arg(short, long, global = true)]
    provider: Option<String>,
    /// Ruta de configuración (por defecto ~/.config/karbyx/usage.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Salida en JSON (para scripts, statusline o la propia terminal).
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Resumen: ventana actual, hoy, semana y total (por defecto).
    Summary,
    /// Consumo por día.
    Daily {
        #[arg(short, long, default_value_t = 14)]
        days: usize,
    },
    /// Ventanas de 5 horas.
    Blocks {
        #[arg(short, long, default_value_t = 10)]
        last: usize,
    },
    /// Consumo por modelo.
    Models,
    /// Consumo por proyecto.
    Projects {
        #[arg(short, long, default_value_t = 15)]
        top: usize,
    },
    /// Refresca el resumen cada N segundos.
    Watch {
        #[arg(short, long, default_value_t = 5)]
        interval: u64,
    },
    /// Una línea compacta, para statusline o prompt.
    Line,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = UsageConfig::load(cli.config.clone())?;
    let prices = cfg.price_table();
    let providers = match &cli.provider {
        Some(id) => vec![
            Provider::from_id(id)
                .ok_or_else(|| anyhow::anyhow!("asistente desconocido: {id} (usa claude, codex o gemini)"))?,
        ],
        None => cfg.selected(),
    };
    let mut scanner = Scanner::new(&providers);
    if !scanner.has_sources() {
        anyhow::bail!(
            "no encuentro historial de ningún asistente (Claude Code: ~/.claude, Codex: ~/.codex, Gemini: ~/.gemini)"
        );
    }

    let cmd = cli.cmd.unwrap_or(Cmd::Summary);
    let summaries = |scanner: &Scanner| -> Vec<(Provider, Summary)> {
        let now = Utc::now();
        providers
            .iter()
            .filter(|p| !scanner.records(**p).is_empty())
            .map(|p| (*p, summarize(scanner.records(*p), &prices, &cfg.limits_for(*p), now)))
            .collect()
    };

    if let Cmd::Watch { interval } = cmd {
        loop {
            scanner.scan();
            print!("\x1b[2J\x1b[H"); // limpia pantalla
            for (p, s) in summaries(&scanner) {
                print_summary(p, &s);
                println!();
            }
            println!("{}", format!("actualiza cada {interval}s · Ctrl+C para salir").dimmed());
            std::thread::sleep(StdDuration::from_secs(interval));
        }
    }

    scanner.scan();
    let all = summaries(&scanner);

    if cli.json {
        let map: std::collections::BTreeMap<&str, &Summary> = all.iter().map(|(p, s)| (p.id(), s)).collect();
        println!("{}", serde_json::to_string_pretty(&map)?);
        return Ok(());
    }
    if all.is_empty() {
        println!("Sin historial todavía.");
    }

    for (i, (p, s)) in all.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let title = |t: &str| format!("{} · {t}", p.name());
        match cmd {
            Cmd::Summary | Cmd::Watch { .. } => print_summary(*p, s),
            Cmd::Daily { days } => print_table(
                &title("por día"),
                s.by_day.iter().rev().take(days).map(|(d, b)| (d.format("%a %d %b").to_string(), b)),
            ),
            Cmd::Blocks { last } => print_blocks(&title("ventanas de 5 h"), s, last),
            Cmd::Models => print_table(&title("por modelo"), sorted_by_cost(&s.by_model, usize::MAX)),
            Cmd::Projects { top } => print_table(&title("por proyecto"), sorted_by_cost(&s.by_project, top)),
            Cmd::Line => println!("{}", one_line(*p, s)),
        }
    }
    Ok(())
}

// ---------- formato ----------

fn usd(b: &Bucket) -> String {
    format!("${:.2}{}", b.cost_usd, if b.has_unpriced { "*" } else { "" })
}

fn bar(pct: f64, width: usize) -> String {
    let filled = ((pct / 100.0).clamp(0.0, 1.0) * width as f64).round() as usize;
    let s = format!("{}{}", "█".repeat(filled), "░".repeat(width - filled));
    match pct {
        p if p >= 90.0 => s.red().to_string(),
        p if p >= 75.0 => s.yellow().to_string(),
        _ => s.green().to_string(),
    }
}

fn sorted_by_cost(
    m: &std::collections::BTreeMap<String, Bucket>,
    top: usize,
) -> impl Iterator<Item = (String, &Bucket)> {
    let mut v: Vec<_> = m.iter().collect();
    v.sort_by(|a, b| b.1.cost_usd.total_cmp(&a.1.cost_usd));
    v.into_iter().take(top).map(|(k, b)| (k.clone(), b))
}

fn print_limit(label: &str, l: &LimitStatus) {
    let left = l.limit.saturating_sub(l.used);
    println!(
        "  {label:<10} {} {:>5.1}%  quedan {} de {}",
        bar(l.percent, 24),
        l.percent,
        tok(left).bold(),
        tok(l.limit)
    );
    match l.exhausted_at {
        Some(t) => println!(
            "  {:<10} {}",
            "",
            format!("⚠ al ritmo actual se agota a las {} (reinicio {})", hm(t), hm(l.resets_at)).yellow()
        ),
        None => println!(
            "  {:<10} {}",
            "",
            format!("reinicia {}", l.resets_at.with_timezone(&Local).format("%a %H:%M")).dimmed()
        ),
    }
}

fn print_summary(p: Provider, s: &Summary) {
    println!("{}", format!("◆ {} · consumo", p.name()).bold().magenta());
    println!();
    match &s.active {
        Some(a) => {
            let b = &a.block;
            println!(
                "{}  {}–{}  ·  quedan {}",
                "Ventana actual".bold(),
                hm(b.start),
                hm(b.end),
                dur(a.remaining).cyan()
            );
            println!(
                "  {:<10} {} frescos · {} caché leída · {} · {} pet.",
                "usado",
                tok(b.usage.tokens.billable_weighted()).bold(),
                tok(b.usage.tokens.cache_read),
                usd(&b.usage).green(),
                b.usage.requests
            );
            println!(
                "  {:<10} {}/min · ${:.2}/h  →  proyección {} · ${:.2}",
                "ritmo",
                tok(a.tokens_per_min as u64),
                a.cost_per_hour,
                tok(a.projected_tokens),
                a.projected_cost
            );
            match &a.limit {
                Some(l) => print_limit("límite 5h", l),
                None => println!("  {}", "(define limits.block_tokens para ver lo que te queda)".dimmed()),
            }
        }
        None => println!("{}  {}", "Ventana actual".bold(), "sin actividad en las últimas 5 h".dimmed()),
    }
    println!();
    let row = |name: &str, b: &Bucket| {
        println!(
            "  {:<10} {:>9} frescos  {:>9} caché  {:>10}  {:>6} pet.",
            name,
            tok(b.tokens.billable_weighted()),
            tok(b.tokens.cache_read),
            usd(b),
            b.requests
        )
    };
    row("hoy", &s.today);
    row("semana", &s.week);
    if let Some(l) = &s.week_limit {
        print_limit("límite sem", l);
    }
    row("total", &s.total);
    if s.total.has_unpriced {
        println!("\n  {}", "* incluye modelos sin precio en la tabla".dimmed());
    }
}

fn print_table<'a>(title: &str, rows: impl Iterator<Item = (String, &'a Bucket)>) {
    println!("{}", title.bold());
    println!(
        "  {:<28} {:>9} {:>9} {:>9} {:>9} {:>10} {:>6}",
        "", "input", "output", "c.write", "c.read", "coste", "pet."
    );
    for (k, b) in rows {
        let t = b.tokens;
        println!(
            "  {:<28} {:>9} {:>9} {:>9} {:>9} {:>10} {:>6}",
            truncate(&k, 28),
            tok(t.input),
            tok(t.output),
            tok(t.cache_write()),
            tok(t.cache_read),
            usd(b),
            b.requests
        );
    }
}

fn print_blocks(title: &str, s: &Summary, last: usize) {
    println!("{}", title.bold());
    for b in s.blocks.iter().rev().take(last) {
        let active = s.active.as_ref().is_some_and(|a| a.block.start == b.start);
        println!(
            "  {} {}–{}  {:>9} frescos  {:>10}  {:>5} pet.  {}",
            if active { "●".green().to_string() } else { "○".dimmed().to_string() },
            b.start.with_timezone(&Local).format("%a %d %H:%M"),
            hm(b.end),
            tok(b.usage.tokens.billable_weighted()),
            usd(&b.usage),
            b.usage.requests,
            b.models.iter().map(|m| m.trim_start_matches("claude-")).collect::<Vec<_>>().join(",").dimmed()
        );
    }
}

fn one_line(p: Provider, s: &Summary) -> String {
    match &s.active {
        Some(a) => {
            let pct = a.limit.as_ref().map(|l| format!(" {:.0}%", l.percent)).unwrap_or_default();
            format!(
                "◆ {} {}{} · {} · {} restantes · hoy {}",
                p.name(),
                tok(a.block.usage.tokens.billable_weighted()),
                pct,
                usd(&a.block.usage),
                dur(a.remaining),
                usd(&s.today)
            )
        }
        None => format!("◆ {} inactivo · hoy {}", p.name(), usd(&s.today)),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n - 1).collect();
        format!("{t}…")
    }
}

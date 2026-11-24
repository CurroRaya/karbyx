// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Markdown de las respuestas: lo justo para que una respuesta de un modelo
//! se lea bien (párrafos, listas, tablas, código con colores...).

use crate::gfx::RichSpan;
use crate::ui::{self, Rgb};

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Para(Vec<RichSpan>),
    Heading(u8, Vec<RichSpan>),
    Item { depth: usize, marker: String, spans: Vec<RichSpan> },
    Quote(Vec<RichSpan>),
    Code { lang: String, text: String },
    Table(Vec<Vec<Vec<RichSpan>>>),
    Rule,
}

pub const INK: Rgb = [0xe4, 0xdc, 0xd2];
const CODE: Rgb = [0xf2, 0xb5, 0x84];
const LINK: Rgb = [0x8f, 0xb8, 0xd8];

pub fn span(text: impl Into<String>, color: Rgb) -> RichSpan {
    RichSpan { text: text.into(), color, bold: false, italic: false, mono: false }
}

pub fn mono(text: impl Into<String>, color: Rgb) -> RichSpan {
    RichSpan { mono: true, ..span(text, color) }
}

/// Énfasis, código y enlaces de una línea.
pub fn inline(s: &str, color: Rgb) -> Vec<RichSpan> {
    let mut out: Vec<RichSpan> = Vec::new();
    let (mut bold, mut italic) = (false, false);
    let mut cur = String::new();
    let mut rest = s;
    let mut prev = ' ';
    macro_rules! flush {
        () => {
            if !cur.is_empty() {
                let c = if bold { ui::TEXT } else { color };
                out.push(RichSpan { bold, italic, ..span(std::mem::take(&mut cur), c) });
            }
        };
    }
    while let Some(c) = rest.chars().next() {
        // Código: hasta las mismas comillas invertidas.
        if c == '`' {
            let n = rest.len() - rest.trim_start_matches('`').len();
            let (fence, body) = rest.split_at(n);
            if let Some(end) = body.find(fence) {
                flush!();
                out.push(mono(body[..end].trim(), CODE));
                rest = &body[end + n..];
                prev = '`';
                continue;
            }
        }
        if c == '\\' && rest[1..].starts_with(|c: char| c.is_ascii_punctuation()) {
            let esc = rest[1..].chars().next().unwrap_or('\\');
            cur.push(esc);
            rest = &rest[1 + esc.len_utf8()..];
            prev = esc;
            continue;
        }
        if c == '['
            && let Some((label, url, len)) = link(rest)
        {
            flush!();
            let shown = if label.is_empty() { url } else { label };
            out.push(span(shown, LINK));
            rest = &rest[len..];
            prev = ')';
            continue;
        }
        if c == '*' || c == '_' {
            let n = rest.chars().take_while(|x| *x == c).count().min(3);
            let after = rest[n..].chars().next().unwrap_or(' ');
            let word = |c: char| c.is_alphanumeric();
            let opens = !after.is_whitespace() && (c == '*' || !word(prev));
            let closes = !prev.is_whitespace() && (c == '*' || !word(after));
            let (b, i) = (n >= 2, n != 2);
            // Se abre si más adelante hay con qué cerrar.
            let can_open = opens && rest[n..].contains(&rest[..n]);
            let is_open = (!b || bold) && (!i || italic);
            if (closes && is_open) || (can_open && !(b && bold) && !(i && italic)) {
                flush!();
                bold ^= b;
                italic ^= i;
                rest = &rest[n..];
                prev = c;
                continue;
            }
            cur.push_str(&rest[..n]);
            rest = &rest[n..];
            prev = c;
            continue;
        }
        cur.push(c);
        prev = c;
        rest = &rest[c.len_utf8()..];
    }
    flush!();
    if out.is_empty() {
        out.push(span("", color));
    }
    out
}

/// `[texto](dirección)` al principio de `s`: (texto, dirección, bytes).
fn link(s: &str) -> Option<(&str, &str, usize)> {
    let close = s.find("](")?;
    let label = &s[1..close];
    if label.contains(['[', '\n']) {
        return None;
    }
    let end = s[close + 2..].find(')')?;
    Some((label, &s[close + 2..close + 2 + end], close + 2 + end + 1))
}

fn list_marker(t: &str) -> Option<(String, &str)> {
    for m in ["- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(m) {
            for (b, mark) in [("[ ] ", "☐"), ("[x] ", "☑"), ("[X] ", "☑")] {
                if let Some(r) = r.strip_prefix(b) {
                    return Some((mark.into(), r));
                }
            }
            return Some(("•".into(), r));
        }
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    let r = &t[digits..];
    if (1..=3).contains(&digits) && (r.starts_with(". ") || r.starts_with(") ")) {
        return Some((format!("{}.", &t[..digits]), &r[2..]));
    }
    None
}

fn is_rule(t: &str) -> bool {
    let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3 && ["-", "*", "_"].iter().any(|c| t.replace(c, "").is_empty())
}

fn cells(l: &str) -> Vec<&str> {
    l.trim().trim_start_matches('|').trim_end_matches('|').split('|').map(str::trim).collect()
}

fn is_table_rule(l: &str) -> bool {
    l.contains('-') && l.contains('|') && l.chars().all(|c| "|-: \t".contains(c))
}

pub fn parse(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut i = 0;
    macro_rules! flush {
        () => {
            if !para.is_empty() {
                // Los saltos de línea del modelo se respetan.
                out.push(Block::Para(inline(&para.join("\n"), INK)));
                para.clear();
            }
        };
    }
    while i < lines.len() {
        let l = lines[i];
        let t = l.trim_start();
        let indent = l.len() - t.len();
        if t.starts_with("```") || t.starts_with("~~~") {
            flush!();
            let mark = &t[..3];
            let n = t.len() - t.trim_start_matches(&mark[..1]).len();
            let fence = &t[..n];
            let lang = t[n..].split_whitespace().next().unwrap_or_default().to_string();
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && lines[i].trim() != fence {
                // Sangría de la propia valla (dentro de una lista).
                let b = lines[i];
                let cut = b.len() - b.trim_start_matches(' ').len();
                body.push(&b[cut.min(indent)..]);
                i += 1;
            }
            i += 1;
            out.push(Block::Code { lang, text: body.join("\n") });
            continue;
        }
        if t.is_empty() {
            flush!();
        } else if let Some(h) = heading(t) {
            flush!();
            out.push(Block::Heading(h.0, inline(h.1, ui::TEXT)));
        } else if is_rule(t) {
            flush!();
            out.push(Block::Rule);
        } else if let Some(q) = t.strip_prefix('>') {
            flush!();
            let mut q = vec![q.trim_start()];
            while let Some(n) = lines.get(i + 1).and_then(|l| l.trim_start().strip_prefix('>')) {
                q.push(n.trim_start());
                i += 1;
            }
            out.push(Block::Quote(inline(&q.join("\n"), ui::DIM)));
        } else if t.contains('|') && lines.get(i + 1).is_some_and(|n| is_table_rule(n)) {
            flush!();
            let mut rows = vec![cells(t).iter().map(|c| inline(c, ui::TEXT)).collect::<Vec<_>>()];
            let n = rows[0].len();
            i += 2;
            while i < lines.len() && lines[i].contains('|') {
                let mut row: Vec<_> = cells(lines[i]).iter().take(n).map(|c| inline(c, INK)).collect();
                row.resize(n, vec![span("", INK)]);
                rows.push(row);
                i += 1;
            }
            out.push(Block::Table(rows));
            continue;
        } else if let Some((marker, r)) = list_marker(t) {
            flush!();
            let mut body = vec![r];
            // Continuación del mismo punto, con sangría.
            while let Some(n) = lines.get(i + 1) {
                let nt = n.trim_start();
                if nt.is_empty()
                    || n.len() - nt.len() <= indent
                    || list_marker(nt).is_some()
                    || nt.starts_with("```")
                {
                    break;
                }
                body.push(nt);
                i += 1;
            }
            out.push(Block::Item { depth: (indent / 2).min(4), marker, spans: inline(&body.join(" "), INK) });
        } else {
            para.push(t);
        }
        i += 1;
    }
    flush!();
    out
}

fn heading(t: &str) -> Option<(u8, &str)> {
    let n = t.bytes().take_while(|b| *b == b'#').count();
    let r = t[n..].strip_prefix(' ')?;
    (1..=6).contains(&n).then(|| (n as u8, r.trim_end_matches('#').trim()))
}

// ---------- colores del código ----------

const C_TEXT: Rgb = [0xd8, 0xcf, 0xc4];
const C_KEY: Rgb = [0xf0, 0x8a, 0x4b];
const C_STR: Rgb = [0xa9, 0xc4, 0x7f];
const C_NUM: Rgb = [0xe9, 0xc0, 0x6e];
const C_COM: Rgb = [0x7a, 0x70, 0x66];
const C_FUN: Rgb = [0x8f, 0xb8, 0xd8];
const C_TYPE: Rgb = [0xd9, 0xa8, 0xc4];

const KEYWORDS: &str = " abstract and as assert async await begin bool boolean break by case catch char class const continue crate def default defer del delete do done double dyn echo elif else end enum esac except export extends extern fallthrough false False fi final finally float fn for foreach from func function go goto if impl implements import in include inline instanceof int interface is lambda let local long loop match mod module mut namespace new nil None nonlocal not null nullptr object of or override package pass private protected pub public raise ref return self Self select short sizeof static str string struct super switch template then this throw trait true True try type typedef typeof undefined union unless unsafe use using var virtual void volatile when where while with yield SELECT FROM WHERE INSERT UPDATE DELETE JOIN ON GROUP ORDER BY LIMIT CREATE TABLE VALUES INTO SET AND OR NOT NULL ";

/// Colorea código de casi cualquier lenguaje: no analiza, reconoce formas.
pub fn highlight(lang: &str, text: &str) -> Vec<RichSpan> {
    let lang = lang.to_lowercase();
    if matches!(
        lang.as_str(),
        "" | "text" | "txt" | "plain" | "output" | "log" | "console" | "md" | "markdown"
    ) {
        return vec![mono(text, C_TEXT)];
    }
    if lang == "diff" || lang == "patch" {
        return text
            .split_inclusive('\n')
            .map(|l| {
                let c = match l.as_bytes().first() {
                    Some(b'+') => ui::GREEN,
                    Some(b'-') => ui::RED,
                    Some(b'@') => C_FUN,
                    _ => C_TEXT,
                };
                mono(l, c)
            })
            .collect();
    }
    let hash = matches!(
        lang.as_str(),
        "py" | "python"
            | "sh"
            | "bash"
            | "zsh"
            | "shell"
            | "rb"
            | "ruby"
            | "yaml"
            | "yml"
            | "toml"
            | "ini"
            | "r"
            | "make"
            | "makefile"
            | "dockerfile"
            | "perl"
            | "elixir"
            | "powershell"
            | "ps1"
            | "nix"
    );
    let slash = !hash && !matches!(lang.as_str(), "sql" | "lua" | "haskell" | "html" | "xml" | "css");
    let dash = matches!(lang.as_str(), "sql" | "lua" | "haskell");
    let mut out: Vec<RichSpan> = Vec::new();
    let mut push = |t: &str, c: Rgb| match out.last_mut() {
        Some(s) if s.color == c => s.text.push_str(t),
        _ => out.push(mono(t, c)),
    };
    let mut r = text;
    let mut prev = '\n';
    while let Some(c) = r.chars().next() {
        let eol = r.find('\n').unwrap_or(r.len());
        let len;
        let color;
        if (hash && c == '#' && (prev.is_whitespace()))
            || (slash && r.starts_with("//"))
            || (dash && r.starts_with("--"))
        {
            (len, color) = (eol, C_COM);
        } else if r.starts_with("/*") && !hash {
            (len, color) = (r[2..].find("*/").map_or(r.len(), |i| i + 4), C_COM);
        } else if r.starts_with("<!--") {
            (len, color) = (r.find("-->").map_or(r.len(), |i| i + 3), C_COM);
        } else if r.starts_with("\"\"\"") || (hash && r.starts_with("'''")) {
            (len, color) = (r[3..].find(&r[..3]).map_or(r.len(), |i| i + 6), C_STR);
        } else if c == '"' || c == '`' || (c == '\'' && !prev.is_alphanumeric()) {
            let mut end = None;
            let mut it = r.char_indices().skip(1);
            while let Some((i, x)) = it.next() {
                match x {
                    '\\' => drop(it.next()),
                    '\n' if c != '`' => break,
                    x if x == c => {
                        end = Some(i + 1);
                        break;
                    }
                    _ => {}
                }
            }
            match end {
                Some(e) => (len, color) = (e, C_STR),
                // Comilla suelta: vida de Rust ('a), apóstrofo...
                None => (len, color) = (c.len_utf8(), C_TEXT),
            }
        } else if c.is_ascii_digit() && !prev.is_alphanumeric() && prev != '_' {
            len = r.find(|x: char| !(x.is_ascii_alphanumeric() || x == '_' || x == '.')).unwrap_or(r.len());
            color = C_NUM;
        } else if c.is_alphabetic() || c == '_' {
            len = r.find(|x: char| !(x.is_alphanumeric() || x == '_')).unwrap_or(r.len());
            let w = &r[..len];
            let after = r[len..].chars().next().unwrap_or(' ');
            color = if KEYWORDS.contains(&format!(" {w} ")) {
                C_KEY
            } else if after == '(' || (after == '!' && slash) {
                C_FUN
            } else if c.is_uppercase() && w.chars().any(char::is_lowercase) {
                C_TYPE
            } else {
                C_TEXT
            };
        } else {
            (len, color) = (c.len_utf8(), C_TEXT);
        }
        push(&r[..len], color);
        prev = r[..len].chars().next_back().unwrap_or(' ');
        r = &r[len..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(s: &[RichSpan]) -> String {
        s.iter()
            .map(|s| {
                let t = &s.text;
                match (s.bold, s.italic, s.mono) {
                    (true, true, _) => format!("<bi {t}>"),
                    (true, ..) => format!("<b {t}>"),
                    (_, true, _) => format!("<i {t}>"),
                    (.., true) => format!("<c {t}>"),
                    _ => t.clone(),
                }
            })
            .collect()
    }

    #[test]
    fn emphasis_code_and_links() {
        let f = |s| flat(&inline(s, INK));
        assert_eq!(f("a **b** c `d*e*` f"), "a <b b> c <c d*e*> f");
        assert_eq!(f("*i* y ***todo***"), "<i i> y <bi todo>");
        assert_eq!(f("snake_case_aqui 2 * 3 * 4"), "snake_case_aqui 2 * 3 * 4");
        assert_eq!(f("_i_ [web](http://x) [no]"), "<i i> web [no]");
        assert_eq!(f(r"\*tal cual\* **sin cerrar"), "*tal cual* **sin cerrar");
        assert_eq!(f("``a`b`` ñ `sin"), "<c a`b> ñ `sin");
    }

    #[test]
    fn blocks() {
        let b = parse(
            "# T\nuno\ndos\n\n- a\n  sigue\n  - b\n1. c\n> cita\n\n```rs\nfn x() {}\n```\n---\n| A | B |\n|---|---|\n| 1 | 2 |\n| 3 |\nfin",
        );
        let kinds: Vec<&str> = b
            .iter()
            .map(|b| match b {
                Block::Para(_) => "p",
                Block::Heading(..) => "h",
                Block::Item { .. } => "i",
                Block::Quote(_) => "q",
                Block::Code { .. } => "c",
                Block::Table(_) => "t",
                Block::Rule => "r",
            })
            .collect();
        assert_eq!(kinds, ["h", "p", "i", "i", "i", "q", "c", "r", "t", "p"]);
        assert!(matches!(&b[3], Block::Item { depth: 1, .. }));
        assert!(matches!(&b[6], Block::Code { lang, text } if lang == "rs" && text == "fn x() {}"));
        let Block::Table(rows) = &b[8] else { panic!() };
        assert!(rows.len() == 3 && rows.iter().all(|r| r.len() == 2));
        // Una valla sin cerrar (llegando en streaming) ya es código.
        assert!(matches!(parse("```py\nx = 1").as_slice(), [Block::Code { .. }]));
    }

    #[test]
    fn highlighting_keeps_the_text() {
        for (lang, src) in [
            ("rust", "fn f<'a>(x: &'a str) -> String { // ñ\n    \"a\\\"b\".into() /* c */\n}\n"),
            ("py", "def f(x):  # hola\n    return '''t''' + 'it\\'s' 0x1f\n"),
            ("bash", "echo \"$A\" 'b' # c\nsin cerrar '"),
            ("json", "{\"a\": [1, 2.5, true]}"),
            ("diff", "+a\n-b\n c"),
        ] {
            let s = highlight(lang, src);
            assert_eq!(s.iter().map(|s| s.text.as_str()).collect::<String>(), src);
            assert!(s.len() > 1, "{lang}");
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Verificación automática: cuando un agente termina en su worktree, forge
//! ejecuta los tests del proyecto (o el comando configurado) en segundo
//! plano, sin gastar tokens, y la tarjeta llega a «Para revisar» con el
//! resultado. Si fallan, un botón le devuelve la salida al agente.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use forge_agent::exec::{self, End};

/// Cómo acabó una verificación.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Todo en verde («36 tests en verde»).
    Passed(String),
    /// Algo falla («2 tests fallan», «No compila»).
    Failed(String),
    /// No se ejecutó nada («Sin cambios», «Sin tests que ejecutar»).
    Skipped(String),
    /// No se ejecuta nada hasta que el usuario lo diga.
    Ask(Ask),
}

/// Por qué forge pregunta antes de verificar.
#[derive(Debug, Clone, PartialEq)]
pub enum Ask {
    /// El proyecto no tiene activada la verificación automática.
    Enable,
    /// El agente ha cambiado un archivo que decide qué se ejecuta.
    Touched(String),
}

/// Quién pide la verificación.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Start {
    /// El usuario, con un botón.
    Manual,
    /// Sola, al terminar el agente. `enabled`: el proyecto la tiene activada.
    Auto { enabled: bool },
}

/// Qué verificar y cómo.
pub struct Job {
    pub dir: PathBuf,
    /// Copia principal del repositorio (identifica el proyecto).
    pub repo: PathBuf,
    pub base: String,
    pub configured: Option<String>,
    pub timeout: Duration,
    pub network: bool,
    pub log: PathBuf,
    pub start: Start,
}

/// Archivos que deciden qué se ejecuta al lanzar los tests.
const SENSITIVE: [&str; 24] = [
    "makefile",
    "gnumakefile",
    "build.rs",
    "cargo.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
    "package.json",
    ".npmrc",
    ".yarnrc",
    ".yarnrc.yml",
    ".pnpmfile.cjs",
    "bunfig.toml",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "tox.ini",
    "pytest.ini",
    "conftest.py",
    "go.mod",
    ".gitignore",
    ".gitattributes",
    ".gitmodules",
    ".envrc",
    ".tool-versions",
];

/// ¿Cambia este archivo qué se ejecuta al verificar con `cmd`?
pub fn is_sensitive(path: &str, cmd: &str) -> bool {
    let path = path.trim_matches('"').to_lowercase();
    let name = path.rsplit('/').next().unwrap_or(&path);
    SENSITIVE.contains(&name)
        || path.starts_with(".cargo/")
        || path.contains("/.cargo/")
        || name.starts_with("jest.config.")
        || name.starts_with("vitest.config.")
        || name.starts_with("vite.config.")
        || name.starts_with("babel.config.")
        // Un script que nombra el propio comando configurado.
        || cmd.to_lowercase().contains(&path)
}

/// Lista de proyectos (una ruta por línea) con la verificación automática activada.
fn enabled_file() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("forge").join("verify-projects.txt"))
}

fn listed(text: &str, repo: &Path) -> bool {
    repo.to_str().is_some_and(|r| text.lines().any(|l| l == r))
}

/// ¿Tiene este proyecto activada la verificación automática?
pub fn is_enabled(repo: &Path) -> bool {
    let text = enabled_file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    listed(&text, repo)
}

/// Activa la verificación automática de un proyecto.
pub fn enable(repo: &Path) -> Result<(), String> {
    let file = enabled_file().ok_or("sin carpeta de caché")?;
    let repo_s = repo.to_str().filter(|r| !r.contains('\n')).ok_or("ruta no válida")?;
    let mut text = std::fs::read_to_string(&file).unwrap_or_default();
    if listed(&text, repo) {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(repo_s);
    text.push('\n');
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&file, text).map_err(|e| e.to_string())
}

/// Resultado completo, con la salida guardada en un archivo.
#[derive(Debug, Clone)]
pub struct Report {
    pub cmd: Option<String>,
    pub outcome: Outcome,
    pub took: Duration,
    pub log: Option<PathBuf>,
    /// Se ejecutó con red.
    pub network: bool,
}

/// Qué ejecutar en una carpeta, o por qué no hay nada que ejecutar.
pub fn detect(dir: &Path) -> Result<String, String> {
    let has = |f: &str| dir.join(f).exists();
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
    if has("Cargo.toml") {
        return Ok("cargo test --quiet".into());
    }
    if has("package.json") {
        let pkg: serde_json::Value = serde_json::from_str(&read("package.json")).unwrap_or_default();
        let test = pkg.pointer("/scripts/test").and_then(|t| t.as_str()).unwrap_or_default();
        if test.is_empty() || test.contains("no test specified") {
            return Err("Sin script de tests en package.json".into());
        }
        if !has("node_modules") {
            return Err("Faltan las dependencias (node_modules)".into());
        }
        let runner = if has("pnpm-lock.yaml") {
            "pnpm test"
        } else if has("yarn.lock") {
            "yarn test"
        } else if has("bun.lockb") || has("bun.lock") {
            "bun run test"
        } else {
            "npm test"
        };
        return Ok(runner.into());
    }
    if has("go.mod") {
        return Ok("go test ./...".into());
    }
    if has("pytest.ini") || has("conftest.py") || read("pyproject.toml").contains("pytest") {
        return Ok("python3 -m pytest -q".into());
    }
    if read("Makefile").lines().any(|l| l.starts_with("test:")) {
        return Ok("make test".into());
    }
    Err("Sin tests que ejecutar".into())
}

/// Número justo antes de `word` en una línea («3 passed», «2 failed,»).
fn count_before(line: &str, word: &str) -> Option<usize> {
    let words: Vec<&str> = line.split(|c: char| c.is_whitespace() || c == ';' || c == ',').collect();
    let words: Vec<&str> = words.into_iter().filter(|w| !w.is_empty()).collect();
    words.windows(2).filter(|w| w[1].trim_end_matches(['.', ':']) == word).find_map(|w| w[0].parse().ok())
}

fn tests(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 test {one}") } else { format!("{n} tests {many}") }
}

/// Resume la salida de los tests en una frase.
pub fn summarize(output: &str, ok: bool) -> Outcome {
    let (mut passed, mut failed, mut go_fail) = (0, 0, 0);
    let mut cargo = false;
    for line in output.lines() {
        let l = line.trim();
        if l.starts_with("test result:") {
            // cargo: una línea por binario de tests; se suman.
            cargo = true;
            passed += count_before(l, "passed").unwrap_or(0);
            failed += count_before(l, "failed").unwrap_or(0);
        } else if l.starts_with("--- FAIL") {
            go_fail += 1;
        } else if !cargo && (l.starts_with("Tests:") || l.starts_with('=') || l.contains(" passed")) {
            // jest («Tests: 1 failed, 3 passed»), pytest («== 1 failed, 2 passed ==»)...
            if let Some(n) = count_before(l, "failed") {
                failed = n;
            }
            if let Some(n) = count_before(l, "passed") {
                passed = n;
            }
        }
    }
    let failed = failed.max(go_fail);
    if ok {
        return Outcome::Passed(if passed > 0 {
            tests(passed, "en verde", "en verde")
        } else {
            "Todo en verde".into()
        });
    }
    if failed > 0 {
        return Outcome::Failed(tests(failed, "falla", "fallan"));
    }
    let compile = output.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("error[E") || l.starts_with("error: could not compile") || l.contains("SyntaxError")
    });
    Outcome::Failed(if compile { "No compila".into() } else { "La verificación falla".into() })
}

/// Ejecuta `cmd` aislado en el worktree de `job`. Devuelve (éxito, salida)
/// o `None` si se canceló.
fn run(job: &Job, cmd: &str, cancel: &AtomicBool) -> Result<Option<(bool, String)>, String> {
    let spec = exec::Spec {
        cmd,
        dir: &job.dir,
        project: &job.repo,
        sandboxed: true,
        network: job.network,
        timeout: job.timeout,
        env: &[("CI", "1"), ("FORGE_VERIFY", "1")],
    };
    let out = exec::run(&spec, &|| cancel.load(Ordering::Relaxed), &mut |_| {})?;
    Ok(match out.end {
        End::Cancelled => None,
        End::Exit(code) => Some((code == Some(0), out.text)),
        End::Timeout => {
            let min = job.timeout.as_secs() / 60;
            Some((false, format!("{}\n[forge] Cancelado: tardó más de {min} min\n", out.text)))
        }
    })
}

/// Verifica un worktree: si tiene cambios y se puede, ejecuta el comando
/// configurado (o el que detecte) y guarda la salida. `None` si se canceló.
pub fn check(job: &Job, cancel: &AtomicBool) -> Option<Report> {
    let start = Instant::now();
    let done = |outcome: Outcome, cmd: Option<String>| {
        Some(Report { cmd, outcome, took: start.elapsed(), log: None, network: job.network })
    };
    let skip = |why: String, cmd: Option<String>| done(Outcome::Skipped(why), cmd);
    let files = match crate::worktree::diff(&job.dir, &job.base) {
        Ok(f) if f.is_empty() => return skip("Sin cambios que verificar".into(), None),
        Ok(f) => f,
        // Sin saber qué ha cambiado, no se ejecuta nada.
        Err(e) => return skip(format!("No se puede leer el diff: {e}"), None),
    };
    let cmd = match &job.configured {
        Some(c) => c.clone(),
        None => match detect(&job.dir) {
            Ok(c) => c,
            Err(why) => return skip(why, None),
        },
    };
    if let Start::Auto { enabled } = job.start {
        if let Some(f) = files.iter().find(|f| is_sensitive(&f.path, &cmd)) {
            let name = f.path.trim_matches('"').rsplit('/').next().unwrap_or_default().to_string();
            return done(Outcome::Ask(Ask::Touched(name)), None);
        }
        if !enabled {
            return done(Outcome::Ask(Ask::Enable), None);
        }
    }
    let (ok, output) = match run(job, &cmd, cancel) {
        Ok(Some(r)) => r,
        Ok(None) => return None,
        Err(e) => return skip(e, Some(cmd)),
    };
    let log = &job.log;
    let saved = log.parent().is_some_and(|d| std::fs::create_dir_all(d).is_ok())
        && std::fs::write(log, format!("$ {cmd}\n\n{output}")).is_ok();
    Some(Report {
        outcome: summarize(&output, ok),
        cmd: Some(cmd),
        took: start.elapsed(),
        log: saved.then(|| log.clone()),
        network: job.network,
    })
}

/// Lo que se le escribe al agente para que arregle el fallo.
pub fn fix_prompt(r: &Report) -> Option<String> {
    let Outcome::Failed(what) = &r.outcome else { return None };
    let cmd = r.cmd.as_deref().unwrap_or("los tests");
    let log = match &r.log {
        Some(log) => format!(" La salida completa está en {}.", log.display()),
        None => String::new(),
    };
    let net = if r.network { "" } else { " y sin red" };
    Some(format!(
        "He ejecutado `{cmd}` en este worktree, en un sandbox que solo deja escribir en él{net}, y falla ({}).{log} \
         Arréglalo y comprueba que pasa; si el fallo se debe solo al sandbox, dímelo y no cambies código.",
        what.to_lowercase()
    ))
}

/// Como `fix_prompt`, con el final de la salida: un modelo local no puede
/// leer el registro, que está fuera de su carpeta.
pub fn fix_prompt_with_output(r: &Report) -> Option<String> {
    let base = fix_prompt(&Report { log: None, ..r.clone() })?;
    let text = r.log.as_ref().and_then(|l| std::fs::read_to_string(l).ok()).unwrap_or_default();
    let mut from = text.len().saturating_sub(6_000);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    Some(format!("{base}\n\nSalida (el final):\n```\n{}\n```", text[from..].trim()))
}

/// Una verificación en curso o terminada, por panel.
#[derive(Debug, Clone)]
pub struct Verify {
    /// Para descartar resultados de una verificación ya sustituida.
    pub generation: u64,
    pub since: Instant,
    pub cancel: Arc<AtomicBool>,
    /// `None` mientras se ejecuta.
    pub report: Option<Report>,
}

impl Verify {
    pub fn running(generation: u64) -> Self {
        Self { generation, since: Instant::now(), cancel: Arc::new(AtomicBool::new(false)), report: None }
    }

    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_agent::exec::{strip_ansi, target_dir};
    use forge_agent::testing::{Needs, skip};
    use std::process::Command;

    #[test]
    fn summarizes_cargo() {
        let ok = "running 3 tests\ntest result: ok. 3 passed; 0 failed; 0 ignored\n\ntest result: ok. 33 passed; 0 failed;";
        assert_eq!(summarize(ok, true), Outcome::Passed("36 tests en verde".into()));
        let bad = "test a ... FAILED\ntest result: FAILED. 2 passed; 1 failed; 0 ignored";
        assert_eq!(summarize(bad, false), Outcome::Failed("1 test falla".into()));
        let broken = "error[E0425]: cannot find value `x`\nerror: could not compile `demo`";
        assert_eq!(summarize(broken, false), Outcome::Failed("No compila".into()));
    }

    #[test]
    fn summarizes_other_runners() {
        let jest = "Tests:       2 failed, 5 passed, 7 total";
        assert_eq!(summarize(jest, false), Outcome::Failed("2 tests fallan".into()));
        let pytest = "===== 4 passed in 0.12s =====";
        assert_eq!(summarize(pytest, true), Outcome::Passed("4 tests en verde".into()));
        let go = "--- FAIL: TestA (0.00s)\n--- FAIL: TestB (0.00s)\nFAIL";
        assert_eq!(summarize(go, false), Outcome::Failed("2 tests fallan".into()));
        assert_eq!(summarize("", true), Outcome::Passed("Todo en verde".into()));
        assert_eq!(summarize("make: *** Error 2", false), Outcome::Failed("La verificación falla".into()));
    }

    #[test]
    fn strips_escapes() {
        assert_eq!(strip_ansi("\x1b[1;31merror\x1b[0m: x\r\n\x1b]0;título\x07ok"), "error: x\nok");
    }

    #[test]
    fn detects_commands() {
        let dir = std::env::temp_dir().join(format!("forge-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(detect(&dir).is_err());
        std::fs::write(dir.join("Makefile"), "all:\n\ttrue\ntest:\n\ttrue\n").unwrap();
        assert_eq!(detect(&dir).unwrap(), "make test");
        std::fs::write(dir.join("package.json"), r#"{"scripts":{"test":"jest"}}"#).unwrap();
        assert_eq!(detect(&dir).unwrap_err(), "Faltan las dependencias (node_modules)");
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("yarn.lock"), "").unwrap();
        assert_eq!(detect(&dir).unwrap(), "yarn test");
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(detect(&dir).unwrap(), "cargo test --quiet");
        for f in ["Makefile", "package.json", "yarn.lock", "Cargo.toml"] {
            let _ = std::fs::remove_file(dir.join(f));
        }
        let _ = std::fs::remove_dir(dir.join("node_modules"));
        let _ = std::fs::remove_dir(&dir);
    }

    /// Repositorio nuevo con un Makefile y un archivo cambiado.
    fn repo(name: &str, test: &str) -> (PathBuf, Job) {
        let dir = std::env::temp_dir().join(format!("forge-vjob-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success();
            assert!(ok, "git {args:?}");
        };
        std::fs::write(dir.join("Makefile"), format!("test:\n\t{test}\n")).unwrap();
        std::fs::write(dir.join("a.txt"), "uno\n").unwrap();
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "x"]);
        let job = Job {
            dir: dir.clone(),
            repo: dir.clone(),
            base: "HEAD".into(),
            configured: None,
            timeout: Duration::from_secs(30),
            network: false,
            log: dir.join("log.txt"),
            start: Start::Manual,
        };
        (dir, job)
    }

    #[test]
    fn asks_before_running() {
        let (dir, mut job) = repo("ask", "touch ejecutado");
        let no = AtomicBool::new(false);
        let outcome = |job: &Job| check(job, &no).unwrap().outcome;
        assert_eq!(outcome(&job), Outcome::Skipped("Sin cambios que verificar".into()));
        std::fs::write(dir.join("a.txt"), "dos\n").unwrap();
        job.start = Start::Auto { enabled: false };
        assert_eq!(outcome(&job), Outcome::Ask(Ask::Enable));
        // Activada, pero el agente ha tocado el Makefile.
        std::fs::write(dir.join("Makefile"), "test:\n\ttouch ejecutado\n\ttrue\n").unwrap();
        job.start = Start::Auto { enabled: true };
        assert_eq!(outcome(&job), Outcome::Ask(Ask::Touched("Makefile".into())));
        // Sin poder leer el diff no se ejecuta nada, ni a mano.
        job.start = Start::Manual;
        job.base = "no-existe".into();
        assert!(matches!(outcome(&job), Outcome::Skipped(_)));
        assert!(!dir.join("ejecutado").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn runs_sandboxed_and_cancels() {
        // Dentro de otro sandbox no se puede crear uno.
        if skip(&[Needs::Sandbox]) {
            return;
        }
        let (dir, mut job) = repo("run", "echo dentro; echo x > ../forge-vjob-fuera-$$$$");
        std::fs::write(dir.join("a.txt"), "dos\n").unwrap();
        job.start = Start::Auto { enabled: true };
        let no = AtomicBool::new(false);
        let r = check(&job, &no).unwrap();
        assert!(matches!(r.outcome, Outcome::Failed(_)), "{r:?}");
        let log = std::fs::read_to_string(r.log.unwrap()).unwrap();
        assert!(log.contains("dentro") && log.contains("not permitted"), "{log}");
        let (ok, out) = run(&job, "echo uno; echo dos >&2; false", &no).unwrap().unwrap();
        assert!(!ok && out.contains("uno") && out.contains("dos"));
        let yes = AtomicBool::new(true);
        assert!(run(&job, "sleep 5", &yes).unwrap().is_none());
        let _ = std::fs::remove_dir_all(target_dir(&job.repo).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sensitive_files() {
        for f in [
            "Makefile",
            "sub/build.rs",
            ".cargo/config.toml",
            "web/package.json",
            "jest.config.js",
            "\"a/Cargo.toml\"",
        ] {
            assert!(is_sensitive(f, "cargo test"), "{f}");
        }
        for f in ["src/main.rs", "README.md", "tests/a.rs"] {
            assert!(!is_sensitive(f, "cargo test"), "{f}");
        }
        assert!(is_sensitive("scripts/ci.sh", "./scripts/ci.sh --rapido"));
        assert!(listed("/a/b\n/c\n", Path::new("/c")));
        assert!(!listed("/a/b\n", Path::new("/a")));
    }

    #[test]
    fn fix_prompt_carries_the_end_of_the_output() {
        let log = std::env::temp_dir().join(format!("forge-fix-{}.log", std::process::id()));
        std::fs::write(&log, format!("{}ñFALLO FINAL", "ñ".repeat(9000))).unwrap();
        let r = Report {
            cmd: Some("cargo test".into()),
            outcome: Outcome::Failed("No compila".into()),
            took: Duration::ZERO,
            log: Some(log.clone()),
            network: false,
        };
        let p = fix_prompt_with_output(&r).unwrap();
        assert!(p.contains("FALLO FINAL") && p.len() < 7_000 && !p.contains("completa está en"));
        let _ = std::fs::remove_file(log);
    }

    #[test]
    fn fix_prompt_only_on_failure() {
        let mut r = Report {
            cmd: Some("cargo test".into()),
            outcome: Outcome::Failed("2 tests fallan".into()),
            took: Duration::ZERO,
            log: Some("/tmp/v.log".into()),
            network: false,
        };
        let p = fix_prompt(&r).unwrap();
        assert!(p.contains("`cargo test`") && p.contains("2 tests fallan") && p.contains("/tmp/v.log"));
        assert!(!p.contains('\n'));
        r.outcome = Outcome::Passed("ok".into());
        assert!(fix_prompt(&r).is_none());
    }
}

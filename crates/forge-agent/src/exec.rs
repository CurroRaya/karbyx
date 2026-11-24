// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Ejecución de comandos: con la shell del usuario, aislados por el sandbox
//! (salvo que el usuario permita lo contrario), con límite de tiempo y
//! cancelables. Lo usan el agente y la verificación de la mesa.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::sandbox;

/// Bytes de salida que se guardan (el final, que es donde están los fallos).
const KEEP: usize = 512 * 1024;

/// Caché de compilación del proyecto: su nombre y un resumen de su ruta, para
/// que dos proyectos que se llamen igual no la compartan.
pub fn target_dir(repo: &Path) -> Option<PathBuf> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    repo.hash(&mut h);
    let name = repo.file_name()?.to_string_lossy().into_owned();
    Some(dirs::cache_dir()?.join("forge").join("target").join(format!("{name}-{:016x}", h.finish())))
}

/// Carpeta temporal propia de una ejecución; se borra al acabar.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Option<Self> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
        let d = dirs::cache_dir()?.join("forge").join("tmp").join(format!("v{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&d).ok()?;
        Some(Self(d))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// SDK de macOS: `xcrun` no funciona dentro del sandbox.
pub fn sdk_root() -> Option<String> {
    let out = Command::new("/usr/bin/xcrun").arg("--show-sdk-path").current_dir("/").output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// Quita las secuencias de escape ANSI (colores, cursor).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            if c != '\r' {
                out.push(c);
            }
            continue;
        }
        match it.next() {
            Some('[') => {
                for c in it.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = it.next() {
                    if c == '\x07' || (c == '\x1b' && it.peek() == Some(&'\\')) {
                        it.next_if_eq(&'\\');
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Shell del usuario, con su PATH (como sus terminales).
pub fn shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into())
}

/// Qué ejecutar y cómo.
pub struct Spec<'a> {
    pub cmd: &'a str,
    /// Carpeta donde se ejecuta (y la única donde puede escribir, si va aislado).
    pub dir: &'a Path,
    /// Proyecto: identifica su caché de compilación.
    pub project: &'a Path,
    pub sandboxed: bool,
    pub network: bool,
    pub timeout: Duration,
    pub env: &'a [(&'a str, &'a str)],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum End {
    /// Código de salida (`None`: lo mató una señal).
    Exit(Option<i32>),
    Timeout,
    Cancelled,
}

#[derive(Debug)]
pub struct Output {
    pub end: End,
    /// El final de la salida, sin códigos ANSI.
    pub text: String,
}

/// Parte válida de `buf` como texto; deja en `buf` un carácter a medias.
fn take_utf8(buf: &mut Vec<u8>) -> String {
    let cut = match std::str::from_utf8(buf) {
        Ok(_) => buf.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => buf.len(),
    };
    String::from_utf8_lossy(&buf.drain(..cut).collect::<Vec<_>>()).into_owned()
}

/// Ejecuta el comando. `on` recibe la salida según llega (con ANSI).
pub fn run(spec: &Spec, cancelled: &dyn Fn() -> bool, on: &mut dyn FnMut(&str)) -> Result<Output, String> {
    let dir = spec.dir;
    let target = target_dir(spec.project).ok_or("sin carpeta de caché")?;
    let tmp = TempDir::new().ok_or("sin carpeta temporal")?;
    let mut command = if spec.sandboxed {
        std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        let dirs = sandbox::Dirs { worktree: dir.into(), target: target.clone(), tmp: tmp.0.clone() };
        let mut c = sandbox::command(&shell(), &dirs, spec.network)?;
        if dir.join("Cargo.toml").exists()
            && let Some(sdk) = sdk_root()
        {
            c.env("SDKROOT", sdk);
        }
        // Sin target propio en la carpeta, uno compartido por proyecto en la
        // caché: las dependencias se compilan una vez para todos sus agentes.
        if dir.join("target").exists() {
            c.env_remove("CARGO_TARGET_DIR");
        } else {
            c.env("CARGO_TARGET_DIR", &target);
        }
        c
    } else {
        let mut c = Command::new(shell());
        c.env_remove("FORGE_SOCKET").env_remove("FORGE_PANE");
        c
    };
    // -i -l: carga el PATH del usuario (rustup, nvm...) aunque forge se
    // abriera desde el Finder. La salida de error va al mismo sitio.
    command
        .args(["-l", "-i", "-c", &format!("exec 2>&1; {}", spec.cmd)])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("CARGO_TERM_COLOR", "never")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .envs(spec.env.iter().copied())
        .process_group(0);
    let mut child = command.spawn().map_err(|e| format!("No se pudo ejecutar: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("sin salida")?;
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut chunk = [0u8; 16 * 1024];
        while let Ok(n) = stdout.read(&mut chunk) {
            if n == 0 || tx.send(chunk[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let pid = child.id() as i32;
    // Todo el grupo: la shell y lo que lanzó (cargo, rustc, node...).
    // SAFETY: kill(2) con el id de grupo de un proceso hijo nuestro.
    let kill = || unsafe {
        libc::kill(-pid, libc::SIGKILL);
    };
    let (mut kept, mut pending) = (Vec::new(), Vec::new());
    let start = Instant::now();
    let mut exited: Option<(Instant, Option<i32>)> = None;
    let end = loop {
        match rx.recv_timeout(Duration::from_millis(60)) {
            Ok(bytes) => {
                kept.extend_from_slice(&bytes);
                if kept.len() > 2 * KEEP {
                    kept.drain(..kept.len() - KEEP);
                }
                pending.extend_from_slice(&bytes);
                on(&take_utf8(&mut pending));
            }
            // Se cerró la salida: solo queda esperar al proceso.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some((_, code)) = exited {
                    break End::Exit(code);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if cancelled() {
            break End::Cancelled;
        }
        if start.elapsed() > spec.timeout {
            break End::Timeout;
        }
        match (exited, child.try_wait()) {
            // Un proceso que dejó en segundo plano mantiene la salida abierta.
            (Some((at, code)), _) if at.elapsed() > Duration::from_millis(500) => break End::Exit(code),
            (None, Ok(Some(s))) => exited = Some((Instant::now(), s.code())),
            (None, Err(e)) => return Err(e.to_string()),
            _ => {}
        }
    };
    kill();
    let _ = child.wait();
    let text = strip_ansi(&String::from_utf8_lossy(&kept[kept.len().saturating_sub(KEEP)..]));
    Ok(Output { end, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec<'a>(cmd: &'a str, dir: &'a Path, secs: u64) -> Spec<'a> {
        Spec {
            cmd,
            dir,
            project: dir,
            sandboxed: false,
            network: false,
            timeout: Duration::from_secs(secs),
            env: &[],
        }
    }

    #[test]
    fn captures_output_and_exit_code() {
        let dir = std::env::temp_dir();
        let mut live = String::new();
        let out =
            run(&spec("echo año; echo mal >&2; exit 3", &dir, 20), &|| false, &mut |s| live.push_str(s))
                .unwrap();
        assert_eq!(out.end, End::Exit(Some(3)));
        assert!(out.text.contains("año") && out.text.contains("mal"), "{}", out.text);
        assert!(live.contains("año"));
    }

    #[test]
    fn background_processes_and_timeouts_do_not_hang() {
        let dir = std::env::temp_dir();
        let t = Instant::now();
        let out = run(&spec("sleep 60 & echo ya", &dir, 30), &|| false, &mut |_| {}).unwrap();
        assert_eq!(out.end, End::Exit(Some(0)));
        let out = run(&spec("sleep 60", &dir, 1), &|| false, &mut |_| {}).unwrap();
        assert_eq!(out.end, End::Timeout);
        assert!(t.elapsed() < Duration::from_secs(25));
    }

    #[test]
    fn split_characters() {
        let mut b = "ñ".as_bytes()[..1].to_vec();
        assert_eq!(take_utf8(&mut b), "");
        b.push("ñ".as_bytes()[1]);
        assert_eq!(take_utf8(&mut b), "ñ");
    }
}

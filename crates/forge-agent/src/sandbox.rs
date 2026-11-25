// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Sandbox de macOS (`sandbox-exec`) para los comandos del agente del chat y
//! los tests de los agentes de la mesa.
//!
//! Es código que ha podido escribir un modelo y que nadie ha revisado. Todo está prohibido salvo lo que se permite aquí: leer (menos
//! las claves habituales), y escribir en el worktree, en la caché de
//! compilación y en una carpeta temporal. Sin red, sin lanzar otras apps y
//! sin tocar lo que luego se ejecuta fuera del sandbox (`.git`, `.claude`...).

use std::path::{Path, PathBuf};
use std::process::Command;

const EXEC: &str = "/usr/bin/sandbox-exec";

/// Carpetas del HOME que no se pueden leer.
pub const SECRETS: [&str; 26] = [
    ".ssh",
    ".aws",
    ".azure",
    ".gnupg",
    ".kube",
    ".docker",
    ".netrc",
    ".git-credentials",
    ".config/gh",
    ".config/gcloud",
    ".claude",
    ".claude.json",
    ".codex",
    ".gemini",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".zsh_history",
    ".bash_history",
    ".config/forge/mcp.json",
    "Library/Application Support/forge",
    "Library/Keychains",
    "Library/Cookies",
    "Library/Messages",
    "Library/Mail",
    "Library/Safari",
    "Library/Application Support/Google/Chrome",
];

/// Archivos del worktree que no se pueden escribir, a cualquier nivel: los
/// leen o ejecutan luego git, los asistentes o el editor, fuera del sandbox.
pub const PROTECTED: [&str; 12] = [
    ".git",
    ".forge",
    ".claude",
    ".codex",
    ".gemini",
    ".vscode",
    ".idea",
    ".mcp.json",
    ".envrc",
    "claude.md",
    "agents.md",
    "gemini.md",
];

const BASE: &str = r#"(version 1)
(deny default)
(allow process-exec)
(allow process-fork)
(allow process-info* (target same-sandbox))
(allow signal (target same-sandbox))
(allow sysctl-read)
(allow ipc-posix-sem)
(allow user-preference-read)
(allow file-read*)
(allow mach-lookup
  (global-name "com.apple.system.opendirectoryd.libinfo")
  (global-name "com.apple.system.logger")
  (global-name "com.apple.system.notification_center"))
(allow file-write* (literal "/dev/null") (literal "/dev/zero") (literal "/dev/dtracehelper"))
(allow file-write* (subpath (param "WT")) (subpath (param "TARGET")) (subpath (param "TMP")))
(deny file-write* (literal (param "WT")))
(deny file-read* file-write* (literal "/dev/tty") (regex #"^/dev/(tty|cu)"))
(allow pseudo-tty)
(allow file-read* file-write* file-ioctl (literal "/dev/ptmx"))
(allow file-read* file-write* file-ioctl
  (require-all (regex #"^/dev/ttys[0-9]+$") (extension "com.apple.sandbox.pty")))
(allow system-socket (socket-domain AF_UNIX))
(allow network-bind network-inbound network-outbound (subpath (param "TMP")))
"#;

/// Excepción por proyecto: TCP/UDP y DNS (no sockets locales como el de
/// Docker, el agente de SSH o el de forge).
const NETWORK: &str = r#"(allow network-outbound (remote ip "*:*") (literal "/private/var/run/mDNSResponder"))
(allow network-inbound (local ip "*:*"))
(allow network-bind (local ip "*:*"))
(allow system-socket)
(allow mach-lookup (global-name "com.apple.SystemConfiguration.configd") (global-name "com.apple.trustd") (global-name "com.apple.trustd.agent")
  (global-name "com.apple.TrustEvaluationAgent"))
"#;

/// Escapa una ruta para una expresión regular del perfil.
fn quote(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\.[]^$*+?{}()|".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `git` → `[gG][iI][tT]`: el disco de macOS no distingue mayúsculas.
fn any_case(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '.' => "\\.".to_string(),
            c if c.is_ascii_alphabetic() => format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase()),
            c => c.to_string(),
        })
        .collect()
}

/// Carpetas donde el código aislado puede escribir (rutas reales).
pub struct Dirs {
    pub worktree: PathBuf,
    pub target: PathBuf,
    pub tmp: PathBuf,
}

fn profile(dirs: &Dirs, home: &Path, network: bool) -> Result<String, String> {
    let wt =
        dirs.worktree.to_str().filter(|s| !s.contains(['"', '\n'])).ok_or("la ruta no se puede aislar")?;
    let mut p = BASE.to_string();
    for s in SECRETS {
        let path = home.join(s);
        let path =
            path.to_str().filter(|s| !s.contains(['"', '\\', '\n'])).ok_or("la ruta no se puede aislar")?;
        p.push_str(&format!("(deny file-read* (subpath \"{path}\"))\n"));
    }
    let names: Vec<String> = PROTECTED.iter().map(|n| any_case(n)).collect();
    p.push_str(&format!("(deny file-write* (regex #\"^{}/(.*/)?({})(/|$)\"))\n", quote(wt), names.join("|")));
    if network {
        p.push_str(NETWORK);
    }
    Ok(p)
}

/// Comando que ejecuta `program` aislado. Las rutas de `dirs` deben existir.
/// Si no se puede aislar, es un error: nunca se ejecuta sin sandbox.
pub fn command(program: &str, dirs: &Dirs, network: bool) -> Result<Command, String> {
    if !Path::new(EXEC).exists() {
        return Err("Sin sandbox no se verifica".into());
    }
    let real = |p: &Path| p.canonicalize().map_err(|e| format!("No se pudo aislar: {e}"));
    let dirs = Dirs { worktree: real(&dirs.worktree)?, target: real(&dirs.target)?, tmp: real(&dirs.tmp)? };
    let home = real(&dirs::home_dir().ok_or("sin carpeta de usuario")?)?;
    let param = |k: &str, p: &Path| {
        let mut s = std::ffi::OsString::from(format!("{k}="));
        s.push(p);
        s
    };
    let mut c = Command::new(EXEC);
    c.arg("-p").arg(profile(&dirs, &home, network)?);
    for (k, p) in [("WT", &dirs.worktree), ("TARGET", &dirs.target), ("TMP", &dirs.tmp)] {
        c.arg("-D").arg(param(k, p));
    }
    c.arg(program)
        .env("TMPDIR", &dirs.tmp)
        .env("GOCACHE", dirs.target.join("go-build"))
        .env("npm_config_cache", dirs.target.join("npm"))
        .env_remove("SSH_AUTH_SOCK")
        // Los certificados del llavero no se pueden leer.
        .env_remove("NODE_USE_SYSTEM_CA")
        .env_remove("FORGE_SOCKET")
        .env_remove("FORGE_PANE");
    if !network {
        c.env("CARGO_NET_OFFLINE", "true");
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Needs, skip};

    /// Ejecuta `script` aislado en una carpeta nueva. (carpeta, salió bien)
    fn sh(name: &str, script: &str) -> Option<(PathBuf, bool)> {
        // Dentro de otro sandbox no se puede crear uno.
        if skip(&[Needs::Sandbox, Needs::Ports]) {
            return None;
        }
        let root = std::env::temp_dir().join(format!("forge-sb-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // El worktree lleva caracteres especiales de las expresiones regulares.
        let dirs =
            Dirs { worktree: root.join("w+(1).x"), target: root.join("target"), tmp: root.join("tmp") };
        for d in [&dirs.worktree, &dirs.target, &dirs.tmp] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(dirs.worktree.join(".git"), "gitdir: x").unwrap();
        let ok = command("/bin/sh", &dirs, false)
            .unwrap()
            .args(["-c", script])
            .current_dir(&dirs.worktree)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        Some((root, ok))
    }

    #[test]
    fn allows_normal_work() {
        let Some((root, ok)) = sh(
            "ok",
            "echo a > f && mkdir d && echo b > d/g && echo c > $TMPDIR/t && echo d > ../target/x && cd $TMPDIR && (/usr/bin/nc -l -U s & sleep 0.5; /usr/bin/nc -U -w 1 s < /dev/null)",
        ) else {
            return;
        };
        assert!(ok);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn blocks_escapes() {
        let attempts = [
            "echo x > ../fuera",
            "echo x > .git",
            "echo x > .GIT",
            "rm .git",
            "mkdir .claude",
            "mkdir -p sub/.Claude",
            "mkdir -p $TMPDIR/a/.claude && mv $TMPDIR/a/.claude .claude",
            "echo x > CLAUDE.md",
            "mv ../w+\\(1\\).x ../tmp/w",
            "echo y > ../tmp/o && ln -s ../fuera s && echo x > s",
            "ls ~/Library/Keychains",
            "/bin/launchctl submit -l forge.sbtest -- /usr/bin/true",
        ];
        for (i, a) in attempts.iter().enumerate() {
            let Some((root, ok)) = sh(&format!("no{i}"), a) else { return };
            assert!(!ok, "se ha escapado: {a}");
            assert!(!root.join("fuera").exists(), "{a}");
            let git = std::fs::read_to_string(root.join("w+(1).x/.git")).unwrap();
            assert_eq!(git, "gitdir: x", "{a}");
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn only_its_own_terminals() {
        if skip(&[Needs::Sandbox, Needs::Ports]) {
            return;
        }
        // Una terminal propia, sí (`script` abre una).
        let (root, ok) = sh("pty", "/usr/bin/script -q /dev/null /bin/echo hola < /dev/null").unwrap();
        assert!(ok);
        let _ = std::fs::remove_dir_all(root);
        // Las de otros, ni leer ni escribir.
        // SAFETY: abre un pty nuevo y consulta su nombre; se cierra al final.
        let (fd, name) = unsafe {
            let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(fd >= 0 && libc::grantpt(fd) == 0 && libc::unlockpt(fd) == 0);
            (fd, std::ffi::CStr::from_ptr(libc::ptsname(fd)).to_string_lossy().into_owned())
        };
        for c in [format!("echo x > {name}"), format!(": < {name}")] {
            let (root, ok) = sh("tty", &c).unwrap();
            assert!(!ok, "se ha escapado: {c}");
            let _ = std::fs::remove_dir_all(root);
        }
        // SAFETY: `fd` es nuestro.
        unsafe { libc::close(fd) };
    }

    #[test]
    fn blocks_network_and_sockets() {
        if skip(&[Needs::Sandbox, Needs::Ports]) {
            return;
        }
        // Servidores que sí escuchan: fuera del sandbox se conecta, dentro no.
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let sock = std::env::temp_dir().join(format!("forge-sb-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let _unix = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let connect = [
            format!("/usr/bin/nc -z -w 2 127.0.0.1 {}", tcp.local_addr().unwrap().port()),
            format!("/usr/bin/nc -U -w 1 '{}' < /dev/null", sock.display()),
            // Por un enlace desde la carpeta temporal, donde sí se permiten sockets.
            format!(
                "ln -sf '{}' \"$TMPDIR/forge-sb-ln\" && /usr/bin/nc -U -w 1 \"$TMPDIR/forge-sb-ln\" < /dev/null",
                sock.display()
            ),
        ];
        for (i, c) in connect.iter().enumerate() {
            let Some((root, ok)) = sh(&format!("net{i}"), c) else { return };
            assert!(Command::new("/bin/sh").args(["-c", c]).output().unwrap().status.success(), "{c}");
            assert!(!ok, "se ha escapado: {c}");
            let _ = std::fs::remove_dir_all(root);
        }
        let _ = std::fs::remove_file(std::env::temp_dir().join("forge-sb-ln"));
        let _ = std::fs::remove_file(sock);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! Instalar habilidades de GitHub (o de una carpeta): se descargan aparte, se
//! enseñan, y solo se copian si el usuario acepta. Nada se ejecuta.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use crate::know;

/// Límites de lo que se descarga y de lo que se instala.
const DOWNLOAD_MAX: u64 = 60 << 20;
const FILES_MAX: usize = 200;
const FILE_MAX: u64 = 2 << 20;
const TOTAL_MAX: u64 = 10 << 20;
const FOUND_MAX: usize = 1000;

/// De dónde se baja.
#[derive(Debug, Clone, PartialEq)]
pub enum From {
    GitHub { owner: String, repo: String, rev: Option<String>, path: String },
    Folder(PathBuf),
}

impl From {
    /// `dueño/repo`, `dueño/repo/carpeta`, una dirección de GitHub o una carpeta.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim().trim_end_matches('/');
        if s.is_empty() {
            return Err("Escribe una dirección de GitHub o una carpeta".into());
        }
        if s.starts_with('/') || s.starts_with("~/") {
            let p = match s.strip_prefix("~/") {
                Some(r) => dirs::home_dir().ok_or("sin carpeta personal")?.join(r),
                None => PathBuf::from(s),
            };
            return if p.is_dir() { Ok(From::Folder(p)) } else { Err("Esa carpeta no existe".into()) };
        }
        let rest = s
            .strip_prefix("https://")
            .or_else(|| s.strip_prefix("http://"))
            .map(|r| r.strip_prefix("www.").unwrap_or(r));
        let rest = match rest {
            Some(r) => r.strip_prefix("github.com/").ok_or("De momento solo GitHub")?,
            None => s.strip_prefix("github.com/").unwrap_or(s),
        };
        let rest = rest.split(['?', '#']).next().unwrap_or(rest);
        let parts: Vec<&str> = rest.split('/').collect();
        let [owner, repo, tail @ ..] = parts.as_slice() else {
            return Err("Falta el repositorio: dueño/nombre".into());
        };
        let repo = repo.trim_end_matches(".git");
        let name = |s: &str| {
            !s.is_empty()
                && s != "."
                && s != ".."
                && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        };
        if !name(owner)
            || !name(repo)
            || !tail.iter().all(|p| name(p) || p.contains(' ') && !p.contains(".."))
        {
            return Err("La dirección no es válida".into());
        }
        // `…/tree/rama/carpeta` (la rama, sin barras) o la carpeta sin más.
        let (rev, path) = match tail {
            ["tree" | "blob", rev, path @ ..] => (Some(rev.to_string()), path),
            path => (None, path),
        };
        let path = path.join("/");
        let path = path.strip_suffix("SKILL.md").unwrap_or(&path).trim_end_matches('/').to_string();
        Ok(From::GitHub { owner: owner.to_string(), repo: repo.to_string(), rev, path })
    }

    /// Para enseñarlo y apuntarlo.
    pub fn label(&self) -> String {
        match self {
            From::Folder(p) => p.display().to_string(),
            From::GitHub { owner, repo, rev, path } => {
                let mut s = format!("github.com/{owner}/{repo}");
                if let Some(r) = rev {
                    s.push_str(&format!("/tree/{r}"));
                }
                if !path.is_empty() {
                    s.push_str(&format!("/{path}"));
                }
                s
            }
        }
    }
}

/// Una habilidad encontrada.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub name: String,
    pub description: String,
    /// Su `SKILL.md`, entero.
    pub text: String,
    /// Los demás archivos, relativos, y su tamaño.
    pub files: Vec<(PathBuf, u64)>,
    /// Programas entre ellos: no se ejecutan al instalar, pero el modelo
    /// podría querer usarlos.
    pub scripts: usize,
    /// Por qué no se puede instalar, si es el caso.
    pub problem: Option<String>,
    /// El nombre que declara en su ficha, si no es el de su carpeta.
    declared: Option<String>,
    dir: PathBuf,
}

/// Lo descargado, en una carpeta propia que se borra al soltarlo.
#[derive(Debug)]
pub struct Bundle {
    pub from: String,
    pub found: Vec<Found>,
    tmp: Option<PathBuf>,
}

impl Drop for Bundle {
    fn drop(&mut self) {
        if let Some(t) = &self.tmp {
            let _ = std::fs::remove_dir_all(t);
        }
    }
}

/// Descarga (si hace falta) y busca habilidades.
pub fn fetch(from: &From) -> Result<Bundle, String> {
    let mut b = Bundle { from: from.label(), found: Vec::new(), tmp: None };
    let base = match from {
        From::Folder(p) => p.canonicalize().map_err(|e| e.to_string())?,
        From::GitHub { owner, repo, rev, path } => {
            let tmp = exec_tmp()?;
            b.tmp = Some(tmp.clone());
            let root = download(owner, repo, rev.as_deref().unwrap_or("HEAD"), &tmp)?;
            let base = root.join(path);
            // Dentro de lo descargado, sin enlaces que salgan.
            base.canonicalize()
                .ok()
                .filter(|p| p.starts_with(&root) && p.is_dir())
                .ok_or("Esa carpeta no está en el repositorio")?
        }
    };
    scan(&base, 0, &mut b.found);
    if b.found.is_empty() {
        return Err("No hay ninguna habilidad (SKILL.md) ahí".into());
    }
    b.found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(b)
}

/// Carpeta temporal nueva.
fn exec_tmp() -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let d = dirs::cache_dir()
        .ok_or("sin carpeta de caché")?
        .join("karbyx")
        .join("tmp")
        .join(format!("skill-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    d.canonicalize().map_err(|e| e.to_string())
}

fn download(owner: &str, repo: &str, rev: &str, tmp: &Path) -> Result<PathBuf, String> {
    let url = format!("https://codeload.github.com/{owner}/{repo}/tar.gz/{rev}");
    let file = tmp.join("descarga.tar.gz");
    let out = Command::new("/usr/bin/curl")
        .args(["-q", "-fsSL", "--proto", "=https", "--proto-redir", "=https", "--max-time", "90"])
        .args(["--max-filesize", &DOWNLOAD_MAX.to_string(), "-A", "karbyx", "-o"])
        .arg(&file)
        .args(["--", &url])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(match out.status.code() {
            Some(22) => "No encuentro ese repositorio (¿es privado o está mal escrito?)".into(),
            Some(63) => "El repositorio es demasiado grande".into(),
            Some(6 | 7 | 28) => "No llego a GitHub: revisa tu conexión".into(),
            c => format!("No se ha podido descargar (curl {})", c.unwrap_or(-1)),
        });
    }
    let root = tmp.join("contenido");
    let ok = unpack(&file, &root);
    let _ = std::fs::remove_file(&file);
    ok?;
    root.canonicalize().map_err(|e| e.to_string())
}

/// Extrae un `.tar.gz` sin su primera carpeta, sin dueños ni permisos.
fn unpack(file: &Path, root: &Path) -> Result<(), String> {
    std::fs::create_dir(root).map_err(|e| e.to_string())?;
    // bsdtar rechaza por sí mismo las rutas que salen y las que pasan por enlaces.
    let ok = Command::new("/usr/bin/tar")
        .args(["-x", "-z", "-o", "--no-same-permissions", "--no-acls", "--no-xattrs"])
        .args(["--strip-components", "1", "-f"])
        .arg(file)
        .arg("-C")
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok { Ok(()) } else { Err("Lo descargado está dañado o no es de fiar".into()) }
}

/// Busca carpetas con `SKILL.md`, sin seguir enlaces.
fn scan(dir: &Path, depth: usize, out: &mut Vec<Found>) {
    if out.len() >= FOUND_MAX {
        return;
    }
    if regular(&dir.join("SKILL.md")).is_some() {
        if let Some(f) = inspect(dir) {
            out.push(f);
        }
        return;
    }
    if depth >= 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| !p.ends_with(".git") && !p.ends_with("node_modules"))
        .collect();
    dirs.sort();
    for d in dirs {
        scan(&d, depth + 1, out);
    }
}

/// Tamaño, si es un archivo normal (no un enlace).
fn regular(p: &Path) -> Option<u64> {
    p.symlink_metadata().ok().filter(|m| m.is_file()).map(|m| m.len())
}

fn inspect(dir: &Path) -> Option<Found> {
    let size = regular(&dir.join("SKILL.md"))?;
    let text = if size <= know::SKILL_MAX {
        std::fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default()
    } else {
        String::new()
    };
    let (description, body) = know::parse_skill(&text);
    let raw = dir.file_name()?.to_string_lossy().into_owned();
    // El nombre de la carpeta o, si es la raíz descargada, el que declara.
    let declared =
        text.lines().find_map(|l| l.strip_prefix("name:")).map(|n| n.trim().trim_matches(['"', '\'']));
    let raw = if raw == "contenido" { declared.unwrap_or("habilidad").to_string() } else { raw };
    let declared = declared.map(str::to_string);
    let name: String = raw
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(40)
        .collect();
    let mut files = Vec::new();
    let mut problem = None;
    walk(dir, dir, 0, &mut files);
    let total: u64 = size + files.iter().map(|f| f.1).sum::<u64>();
    if text.trim().is_empty() || body.trim().is_empty() {
        problem = Some("su SKILL.md está vacío, no es texto o es demasiado grande".to_string());
    } else if !know::valid_name(&name) {
        problem = Some("su nombre no es válido".into());
    } else if files.len() > FILES_MAX || total > TOTAL_MAX || files.iter().any(|f| f.1 > FILE_MAX) {
        problem = Some("es demasiado grande".into());
    }
    let script = |p: &Path| {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default();
        matches!(ext, "sh" | "bash" | "zsh" | "py" | "js" | "mjs" | "ts" | "rb" | "pl" | "command" | "exe")
            || starts_with_shebang(&dir.join(p))
    };
    let scripts = files.iter().filter(|(p, _)| script(p)).count();
    Some(Found { name, description, text, files, scripts, problem, declared, dir: dir.to_path_buf() })
}

fn starts_with_shebang(p: &Path) -> bool {
    let mut b = [0u8; 2];
    std::fs::File::open(p).and_then(|mut f| f.read_exact(&mut b)).is_ok() && &b == b"#!"
}

/// Archivos normales de una carpeta, salvo `SKILL.md`.
fn walk(top: &Path, dir: &Path, depth: usize, out: &mut Vec<(PathBuf, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if out.len() > FILES_MAX {
            return;
        }
        let Ok(m) = p.symlink_metadata() else { continue };
        let Ok(rel) = p.strip_prefix(top) else { continue };
        if m.is_dir() && depth < 6 && !p.ends_with(".git") {
            walk(top, &p, depth + 1, out);
        } else if m.is_file() && rel != Path::new("SKILL.md") {
            out.push((rel.to_path_buf(), m.len()));
        }
    }
}

impl Bundle {
    /// La que se llama así, por su carpeta o por lo que declara.
    pub fn position(&self, name: &str) -> Option<usize> {
        let plain = |s: &str| -> String {
            let s: String =
                s.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
            s.trim_matches('-').to_string()
        };
        let want = plain(name);
        let is = |f: &Found| f.declared.as_deref().is_some_and(|d| plain(d) == want);
        self.found.iter().position(is).or_else(|| self.found.iter().position(|f| f.name == want))
    }
}

impl Found {
    /// Copia la habilidad a la carpeta del usuario. Los programas pierden el
    /// permiso de ejecución. Sustituye a la que hubiera con ese nombre.
    pub fn install(&self, skills: &Path, from: &str) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        if let Some(p) = &self.problem {
            return Err(p.clone());
        }
        let fail = |e: std::io::Error| format!("no se ha podido copiar: {e}");
        std::fs::create_dir_all(skills).map_err(fail)?;
        let new = skills.join(format!(".{}-nueva-{}", self.name, std::process::id()));
        let _ = std::fs::remove_dir_all(&new);
        std::fs::create_dir(&new).map_err(fail)?;
        let copy = || -> std::io::Result<()> {
            // Lo mismo que se enseñó, no lo que haya ahora en el disco.
            std::fs::write(new.join("SKILL.md"), &self.text)?;
            for (rel, size) in &self.files {
                let clean = rel.components().all(|c| matches!(c, Component::Normal(_)));
                let src = self.dir.join(rel);
                if !clean || regular(&src) != Some(*size) {
                    return Err(std::io::Error::other("ha cambiado desde que se miró"));
                }
                let dst = new.join(rel);
                if let Some(d) = dst.parent() {
                    std::fs::create_dir_all(d)?;
                }
                std::fs::write(&dst, std::fs::read(&src)?)?;
                std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o644))?;
            }
            std::fs::write(new.join(".origen"), format!("{from}\n"))
        };
        let done = copy().and_then(|()| {
            let dst = skills.join(&self.name);
            // La anterior se aparta hasta que la nueva esté en su sitio.
            let old = skills.join(format!(".{}-vieja-{}", self.name, std::process::id()));
            let had = dst.symlink_metadata().is_ok();
            if had {
                std::fs::rename(&dst, &old)?;
            }
            if let Err(e) = std::fs::rename(&new, &dst) {
                if had {
                    let _ = std::fs::rename(&old, &dst);
                }
                return Err(e);
            }
            if had {
                let _ = std::fs::remove_dir_all(&old).or_else(|_| std::fs::remove_file(&old));
            }
            Ok(())
        });
        if done.is_err() {
            let _ = std::fs::remove_dir_all(&new);
        }
        done.map_err(fail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Needs, skip};

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp(name: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("karbyx-inst-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Tmp(d.canonicalize().unwrap())
    }

    fn put(p: PathBuf, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn addresses() {
        let gh = |s: &str| match From::parse(s).unwrap() {
            From::GitHub { owner, repo, rev, path } => (owner, repo, rev.unwrap_or_default(), path),
            f => panic!("{f:?}"),
        };
        let t = |a: &str, b: &str, c: &str, d: &str| (a.into(), b.into(), c.into(), d.into());
        assert_eq!(gh("anthropics/skills"), t("anthropics", "skills", "", ""));
        assert_eq!(gh("https://github.com/a/b.git"), t("a", "b", "", ""));
        assert_eq!(gh("https://www.github.com/a/b/tree/main/skills/pdf/"), t("a", "b", "main", "skills/pdf"));
        assert_eq!(gh("github.com/a/b/blob/v1.2/x/SKILL.md?plain=1"), t("a", "b", "v1.2", "x"));
        assert_eq!(gh("a/b/skills/x"), t("a", "b", "", "skills/x"));
        for bad in
            ["", "a", "https://gitlab.com/a/b", "a/../b", "a/b/../../c", "a/b;rm", "-a/-b/$(x)", "/no/existe"]
        {
            assert!(From::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn finds_and_installs() {
        let t = tmp("instalar");
        let src = t.0.join("repo");
        put(
            src.join("skills/PDF Tools/SKILL.md"),
            "---\nname: pdf\ndescription: Cosas con PDF\n---\n\nPasos.\n",
        );
        put(src.join("skills/PDF Tools/scripts/x.py"), "print(1)");
        put(src.join("skills/PDF Tools/datos/a.txt"), "x");
        put(src.join("skills/vacia/SKILL.md"), "---\nname: x\n---\n");
        put(t.0.join("secreto.txt"), "secreto");
        std::os::unix::fs::symlink(t.0.join("secreto.txt"), src.join("skills/PDF Tools/enlace")).unwrap();
        std::os::unix::fs::symlink(&t.0, src.join("skills/escape")).unwrap();
        let b = fetch(&From::Folder(src)).unwrap();
        assert_eq!(b.found.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["pdf-tools", "vacia"]);
        let f = &b.found[0];
        assert_eq!((f.description.as_str(), f.scripts, f.files.len()), ("Cosas con PDF", 1, 2));
        // El catálogo usa el nombre declarado; también vale el de la carpeta.
        assert_eq!(
            (b.position("pdf"), b.position("PDF Tools"), b.position("nada")),
            (Some(0), Some(0), None)
        );
        assert!(b.found[1].problem.is_some() && b.found[1].install(&t.0.join("s"), "x").is_err());
        let skills = t.0.join("mias");
        put(skills.join("pdf-tools/SKILL.md"), "la anterior");
        put(skills.join("pdf-tools/viejo.txt"), "x");
        f.install(&skills, "github.com/a/b").unwrap();
        let d = skills.join("pdf-tools");
        assert!(std::fs::read_to_string(d.join("SKILL.md")).unwrap().contains("Pasos."));
        assert!(
            d.join("scripts/x.py").exists() && !d.join("enlace").exists() && !d.join("viejo.txt").exists()
        );
        assert_eq!(std::fs::read_to_string(d.join(".origen")).unwrap(), "github.com/a/b\n");
        assert_eq!(std::fs::read_dir(&skills).unwrap().count(), 1, "sin restos");
        // Si cambia entre mirarla e instalarla, no se instala a medias.
        std::fs::write(f.dir.join("scripts/x.py"), "otra cosa").unwrap();
        assert!(f.install(&skills, "x").is_err());
        assert_eq!(std::fs::read_dir(&skills).unwrap().count(), 1);
    }

    #[test]
    fn unpacks_safely() {
        if skip(&[Needs::Python]) {
            return;
        }
        let t = tmp("tar");
        let py = r#"
import sys, tarfile, io
def add(tar, name, data=b"x", link=None):
    i = tarfile.TarInfo(name)
    if link: i.type, i.linkname = tarfile.SYMTYPE, link
    else: i.size = len(data); i.mode = 0o4777
    tar.addfile(i, None if link else io.BytesIO(data))
with tarfile.open(sys.argv[1], "w:gz") as tar:
    add(tar, "repo-1/skills/a/SKILL.md", b"---\ndescription: d\n---\nhola\n")
with tarfile.open(sys.argv[2], "w:gz") as tar:
    add(tar, "repo-1/../../fuera.txt")
    add(tar, "repo-1/e", link="../..")
    add(tar, "repo-1/e/fuera2.txt")
"#;
        let (good, bad) = (t.0.join("b.tgz"), t.0.join("m.tgz"));
        let ok = Command::new("/usr/bin/python3").args(["-c", py]).args([&good, &bad]).status();
        if !ok.is_ok_and(|s| s.success()) {
            return;
        }
        let root = t.0.join("x/y/contenido");
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        unpack(&good, &root).unwrap();
        assert_eq!(fetch(&From::Folder(root)).unwrap().found[0].name, "a");
        let root = t.0.join("x/z/contenido");
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        let _ = unpack(&bad, &root);
        for p in ["x/fuera.txt", "x/z/fuera.txt", "x/fuera2.txt", "fuera.txt", "fuera2.txt", "x/z/fuera2.txt"]
        {
            assert!(!t.0.join(p).exists(), "se ha escapado: {p}");
        }
        assert!(unpack(&t.0.join("no.tgz"), &t.0.join("x/w")).is_err());
    }

    #[test]
    fn nothing_to_install() {
        let t = tmp("nada");
        assert!(fetch(&From::Folder(t.0.clone())).is_err());
    }
}

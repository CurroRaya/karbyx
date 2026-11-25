// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! Worktrees de git por agente y diffs para revisar su trabajo.
//!
//! Cada agente lanzado con ⌘⇧A trabaja en su propia copia del repositorio
//! (`git worktree`) y en su propia rama (`karbyx/agente-N`), junto al proyecto
//! en `<repo>.worktrees/agente-N`. Así varios agentes pueden tocar el mismo
//! proyecto a la vez sin pisarse, y tú decides qué se fusiona.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Prefijo de las ramas de los agentes.
pub const BRANCH_PREFIX: &str = "karbyx/";
/// Tamaño máximo de un archivo nuevo que se muestra entero.
const MAX_NEW_FILE: u64 = 256 * 1024;
/// Líneas máximas de diff por archivo (lo demás se resume).
const MAX_LINES: usize = 5000;

/// Worktree de un agente.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Worktree {
    /// Carpeta del worktree (donde trabaja el agente).
    pub path: PathBuf,
    /// Copia principal del repositorio (donde se fusiona).
    pub repo: PathBuf,
    /// Rama del agente, p. ej. `karbyx/agente-1`.
    pub branch: String,
    /// Commit del que partió.
    pub base: String,
}

impl Worktree {
    /// Nombre corto: `agente-1`.
    pub fn name(&self) -> &str {
        self.branch.strip_prefix(BRANCH_PREFIX).unwrap_or(&self.branch)
    }
}

/// Ejecuta git en `dir` y devuelve la salida estándar, o el primer mensaje
/// de error legible.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        // Que ningún repositorio (ni uno plantado dentro) lance programas al leerlo.
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .map_err(|e| format!("no se pudo ejecutar git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let out_text = String::from_utf8_lossy(&out.stdout);
        let msg = err
            .lines()
            .chain(out_text.lines())
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("git falló")
            .trim_start_matches("fatal: ")
            .trim_start_matches("error: ")
            .to_string();
        Err(msg)
    }
}

/// Raíz del repositorio (o worktree) que contiene `dir`.
pub fn toplevel(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"]).ok().map(|s| PathBuf::from(s.trim()))
}

/// Copia principal del repositorio de una carpeta raíz de git: ella misma,
/// o, si es un worktree (`.git` es un archivo), la copia de la que cuelga.
/// No lanza procesos: se usa en cada refresco.
pub fn main_checkout(root: &Path) -> PathBuf {
    let dotgit = root.join(".git");
    if dotgit.is_file()
        && let Ok(text) = std::fs::read_to_string(&dotgit)
        && let Some(gitdir) = text.trim().strip_prefix("gitdir:")
    {
        // gitdir: /ruta/repo/.git/worktrees/nombre
        let gitdir = Path::new(gitdir.trim());
        if let Some(dot) = gitdir.ancestors().find(|a| a.file_name().is_some_and(|n| n == ".git"))
            && let Some(repo) = dot.parent()
        {
            return repo.to_path_buf();
        }
    }
    root.to_path_buf()
}

/// Rama actual de una carpeta (vacío si HEAD está suelto).
pub fn current_branch(dir: &Path) -> String {
    git(dir, &["branch", "--show-current"]).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(repo, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok()
}

/// Commit actual de una carpeta.
pub fn head(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "HEAD"]).ok().map(|s| s.trim().to_string())
}

/// Si `dir` está en un worktree de agente de Karbyx (rama `karbyx/…`), lo
/// reconstruye: sirve para revisar uno cuyo panel ya se cerró.
pub fn detect(dir: &Path) -> Option<Worktree> {
    let top = toplevel(dir)?;
    let repo = main_checkout(&top);
    if repo == top {
        return None;
    }
    let branch = current_branch(&top);
    if !branch.starts_with(BRANCH_PREFIX) {
        return None;
    }
    let target = head(&repo)?;
    let base = git(&top, &["merge-base", "HEAD", &target]).ok()?.trim().to_string();
    Some(Worktree { path: top, repo, branch, base })
}

/// Crea un worktree nuevo para un agente a partir del HEAD del repositorio
/// que contiene `dir`.
pub fn create(dir: &Path) -> Result<Worktree, String> {
    let top = toplevel(dir).ok_or("esta carpeta no es un repositorio git")?;
    let repo = main_checkout(&top);
    let base =
        git(&top, &["rev-parse", "HEAD"]).map_err(|_| "el repositorio no tiene ningún commit todavía")?;
    let base = base.trim().to_string();
    let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
    let parent = repo.parent().unwrap_or(&repo);
    let holder = parent.join(format!("{name}.worktrees"));
    let n = (1..1000)
        .find(|n| {
            !holder.join(format!("agente-{n}")).exists()
                && !branch_exists(&repo, &format!("{BRANCH_PREFIX}agente-{n}"))
        })
        .ok_or("demasiados worktrees de agentes")?;
    let path = holder.join(format!("agente-{n}"));
    let branch = format!("{BRANCH_PREFIX}agente-{n}");
    std::fs::create_dir_all(&holder).map_err(|e| format!("no se pudo crear {}: {e}", holder.display()))?;
    let p = path.to_string_lossy();
    git(&top, &["worktree", "add", "-b", &branch, &p, &base])?;
    Ok(Worktree { path, repo, branch, base })
}

/// Fusiona el trabajo del agente en la rama actual de la copia principal:
/// primero guarda en un commit lo que no esté guardado. Devuelve un resumen.
pub fn merge(wt: &Worktree, message: &str) -> Result<String, String> {
    git(&wt.path, &["add", "-A"])?;
    let staged = git(&wt.path, &["diff", "--cached", "--quiet"]).is_err();
    if staged {
        git(&wt.path, &["commit", "-q", "-m", message])?;
    }
    let target = current_branch(&wt.repo);
    if target.is_empty() {
        return Err(format!("{} no está en ninguna rama (HEAD suelto)", wt.repo.display()));
    }
    let ahead = git(&wt.repo, &["rev-list", "--count", &format!("{target}..{}", wt.branch)])?;
    if ahead.trim() == "0" {
        return Ok(format!("{} no tiene cambios nuevos para {target}", wt.name()));
    }
    let msg = format!("Fusiona {} ({})", wt.branch, message);
    match git(&wt.repo, &["merge", "--no-ff", "-m", &msg, &wt.branch]) {
        Ok(_) => Ok(format!("{} fusionado en {target}", wt.name())),
        Err(e) => {
            // Si quedó a medias por conflictos, se deja todo como estaba.
            let merging = wt.repo.join(".git").join("MERGE_HEAD").exists();
            if merging {
                let _ = git(&wt.repo, &["merge", "--abort"]);
                Err(format!("hay conflictos entre {} y {target}: no se ha tocado nada", wt.name()))
            } else {
                Err(e)
            }
        }
    }
}

/// Borra el worktree y su rama (el trabajo no fusionado se pierde).
pub fn remove(wt: &Worktree) -> Result<(), String> {
    let p = wt.path.to_string_lossy();
    git(&wt.repo, &["worktree", "remove", "--force", &p])?;
    git(&wt.repo, &["branch", "-D", &wt.branch])?;
    // Si era el último, quita la carpeta contenedora vacía.
    if let Some(holder) = wt.path.parent() {
        let _ = std::fs::remove_dir(holder);
    }
    Ok(())
}

// ---------- diff ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    /// Cabecera de bloque (`@@ … @@`).
    Hunk,
    /// Aviso (binario, recortado…).
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// `M` modificado, `A` nuevo, `D` borrado, `R` renombrado.
    pub status: char,
    pub added: usize,
    pub removed: usize,
    pub lines: Vec<DiffLine>,
}

fn line(kind: LineKind, old: Option<u32>, new: Option<u32>, text: &str) -> DiffLine {
    DiffLine { kind, old, new, text: text.replace('\t', "    ") }
}

/// `@@ -12,3 +14,5 @@ fn x` → (12, 14).
fn hunk_start(h: &str) -> (u32, u32) {
    let mut parts = h.split_whitespace().skip(1);
    let num = |s: Option<&str>| {
        s.and_then(|s| s.get(1..)).and_then(|s| s.split(',').next()).and_then(|n| n.parse().ok()).unwrap_or(1)
    };
    (num(parts.next()), num(parts.next()))
}

/// Interpreta la salida de `git diff` (formato unificado).
pub fn parse(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    for l in text.lines() {
        if let Some(rest) = l.strip_prefix("diff --git ") {
            // "a/ruta b/ruta": la ruta de destino es la mitad derecha.
            let path = rest.rsplit_once(" b/").map(|(_, b)| b).unwrap_or(rest).to_string();
            files.push(FileDiff { path, status: 'M', added: 0, removed: 0, lines: Vec::new() });
            in_hunk = false;
            continue;
        }
        let Some(f) = files.last_mut() else { continue };
        if !in_hunk {
            if l.starts_with("new file") {
                f.status = 'A';
            } else if l.starts_with("deleted file") {
                f.status = 'D';
            } else if let Some(to) = l.strip_prefix("rename to ") {
                f.status = 'R';
                f.path = to.to_string();
            } else if l.starts_with("Binary files") {
                f.lines.push(line(LineKind::Note, None, None, "archivo binario"));
            }
        }
        if l.starts_with("@@") {
            in_hunk = true;
            (old, new) = hunk_start(l);
            f.lines.push(line(LineKind::Hunk, None, None, l));
            continue;
        }
        if !in_hunk {
            continue;
        }
        let truncated = f.lines.len() >= MAX_LINES;
        match l.as_bytes().first() {
            Some(b'+') => {
                f.added += 1;
                if !truncated {
                    f.lines.push(line(LineKind::Added, None, Some(new), &l[1..]));
                }
                new += 1;
            }
            Some(b'-') => {
                f.removed += 1;
                if !truncated {
                    f.lines.push(line(LineKind::Removed, Some(old), None, &l[1..]));
                }
                old += 1;
            }
            Some(b'\\') => {} // "\ No newline at end of file"
            _ => {
                if !truncated {
                    f.lines.push(line(LineKind::Context, Some(old), Some(new), l.get(1..).unwrap_or("")));
                }
                old += 1;
                new += 1;
            }
        }
    }
    for f in &mut files {
        if f.lines.len() >= MAX_LINES {
            f.lines.push(line(LineKind::Note, None, None, "… diff recortado (demasiado largo)"));
        }
    }
    files
}

/// Archivo sin seguimiento mostrado como nuevo entero.
fn untracked(top: &Path, rel: &str) -> FileDiff {
    let mut f = FileDiff { path: rel.to_string(), status: 'A', added: 0, removed: 0, lines: Vec::new() };
    let full = top.join(rel);
    // Un enlace no se sigue: podría apuntar a cualquier archivo del disco.
    let meta = std::fs::symlink_metadata(&full).ok();
    if meta.as_ref().is_some_and(|m| !m.is_file()) {
        f.lines.push(line(LineKind::Note, None, None, "enlace o archivo especial nuevo"));
        return f;
    }
    let size = meta.map(|m| m.len()).unwrap_or(0);
    if size > MAX_NEW_FILE {
        f.lines.push(line(LineKind::Note, None, None, &format!("archivo nuevo de {} KB", size / 1024)));
        return f;
    }
    let Ok(bytes) = std::fs::read(&full) else { return f };
    if bytes.contains(&0) {
        f.lines.push(line(LineKind::Note, None, None, "archivo binario nuevo"));
        return f;
    }
    let text = String::from_utf8_lossy(&bytes);
    f.lines.push(line(LineKind::Hunk, None, None, "archivo nuevo"));
    for (i, l) in text.lines().enumerate() {
        f.added += 1;
        if f.lines.len() < MAX_LINES {
            f.lines.push(line(LineKind::Added, None, Some(i as u32 + 1), l));
        }
    }
    f
}

/// Cambios de la carpeta respecto a `base` (un commit o `HEAD`): lo
/// guardado en commits, lo que no, y los archivos nuevos sin seguimiento.
/// No modifica nada (ni siquiera el índice).
pub fn diff(dir: &Path, base: &str) -> Result<Vec<FileDiff>, String> {
    let top = toplevel(dir).ok_or("esta carpeta no es un repositorio git")?;
    let text = git(&top, &["diff", "--no-color", "--no-ext-diff", "-M", base, "--"])?;
    let mut files = parse(&text);
    let others = git(&top, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for rel in others.split('\0').filter(|s| !s.is_empty()) {
        files.push(untracked(&top, rel));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,3 +10,4 @@ fn main() {
 uno
-dos
+DOS
+tres
 \tcuatro
\\ No newline at end of file
diff --git a/nuevo.txt b/nuevo.txt
new file mode 100644
--- /dev/null
+++ b/nuevo.txt
@@ -0,0 +1 @@
+hola
diff --git a/viejo.txt b/renombrado.txt
similarity index 90%
rename from viejo.txt
rename to renombrado.txt
diff --git a/img.png b/img.png
Binary files a/img.png and b/img.png differ
";

    #[test]
    fn parses_unified_diff() {
        let f = parse(SAMPLE);
        assert_eq!(f.len(), 4);
        assert_eq!((f[0].path.as_str(), f[0].status, f[0].added, f[0].removed), ("src/a.rs", 'M', 2, 1));
        let l = &f[0].lines;
        assert_eq!(l[0].kind, LineKind::Hunk);
        assert_eq!((l[1].old, l[1].new, l[1].text.as_str()), (Some(10), Some(10), "uno"));
        assert_eq!((l[2].kind, l[2].old), (LineKind::Removed, Some(11)));
        assert_eq!((l[3].kind, l[3].new), (LineKind::Added, Some(11)));
        assert_eq!((l[5].old, l[5].new, l[5].text.as_str()), (Some(12), Some(13), "    cuatro"));
        assert_eq!(l.len(), 6);
        assert_eq!((f[1].status, f[1].added), ('A', 1));
        assert_eq!((f[2].path.as_str(), f[2].status), ("renombrado.txt", 'R'));
        assert_eq!(f[3].lines[0].kind, LineKind::Note);
    }

    #[test]
    fn main_checkout_of_a_worktree() {
        let dir = std::env::temp_dir().join(format!("karbyx-wt-{}", std::process::id()));
        let wt = dir.join("repo.worktrees").join("agente-1");
        std::fs::create_dir_all(&wt).unwrap();
        let gitdir = dir.join("repo").join(".git").join("worktrees").join("agente-1");
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        assert_eq!(main_checkout(&wt), dir.join("repo"));
        assert_eq!(main_checkout(&dir), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// De punta a punta con un repositorio de verdad (si hay git).
    #[test]
    fn create_diff_merge_remove() {
        // Ruta real (en macOS /var es un enlace a /private/var, y git da la real).
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("karbyx-wt-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("proyecto");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |d: &Path, a: &[&str]| git(d, a);
        if run(&repo, &["init", "-q", "-b", "main"]).is_err() {
            return; // sin git en la máquina
        }
        run(&repo, &["config", "user.email", "test@example.com"]).unwrap();
        run(&repo, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(repo.join("a.txt"), "uno\n").unwrap();
        run(&repo, &["add", "."]).unwrap();
        run(&repo, &["commit", "-q", "-m", "inicio"]).unwrap();

        let wt = create(&repo).unwrap();
        assert_eq!(wt.branch, "karbyx/agente-1");
        assert_eq!(wt.path, dir.join("proyecto.worktrees").join("agente-1"));
        assert_eq!(main_checkout(&wt.path).canonicalize().unwrap(), repo.canonicalize().unwrap());
        // El segundo agente no pisa al primero.
        let wt2 = create(&repo).unwrap();
        assert_eq!(wt2.name(), "agente-2");
        assert_eq!(detect(&wt.path).as_ref(), Some(&wt));
        assert_eq!(detect(&repo), None);

        std::fs::write(wt.path.join("a.txt"), "uno\ndos\n").unwrap();
        std::fs::write(wt.path.join("b.txt"), "nuevo\n").unwrap();
        let d = diff(&wt.path, &wt.base).unwrap();
        let summary: Vec<_> = d.iter().map(|f| (f.path.as_str(), f.status, f.added, f.removed)).collect();
        assert_eq!(summary, [("a.txt", 'M', 1, 0), ("b.txt", 'A', 1, 0)]);

        assert_eq!(merge(&wt, "prueba").unwrap(), "agente-1 fusionado en main");
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "uno\ndos\n");
        assert!(repo.join("b.txt").exists());
        assert!(merge(&wt, "otra vez").unwrap().contains("no tiene cambios"));

        remove(&wt).unwrap();
        remove(&wt2).unwrap();
        assert!(!wt.path.exists());
        assert!(!branch_exists(&repo, "karbyx/agente-1"));
        assert!(!dir.join("proyecto.worktrees").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Guardar y leer los archivos propios de forge sin perder lo que hubiera.

use std::ffi::OsString;
use std::io::{Error, ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;

/// `archivo.json` → `archivo.json.lo-que-sea`.
fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = OsString::from(p);
    s.push(suffix);
    s.into()
}

/// Escribe de una vez (archivo nuevo y cambio de nombre), con esos permisos.
/// Si falla, lo que hubiera sigue igual.
pub fn write(file: &Path, text: &str, dir_mode: u32, mode: u32) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if let Some(d) = file.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(dir_mode).create(d)?;
    }
    // Cada escritura usa el suyo: dos a la vez no se mezclan.
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = with_suffix(file, &format!(".{}-{n}.tmp", std::process::id()));
    let put = || -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp)?;
        f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        f.write_all(text.as_bytes())?;
        std::fs::rename(&tmp, file)
    };
    put().inspect_err(|e| {
        // Si ya existía, no es nuestro.
        if e.kind() != ErrorKind::AlreadyExists {
            let _ = std::fs::remove_file(&tmp);
        }
    })
}

/// Como [`write`], solo para el usuario.
pub fn write_private(file: &Path, text: &str) -> std::io::Result<()> {
    write(file, text, 0o700, 0o600)
}

/// Lee un JSON. Si no existe, el valor por defecto. Si no se entiende, también,
/// pero se aparta para no machacarlo al guardar: devuelve dónde ha quedado.
pub fn load_json<T: DeserializeOwned + Default>(file: &Path) -> (T, Option<PathBuf>) {
    let read = || -> std::io::Result<T> {
        serde_json::from_str(&std::fs::read_to_string(file)?).map_err(Error::other)
    };
    match read() {
        Ok(v) => (v, None),
        Err(e) if e.kind() == ErrorKind::NotFound => (T::default(), None),
        Err(_) => {
            let to = (0..100)
                .map(|i| with_suffix(file, &if i == 0 { ".roto".to_string() } else { format!(".roto{i}") }))
                .find(|p| p.symlink_metadata().is_err());
            (T::default(), to.filter(|to| std::fs::rename(file, to).is_ok()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("forge-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn private_and_atomic() {
        let d = dir("priv");
        let p = d.join("a.json");
        write_private(&p, "{}").unwrap();
        write_private(&p, "{\"servers\":[]}").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&d), mode(&p)), (0o700, 0o600));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"servers\":[]}");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "sin restos");
        // Si no se puede, queda lo anterior y ningún resto.
        std::fs::create_dir(d.join("b")).unwrap();
        assert!(write_private(&d.join("b"), "x").is_err());
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 2);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn writes_at_once_do_not_mix() {
        let d = dir("vez");
        // Con el mismo nombre y distinta extensión, y con el mismo del todo.
        let write_many = |name: &str, c: &str| {
            let (p, text) = (d.join(name), c.repeat(50_000));
            move || (0..40).for_each(|_| write_private(&p, &text).unwrap())
        };
        std::thread::scope(|s| {
            s.spawn(write_many("chat.json", "a"));
            s.spawn(write_many("chat.sesiones", "b"));
            s.spawn(write_many("chat.json", "a"));
        });
        for (n, c) in [("chat.json", "a"), ("chat.sesiones", "b")] {
            assert!(std::fs::read_to_string(d.join(n)).unwrap() == c.repeat(50_000), "{n}");
        }
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 2);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn damaged_files_are_kept() {
        let d = dir("roto");
        let p = d.join("a.json");
        assert_eq!(load_json::<Vec<u8>>(&p), (vec![], None));
        write_private(&p, "[1,2]").unwrap();
        assert_eq!(load_json::<Vec<u8>>(&p), (vec![1, 2], None));
        for (i, name) in ["a.json.roto", "a.json.roto1"].into_iter().enumerate() {
            write_private(&p, &format!("[1,{i}")).unwrap();
            assert_eq!(load_json::<Vec<u8>>(&p), (vec![], Some(d.join(name))));
            assert_eq!(std::fs::read_to_string(d.join(name)).unwrap(), format!("[1,{i}"));
            assert!(!p.exists());
        }
        let _ = std::fs::remove_dir_all(d);
    }
}

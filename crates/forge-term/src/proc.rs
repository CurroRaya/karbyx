// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 CurroRaya y los colaboradores de forge

//! Información del proceso que corre en el PTY: nombre del proceso en primer
//! plano y su directorio de trabajo. Sirve para titular las sesiones
//! ("nvim · ~/Rust/forge") sin depender de que la shell envíe secuencias OSC.

use std::path::PathBuf;

/// Proceso en primer plano del terminal (el grupo que tiene el control del TTY).
#[cfg(target_os = "macos")]
pub fn foreground_pid(master_fd: i32) -> Option<i32> {
    // SAFETY: tcgetpgrp solo consulta el descriptor; si no es válido devuelve -1.
    let pgid = unsafe { libc::tcgetpgrp(master_fd) };
    (pgid > 0).then_some(pgid)
}

#[cfg(target_os = "macos")]
pub fn process_name(pid: i32) -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: el buffer es válido y se pasa su tamaño real.
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

#[cfg(target_os = "macos")]
pub fn process_cwd(pid: i32) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::mem::{MaybeUninit, size_of};

    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = size_of::<libc::proc_vnodepathinfo>() as i32;
    // SAFETY: el buffer tiene exactamente el tamaño de la estructura pedida.
    let n =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, info.as_mut_ptr().cast(), size) };
    if n != size {
        return None;
    }
    // SAFETY: proc_pidinfo ha rellenado la estructura completa.
    let info = unsafe { info.assume_init() };
    let raw = &info.pvi_cdir.vip_path;
    // vip_path es [[c_char; 32]; 32] = un buffer contiguo de MAXPATHLEN.
    // SAFETY: es memoria contigua de 1024 bytes terminada en NUL por el kernel.
    let bytes = unsafe { std::slice::from_raw_parts(raw.as_ptr().cast::<u8>(), 32 * 32) };
    let cstr = CStr::from_bytes_until_nul(bytes).ok()?;
    let s = cstr.to_str().ok()?;
    (!s.is_empty()).then(|| PathBuf::from(s))
}

#[cfg(not(target_os = "macos"))]
pub fn foreground_pid(_master_fd: i32) -> Option<i32> {
    None
}
#[cfg(not(target_os = "macos"))]
pub fn process_name(_pid: i32) -> Option<String> {
    None
}
#[cfg(not(target_os = "macos"))]
pub fn process_cwd(_pid: i32) -> Option<PathBuf> {
    None
}

/// Acorta una ruta sustituyendo el home por `~`.
pub fn tilde(path: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
    }
    path.display().to_string()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn own_process_info() {
        let pid = std::process::id() as i32;
        assert!(process_name(pid).is_some());
        assert_eq!(process_cwd(pid), std::env::current_dir().ok());
    }
}

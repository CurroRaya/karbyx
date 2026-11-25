// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Para los tests: saltarse, diciéndolo, los que aquí no se pueden ejecutar
//! (por ejemplo, dentro del sandbox de forge, que no deja abrir puertos).

use std::io::Write;
use std::process::Command;

use crate::exec;

#[derive(Debug, Clone, Copy)]
pub enum Needs {
    /// Escuchar en un puerto local.
    Ports,
    /// Ejecutar comandos: escribir en la caché de forge.
    Exec,
    /// Aislar un comando (dentro de un sandbox no se puede crear otro).
    Sandbox,
    Python,
}

impl Needs {
    fn missing(self) -> bool {
        let ok = |program: &str, args: &[&str]| {
            Command::new(program).args(args).output().is_ok_and(|o| o.status.success())
        };
        match self {
            Self::Ports => std::net::TcpListener::bind("127.0.0.1:0").is_err(),
            Self::Exec => exec::TempDir::new().is_none(),
            Self::Sandbox => {
                Self::Exec.missing()
                    || !ok("/usr/bin/sandbox-exec", &["-p", "(version 1)(allow default)", "/usr/bin/true"])
            }
            Self::Python => !ok("/usr/bin/python3", &["--version"]),
        }
    }
}

/// ¿Hay que saltarse el test? Con `FORGE_TESTS_STRICT`, falla en vez de saltarse.
pub fn skip(needs: &[Needs]) -> bool {
    let Some(n) = needs.iter().find(|n| n.missing()) else { return false };
    let name = std::thread::current().name().unwrap_or("?").to_string();
    assert!(std::env::var_os("FORGE_TESTS_STRICT").is_none(), "{name} necesita {n:?}");
    // Directo a la salida de error: la de los tests que pasan no se enseña.
    // De una vez, para no mezclarse con los demás.
    let _ = std::io::stderr().write_all(format!("\nSALTADO {name}: aquí no hay {n:?}\n").as_bytes());
    true
}

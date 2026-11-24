// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! El agente de forge: conversa con modelos locales (llama.cpp, Strata...) y
//! trabaja en un proyecto con herramientas, pidiendo permiso. Sin interfaz.

pub mod agent;
pub mod api;
mod compact;
pub mod exec;
pub mod http;
pub mod install;
pub mod know;
pub mod mcp;
pub mod sandbox;
pub mod tools;

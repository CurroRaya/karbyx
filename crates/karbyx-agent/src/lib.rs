// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx

//! El agente de Karbyx: conversa con modelos locales (llama.cpp, Strata...) y
//! trabaja en un proyecto con herramientas, pidiendo permiso. Sin interfaz.

pub mod agent;
pub mod api;
pub mod browse;
pub mod catalog;
mod compact;
pub mod exec;
pub mod files;
pub mod http;
pub mod install;
pub mod know;
pub mod mcp;
pub mod oauth;
pub mod registry;
mod remote;
pub mod sandbox;
#[doc(hidden)]
pub mod testing;
pub mod tools;
pub mod web;

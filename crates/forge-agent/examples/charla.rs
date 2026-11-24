// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Prueba del agente sin interfaz:
//! `cargo run --example charla -- http://mi-pc:8080 /ruta/proyecto "tarea"`
//! Permite sin preguntar lo normal; lo peligroso lo niega.

use std::sync::mpsc;

use forge_agent::agent::{Agent, Decision, Event, Setup};
use forge_agent::api::{self, Server};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let [url, root, task] = a.as_slice() else {
        eprintln!("uso: charla <servidor> <carpeta> <tarea>");
        return;
    };
    let server = Server { name: "prueba".into(), url: url.clone(), key: None };
    let models = match api::probe(&server) {
        Ok(m) => m,
        Err(e) => return eprintln!("{e}"),
    };
    eprintln!("{models:?}");
    let (tx, rx) = mpsc::channel();
    let mut agent = Agent::spawn(vec![], move |e| drop(tx.send(e)));
    agent.set_auto(true);
    let setup = Setup { server, model: models[0].id.clone(), ctx: models[0].ctx, root: Some(root.into()) };
    agent.prompt(task.clone(), setup);
    for e in rx {
        match e {
            Event::Text(t) => print!("{t}"),
            Event::Reasoning(_) | Event::Preparing(..) | Event::Output(..) => {}
            Event::Action { id, ask: Some(_), .. } => agent.answer(id, Decision::Deny("not allowed".into())),
            Event::Done => break,
            e => {
                let s = format!("{e:?}");
                println!("\n· {}", s.chars().take(300).collect::<String>());
            }
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge

//! Arranca un proceso real en un PTY y comprueba que su salida llega al grid.

use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_term::{GridSize, Session, SessionOptions, TermEvent, Theme};

#[test]
fn echo_reaches_grid() {
    let size = GridSize { cols: 40, rows: 5, cell_width: 8, cell_height: 16 };
    let opts = SessionOptions {
        command: Some(("/bin/sh".into(), vec!["-c".into(), "printf 'forge-ok\\033[31mRED'; sleep 1".into()])),
        ..Default::default()
    };
    let s = Session::spawn(opts, size, Theme::default(), Arc::new(|| {})).expect("spawn");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snap = s.snapshot();
        let line: String = snap.row(0).iter().map(|c| c.c).collect();
        if line.starts_with("forge-okRED") {
            // "R" en rojo (ANSI 31 → ansi[1] del tema).
            assert_eq!(snap.row(0)[8].fg, Theme::default().ansi[1]);
            assert_eq!(snap.row(0)[0].fg, Theme::default().foreground);
            break;
        }
        assert!(Instant::now() < deadline, "no llegó la salida: {line:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    // Al terminar el proceso llega Exited.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !s.poll_events().iter().any(|e| matches!(e, TermEvent::Exited)) {
        assert!(Instant::now() < deadline, "no llegó Exited");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for(s: &Session, pred: impl Fn(&forge_term::Snapshot) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pred(&s.snapshot()) {
        assert!(Instant::now() < deadline, "timeout esperando contenido");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn spawn_printing(text: &str) -> Session {
    let size = GridSize { cols: 40, rows: 6, cell_width: 8, cell_height: 16 };
    let opts = SessionOptions {
        command: Some(("/bin/sh".into(), vec!["-c".into(), format!("printf '{text}'; sleep 3")])),
        ..Default::default()
    };
    Session::spawn(opts, size, Theme::default(), Arc::new(|| {})).expect("spawn")
}

#[test]
fn selection_word_line_and_drag() {
    use forge_term::SelectionKind;
    let s = spawn_printing("hola mundo forge\\nsegunda linea");
    wait_for(&s, |sn| sn.row(1).iter().map(|c| c.c).collect::<String>().starts_with("segunda"));

    s.start_selection(SelectionKind::Word, 0, 6, false);
    assert_eq!(s.selection_text().as_deref(), Some("mundo"));

    s.start_selection(SelectionKind::Line, 1, 3, false);
    assert_eq!(s.selection_text().as_deref().map(str::trim_end), Some("segunda linea"));

    s.start_selection(SelectionKind::Simple, 0, 0, false);
    s.update_selection(0, 3, true);
    assert_eq!(s.selection_text().as_deref(), Some("hola"));
    // La celda seleccionada se pinta con el color de selección.
    assert_eq!(s.snapshot().row(0)[1].bg, Theme::default().selection);

    s.clear_selection();
    assert!(s.selection_text().is_none());
}

#[test]
fn selection_goes_beyond_the_screen() {
    use forge_term::SelectionKind;
    let lines: String = (1..=30).map(|i| format!("linea{i:02}\\n")).collect();
    let s = spawn_printing(&lines);
    wait_for(&s, |sn| sn.row(4).iter().map(|c| c.c).collect::<String>().starts_with("linea30"));

    s.select_all();
    let t = s.selection_text().unwrap();
    assert!(t.starts_with("linea01") && t.contains("linea15") && t.trim_end().ends_with("linea30"));

    // Se empieza abajo, se desplaza y se sigue arriba.
    s.start_selection(SelectionKind::Simple, 4, 6, true);
    s.scroll(20);
    s.update_selection(0, 0, false);
    let t = s.selection_text().unwrap();
    assert!(t.starts_with("linea06") && t.ends_with("linea30"), "{t}");
}

#[test]
fn search_highlights_and_steps() {
    let s = spawn_printing("uno Forge\\ndos forge\\ntres");
    wait_for(&s, |sn| sn.row(2).iter().map(|c| c.c).collect::<String>().starts_with("tres"));
    let t = Theme::default();

    assert!(s.search_set("forge")); // smart case: encuentra Forge y forge
    let snap = s.snapshot();
    // La más reciente (fila 1) es la actual; la otra, coincidencia normal.
    assert_eq!(snap.row(1)[4].bg, t.search_current);
    assert_eq!(snap.row(0)[4].bg, t.search_match);

    assert!(s.search_step(true));
    assert_eq!(s.snapshot().row(0)[4].bg, t.search_current);

    assert!(!s.search_set("zzz"));
    s.search_clear();
    assert!(!s.search_active());
    assert_eq!(s.snapshot().row(0)[4].bg, t.background);
}

#[test]
fn blocks_from_manual_marks() {
    // Dos comandos: el primero falla con código 2.
    let script = r"printf '\033]133;A\007$ \033]133;B\007ls -x\033]633;E;ls -x\007\033]133;C\007\nout1\nout2\n\033]133;D;2\007\033]133;A\007$ \033]133;B\007'; sleep 3";
    let size = GridSize { cols: 40, rows: 8, cell_width: 8, cell_height: 16 };
    let opts = SessionOptions {
        command: Some(("/bin/sh".into(), vec!["-c".into(), script.into()])),
        ..Default::default()
    };
    let s = Session::spawn(opts, size, Theme::default(), Arc::new(|| {})).expect("spawn");
    wait_for(&s, |sn| sn.blocks.len() == 2);

    let snap = s.snapshot();
    let (b1, b2) = (&snap.blocks[0], &snap.blocks[1]);
    assert!(b1.starts_here && b2.starts_here);
    assert_eq!((b1.first_row, b1.last_row), (0, 2));
    assert_eq!(b1.output_row, Some(1));
    assert_eq!(b1.info.exit, Some(2));
    assert!(b1.info.failed());
    assert_eq!(b1.info.command.as_deref(), Some("ls -x"));
    assert_eq!(b2.first_row, 3);
    assert!(b2.info.exit.is_none(), "el prompt actual aún no ha terminado");

    let text = s.block_text(b1.id).expect("texto");
    assert_eq!(text.output, "out1\nout2");
    assert_eq!(text.command.as_deref(), Some("ls -x"));

    // El evento de fin de comando llega con su código.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if s.poll_events().iter().any(|e| matches!(e, TermEvent::CommandFinished { exit: Some(2), .. })) {
            break;
        }
        assert!(Instant::now() < deadline, "no llegó CommandFinished");
        std::thread::sleep(Duration::from_millis(20));
    }

    // Saltar entre bloques: desde nada, ↑ va al último comando terminado.
    assert_eq!(s.jump_block(None, true), Some(b1.id));
    assert_eq!(s.jump_block(Some(b1.id), false), Some(b2.id));
    assert_eq!(s.jump_block(Some(b1.id), true), None);
}

#[test]
fn zsh_integration_end_to_end() {
    if !std::path::Path::new("/bin/zsh").exists() {
        return;
    }
    // Configuración de usuario vacía para que el test no dependa de ~/.zshrc.
    let user = std::env::temp_dir().join(format!("forge-zdot-{}", std::process::id()));
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(user.join(".zshrc"), "PS1='%# '\n").unwrap();
    let mut env = forge_term::shell::zsh_env("/bin/zsh").expect("integración zsh");
    env.insert("FORGE_USER_ZDOTDIR".into(), user.to_string_lossy().into_owned());

    let size = GridSize { cols: 60, rows: 12, cell_width: 8, cell_height: 16 };
    let opts =
        SessionOptions { command: Some(("/bin/zsh".into(), vec!["-i".into()])), env, ..Default::default() };
    let s = Session::spawn(opts, size, Theme::default(), Arc::new(|| {})).expect("spawn");
    wait_for(&s, |sn| !sn.blocks.is_empty());
    s.write(b"echo hola\r".to_vec());
    wait_for(&s, |sn| sn.blocks.len() >= 2);
    s.write(b"false\r".to_vec());
    wait_for(&s, |sn| sn.blocks.len() >= 3 && sn.blocks.iter().any(|b| b.info.exit == Some(1)));

    let snap = s.snapshot();
    let echo =
        snap.blocks.iter().find(|b| b.info.command.as_deref() == Some("echo hola")).expect("bloque echo");
    assert_eq!(echo.info.exit, Some(0));
    assert_eq!(s.block_text(echo.id).unwrap().output, "hola");
    let fail = snap.blocks.iter().find(|b| b.info.command.as_deref() == Some("false")).expect("bloque false");
    assert!(fail.info.failed());
    std::fs::remove_dir_all(user).ok();
}

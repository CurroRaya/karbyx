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

#![cfg(windows)]

use ui::time::Duration;
use ui::widget::Widget;

#[test]
fn embedded_windows_terminal_runs_a_command_and_receives_its_output() {
    let mut app = code_editor::CodeApp::new();
    let mut state = ui::State::new();
    app.shots_open_terminal();
    // The echoed command contains the prompt and `echo`; only its output is
    // an entire line equal to the marker.
    app.shots_terminal_input("echo TILO_CODE_CONPTY_READY\r");
    let start = ui::time::now();
    loop {
        app.sync(&mut state.doc, &state.root);
        let output = app.shots_terminal_grid_text().unwrap_or_default();
        if output
            .lines()
            .any(|line| line.trim() == "TILO_CODE_CONPTY_READY")
        {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "embedded terminal did not execute the command; grid: {output:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

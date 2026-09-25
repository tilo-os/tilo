use automerge::AutoCommit;
use portable_pty::{MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use ui::history_mode::{
    HistoryPresentation, HistoryRegistration, HistorySourceRegistration, ManagedRoot,
};
use ui::widget::Widget;
use vello::kurbo::Rect;

const EXPECTED_LIMITATION: &str = "Terminal History cannot reconstruct or restore a live process, its PTY, or process-produced screen output.";

#[test]
fn real_terminal_root_declares_truthful_external_process_output_history() {
    let (widget, _writes, _output) = crate::tests::build_test_widget_io();
    let definition = widget
        .document_history_definition()
        .expect("the real Terminal root must declare framework History");
    assert_eq!(definition.app_id, "so.tilo.terminal");
    assert_eq!(definition.scopes.len(), 1);
    let scope = &definition.scopes[0];
    assert_eq!(scope.key, "process-output");
    assert_eq!(scope.title, "Process output (not recorded)");
    assert!(scope.content_schema.is_none());
    assert!(scope.presentation.preview_factory().is_none());
    let HistoryPresentation::External { reason } = scope.presentation else {
        panic!("Terminal process output must be explicitly unversioned");
    };
    assert_eq!(reason, EXPECTED_LIMITATION);
}

fn managed_driver(widget: crate::TerminalWidget, doc: AutoCommit) -> ui::headless::Driver {
    let definition = widget
        .document_history_definition()
        .expect("the real Terminal root declares framework History");
    let scope = &definition.scopes[0];
    let HistoryPresentation::External { reason } = scope.presentation else {
        panic!("Terminal process output must be external");
    };
    let registration = HistoryRegistration::new(
        definition,
        vec![HistorySourceRegistration::Unversioned {
            scope: scope.key,
            reason,
        }],
    )
    .unwrap();
    ui::headless::Driver::from_managed_root(
        ui::State::from_doc(doc),
        ManagedRoot::new(Box::new(widget), registration),
        (800, 240),
    )
    .unwrap()
}

fn find_terminal_semantics(inspection: &str) -> Option<serde_json::Value> {
    fn find(value: &serde_json::Value) -> Option<&serde_json::Value> {
        if value
            .get("semantics")
            .and_then(|semantics| semantics.get("action_target"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|target| target.starts_with("terminal.transcript:"))
        {
            return value.get("semantics");
        }
        value
            .get("children")
            .and_then(serde_json::Value::as_array)
            .and_then(|children| children.iter().find_map(find))
    }
    let value: serde_json::Value = serde_json::from_str(inspection).unwrap();
    find(&value["root"]).cloned()
}

fn terminal_semantics(inspection: &str) -> serde_json::Value {
    find_terminal_semantics(inspection)
        .unwrap_or_else(|| panic!("Terminal semantics must be present: {inspection}"))
}

struct RecordingMaster(crossbeam_channel::Sender<PtySize>);

impl MasterPty for RecordingMaster {
    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        Ok(PtySize::default())
    }

    fn resize(&self, size: PtySize) -> Result<(), anyhow::Error> {
        let _ = self.0.send(size);
        Ok(())
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
        Ok(Box::new(std::io::sink()))
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
        Ok(Box::new(std::io::empty()))
    }

    fn process_group_leader(&self) -> Option<i32> {
        None
    }

    fn as_raw_fd(&self) -> Option<i32> {
        None
    }

    fn tty_name(&self) -> Option<std::path::PathBuf> {
        None
    }
}

struct ChannelWriter(crossbeam_channel::Sender<Vec<u8>>);

impl Write for ChannelWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.send(bytes.to_vec());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn recording_widget() -> (
    crate::TerminalWidget,
    crossbeam_channel::Receiver<Vec<u8>>,
    crossbeam_channel::Sender<Vec<u8>>,
    crossbeam_channel::Receiver<PtySize>,
) {
    let (write_tx, writes) = crossbeam_channel::unbounded();
    let (output, output_rx) = crossbeam_channel::unbounded();
    let (resize_tx, resizes) = crossbeam_channel::unbounded();
    let widget = crate::TerminalWidget::new(
        Arc::new(Mutex::new(Box::new(ChannelWriter(write_tx)))),
        Arc::new(Mutex::new(Box::new(RecordingMaster(resize_tx)))),
        output_rx,
    );
    (widget, writes, output, resizes)
}

fn resource_snapshot(driver: &ui::headless::Driver) -> Vec<(String, String)> {
    fn collect(resource: &ui::tilo_actions::Resource, out: &mut Vec<(String, String)>) {
        out.push((resource.id.0.clone(), resource.label.clone()));
        for child in &resource.children {
            collect(child, out);
        }
    }
    let mut out = Vec::new();
    for resource in driver.resources() {
        collect(&resource, &mut out);
    }
    out
}

#[test]
fn history_round_trip_preserves_exact_live_view_selection_focus_title_mode_and_transport() {
    let (mut widget, writes, _output) = crate::tests::build_test_widget_io();
    let waker = widget.waker_handle();
    widget.set_select_to_copy(false);
    widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);
    let mut doc = AutoCommit::new();
    let mut payload = b"\x1b]2;Retained Session\x07\x1b[?1h".to_vec();
    for line in 0..60 {
        payload.extend_from_slice(format!("HISTORY-LINE-{line:03}\r\n").as_bytes());
    }
    widget.feed(&mut doc, &automerge::ROOT, &payload);
    widget.sync(&mut doc, &automerge::ROOT);
    assert!(widget.modes().app_cursor_keys);
    assert_eq!(widget.title(), "Retained Session");
    widget.scroll_to_top();
    assert!(widget.scroll_offset() > 0);
    widget.set_selection_cells(
        crate::GridPos { row: 2, col: 1 },
        crate::GridPos { row: 2, col: 4 },
    );
    widget.set_focus(true);

    let mut driver = managed_driver(widget, doc);
    let before = driver.inspection_json().unwrap();
    let before_terminal = terminal_semantics(&before);
    let before_resources = resource_snapshot(&driver);
    assert_eq!(before_terminal["name"], "Retained Session");
    assert_eq!(before_terminal["text_selection"]["anchor"], 1);
    assert_eq!(before_terminal["text_selection"]["focus"], 4);
    assert_eq!(before_terminal["state"]["focused"], true);
    assert!(
        before_terminal["value"]
            .as_str()
            .is_some_and(|value| value.contains("HISTORY-LINE-002"))
    );
    assert!(crate::tests::drain_writes(&writes).is_empty());

    driver.click_named("history-open").unwrap();
    let history = driver.inspection_json().unwrap();
    assert!(history.contains(EXPECTED_LIMITATION));
    assert!(
        find_terminal_semantics(&history).is_none(),
        "an External limitation has no historical terminal canvas"
    );
    assert!(!driver.targets().contains_key("activity_timeline"));
    assert!(!driver.targets().contains_key("history-before"));
    assert!(!driver.targets().contains_key("history-after"));
    assert!(!driver.targets().contains_key("history-restore"));
    assert!(crate::tests::drain_writes(&writes).is_empty());

    driver.click_named("history-open").unwrap();
    let returned = driver.inspection_json().unwrap();
    assert_eq!(terminal_semantics(&returned), before_terminal);
    assert_eq!(resource_snapshot(&driver), before_resources);
    assert_eq!(
        std::sync::Arc::strong_count(&waker),
        2,
        "the managed root still owns the captured production waker"
    );
    assert!(crate::tests::drain_writes(&writes).is_empty());

    driver.key("ArrowUp");
    assert_eq!(
        crate::tests::drain_writes(&writes),
        b"\x1bOA",
        "the retained application-cursor mode and PTY writer remain live"
    );
}

#[test]
fn external_history_does_not_resize_or_reset_the_live_terminal_at_any_width() {
    for width in [390_u32, 900_u32] {
        let (mut widget, writes, output, resizes) = recording_widget();
        let waker = widget.waker_handle();
        let mut doc = AutoCommit::new();
        widget.feed(
            &mut doc,
            &automerge::ROOT,
            b"\x1b]2;Geometry Session\x07\x1b[?1h",
        );
        let mut driver = managed_driver(widget, doc);
        driver.resize((width, 240)).unwrap();

        output.send(b"\x1b[2;4r\x1b[2;999H".to_vec()).unwrap();
        driver.relayout();
        output.send(b"\x1b[6n".to_vec()).unwrap();
        driver.relayout();
        let right_edge_report = crate::tests::drain_writes(&writes);
        assert!(right_edge_report.starts_with(b"\x1b[2;"));
        assert!(right_edge_report.ends_with(b"R"));
        let _settled_resizes: Vec<_> = resizes.try_iter().collect();

        let before = driver.inspection_json().unwrap();
        let before_terminal = terminal_semantics(&before);
        let before_resources = resource_snapshot(&driver);
        assert_eq!(before_terminal["name"], "Geometry Session");
        assert_eq!(std::sync::Arc::strong_count(&waker), 2);

        driver.click_named("history-open").unwrap();
        let history = driver.inspection_json().unwrap();
        assert!(history.contains(EXPECTED_LIMITATION));
        assert!(
            resizes.is_empty(),
            "opening History resized the PTY at width {width}"
        );
        assert!(find_terminal_semantics(&history).is_none());
        assert!(crate::tests::drain_writes(&writes).is_empty());

        driver.click_named("history-open").unwrap();
        assert_eq!(
            terminal_semantics(&driver.inspection_json().unwrap()),
            before_terminal
        );
        assert_eq!(resource_snapshot(&driver), before_resources);
        assert!(
            resizes.is_empty(),
            "closing History resized the PTY at width {width}"
        );
        assert_eq!(std::sync::Arc::strong_count(&waker), 2);

        output.send(b"\x1b[6n".to_vec()).unwrap();
        driver.relayout();
        assert_eq!(
            crate::tests::drain_writes(&writes),
            right_edge_report,
            "History must preserve the right-edge cursor at width {width}"
        );
        output.send(b"\x1bM\x1b[6n".to_vec()).unwrap();
        driver.relayout();
        assert_eq!(
            crate::tests::drain_writes(&writes),
            right_edge_report,
            "reverse index at the top of the retained 2..4 scroll region must keep the cursor at row 2 and the right edge at width {width}"
        );

        let _ = resizes.try_iter().collect::<Vec<_>>();
        driver.resize((width + 80, 240)).unwrap();
        assert!(
            !resizes.is_empty(),
            "ordinary live window resize must still reach the PTY at width {width}"
        );
    }
}

#[test]
fn live_output_is_adopted_once_while_history_is_open_without_terminal_effects_or_rebuild() {
    let (mut widget, writes, output) = crate::tests::build_test_widget_io();
    widget.force_geometry(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 20.0);
    let mut doc = AutoCommit::new();
    widget.feed(&mut doc, &automerge::ROOT, b"INITIAL-LIVE-LINE\r\n");
    widget.sync(&mut doc, &automerge::ROOT);
    let mut driver = managed_driver(widget, doc);
    let resources = resource_snapshot(&driver);

    driver.click_named("history-open").unwrap();
    output.send(b"LIVE-DELTA-7\r\n".to_vec()).unwrap();
    output.send(b"\x1b[5n".to_vec()).unwrap();
    driver.relayout();
    let history = driver.inspection_json().unwrap();
    assert!(history.contains(EXPECTED_LIMITATION));
    assert!(find_terminal_semantics(&history).is_none());
    assert_eq!(
        crate::tests::drain_writes(&writes),
        b"\x1b[0n",
        "the retained parser answers legitimate process queries while hidden"
    );

    driver.click_named("history-open").unwrap();
    let returned = driver.inspection_json().unwrap();
    assert_eq!(returned.matches("LIVE-DELTA-7").count(), 1);
    assert_eq!(resource_snapshot(&driver), resources);
    assert!(crate::tests::drain_writes(&writes).is_empty());

    driver.click_named("history-open").unwrap();
    driver.click_named("history-open").unwrap();
    assert_eq!(resource_snapshot(&driver), resources);
    assert!(crate::tests::drain_writes(&writes).is_empty());
    drop(driver);
    assert!(
        output.send(b"AFTER-DROP\r\n".to_vec()).is_err(),
        "one retained output receiver lives exactly as long as the managed root"
    );
}

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::time::Duration;
use ui::widget::toast::{ToastKind, ToastStack};

#[test]
fn toast_stack_expires_entries() {
    let mut s = ToastStack::new();
    s.push("Workspace created", ToastKind::Info);
    s.push("Save failed", ToastKind::Error);
    assert_eq!(s.len(), 2);
    s.tick(Duration::from_millis(3100)); // default 3s TTL elapsed
    assert_eq!(s.len(), 0);
}

#[test]
fn toast_stack_caps_entries() {
    let mut s = ToastStack::new().with_max(3);
    for i in 0..5 {
        s.push(format!("t{i}"), ToastKind::Info);
    }
    assert_eq!(s.len(), 3);
    assert!(s.iter().any(|t| t.text == "t4"));
    assert!(!s.iter().any(|t| t.text == "t0"));
}

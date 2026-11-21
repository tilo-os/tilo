//! Calendar's sidebar sweep: the pointer moving between two rows repaints the
//! rows it moved between and nothing wider, within the pointer-motion budget.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

#[test]
fn sidebar_hover() {
    ui::framework::testing::perf::sidebar_sweep::<calendar::CalendarShell>(2)
        .expect("calendar opens")
        .print()
        .assert_each_hover_repaints_only_its_rows();
}

//! Files' sidebar sweep: the pointer moving between two rows repaints the
//! rows it moved between and nothing wider, within the pointer-motion budget.
#![allow(
    clippy::expect_used,
    reason = "a measurement fails loudly when the app does not open"
)]

#[test]
fn sidebar_hover() {
    let sweep = ui::framework::testing::perf::sidebar_sweep::<file_explorer::FileExplorerShell>(4)
        .expect("file-explorer opens");
    sweep.print();
    sweep.assert_each_hover_repaints_only_its_rows();
}

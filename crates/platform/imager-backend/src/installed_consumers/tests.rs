//! Mutation tests of the installed consumer contracts. These run the
//! production code against tiny fixture namespaces; they are not native
//! source/receiving qualification evidence.
pub(crate) mod account;
mod data;

use super::run;

#[test]
fn installed_modes_reject_path_or_receipt_input() {
    for mode in super::MODES {
        let error = run(mode, ["--root".into(), "/tmp".into()]).unwrap_err();
        assert!(error.contains("accepts"), "{mode}: {error}");
    }
    let error = run("--data-consumer-catalog-facts", ["--check".into()]).unwrap_err();
    assert!(error.contains("accepts no arguments"), "{error}");
}

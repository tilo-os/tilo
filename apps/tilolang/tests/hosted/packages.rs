//! The app packages the tests open: where each lives, and the one build of it this process keeps.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use tilolang_tilo::bundle::Bundle;

use super::{CHAT, NOTES, RICHBODY, SIGNED, TASKS, TODAY};

/// The package in `dir` (from this crate's), built with the packages it uses.
///
/// Built once for the process, as an app is built once when it is installed: a window opens the
/// bundle the install left on the disk and compiles no source, so a test that opens the app again
/// compiles none either, and what it measures is the window's opening rather than the compiler's.
pub(super) fn package(dir: &'static str) -> Arc<Bundle> {
    static BUILT: OnceLock<Mutex<std::collections::HashMap<&'static str, Arc<Bundle>>>> =
        OnceLock::new();
    let built = BUILT.get_or_init(Default::default);
    let mut built = built.lock().expect("the packages' build cache");
    Arc::clone(built.entry(dir).or_insert_with(|| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
        let dirs = tilolang::source::dependency_order(&path, &[]).expect("the packages resolve");
        let compiled =
            tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default())
                .expect("the packages compile");
        let (package, used) = compiled.split_last().expect("the package is built last");
        Arc::new(Bundle::new(
            &package.compiled,
            used.iter().map(|used| &used.compiled),
        ))
    }))
}

/// Where the package of the app `app` is, from this crate's: the apps the tests open are
/// shipped with it, and the rich body is their `tests/` one.
pub(super) fn package_of(app: &str) -> &'static str {
    match app {
        NOTES => "../tl/notes",
        RICHBODY => "tests/richbody",
        SIGNED => "tests/signed",
        TASKS => "../tl/tasks",
        CHAT => "../tl/chat",
        TODAY => "../tl/today",
        other => panic!("no test package is {other}"),
    }
}

//! The app packages the tests open: where each lives, and the one build of it this process keeps.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use tilolang_tilo::bundle::Bundle;

use super::{CHAT, FIELDBOX, HANDMADE, NOTES, OPENBY, RICHBODY, SIGNED, TASKS, TODAY};

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
        let (dirs, platform) = ordered(&path);
        let compiled =
            tilolang::source::compile_dirs(&dirs, &platform, &mut tilolang::OsEntropy::default())
                .expect("the packages compile");
        let (package, used) = compiled.split_last().expect("the package is built last");
        Arc::new(Bundle::new(
            &package.compiled,
            used.iter().map(|used| &used.compiled),
        ))
    }))
}

/// A later build of the package in `dir`, compiled again with `edit` applied to its source
/// against its ids lock: what `tilolang dev` hands a running window when the source is saved.
pub(super) fn later(dir: &'static str, edit: impl Fn(&str) -> String) -> Bundle {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let (dirs, platform) = ordered(&path);
    let (dir, used) = dirs.split_last().expect("the package is built last");
    let mut entropy = tilolang::OsEntropy::default();
    let used = tilolang::source::compile_dirs(used, &platform, &mut entropy).expect("they compile");
    let mut source = tilolang::PackageSource::read_dir(dir).expect("the package reads");
    for file in &mut source.files {
        file.text = edit(&file.text);
    }
    let imports: Vec<_> = platform
        .into_iter()
        .chain(used.iter().map(|used| used.compiled.meta.clone()))
        .collect();
    let ids = tilolang::source::read_ids(dir).expect("the ids lock reads");
    let compiled = tilolang::compile_package_source_with(&source, &ids, &imports, &mut entropy)
        .unwrap_or_else(|diagnostics| panic!("the edit compiles: {diagnostics:?}"));
    Bundle::new(&compiled, used.iter().map(|used| &used.compiled))
}

/// The package directories `path` needs, dependencies first, and the platform's packages its
/// imports name where no directory holds them: an app is built against those as an install
/// builds it.
fn ordered(path: &Path) -> (Vec<std::path::PathBuf>, Vec<tilolang::PackageMeta>) {
    let platform = tilolang_tilo::platform_apps::platform_packages().expect("the platform reads");
    let dirs =
        tilolang::source::dependency_order_with(path, &[], &tilolang::source::apps_of(&platform))
            .expect("the packages resolve");
    (dirs, platform)
}

/// Where the package of the app `app` is, from this crate's: the apps the tests open are
/// shipped with it, and the rich body is their `tests/` one.
pub(super) fn package_of(app: &str) -> &'static str {
    match app {
        NOTES => "../../../apps/tl/notes",
        RICHBODY => "tests/window/richbody",
        SIGNED => "tests/window/signed",
        FIELDBOX => "tests/window/fieldbox",
        OPENBY => "tests/window/openby",
        HANDMADE => "tests/window/handmade",
        TASKS => "../../../apps/tl/tasks",
        CHAT => "../../../apps/tl/chat",
        TODAY => "../../../apps/tl/today",
        "so.tilo.lang.counter" => "../../../apps/tl/counter",
        "so.tilo.lang.board" => "../../../apps/tl/board",
        "so.tilo.mail" => "../../../apps/tl/mail",
        "so.tilo.calendar" => "../../../apps/tl/calendar",
        "so.tilo.lang.views" => "../../../apps/tl/views",
        "so.tilo.lang.grid" => "tests/window/grid",
        "so.tilo.lang.interaction" => "tests/window/interaction",
        "so.tilo.lang.mentions" => "tests/window/mentions",
        "so.tilo.lang.ticking" => "tests/window/ticking",
        "test.letters" => "../tilolang/tests/fixtures/letters",
        "t.totp" => "tests/window/secret",
        other => panic!("no test package is {other}"),
    }
}

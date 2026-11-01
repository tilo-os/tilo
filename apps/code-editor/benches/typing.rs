//! One physical-key edit through the production editor, to its requested scene.
//! iai-callgrind excludes setup, correctness checks and fixture destruction.
//! UI time is pinned so instrumentation overhead cannot fire unrelated idle timers.

#![expect(
    clippy::expect_used,
    reason = "benchmark setup and verification must fail rather than measure an incomplete fixture"
)]
#![expect(
    clippy::exit,
    reason = "iai-callgrind's generated main propagates benchmark child status with process::exit"
)]

use code_editor::{CodeEditorShell, benchmark_buffer_text, settings::CodeSettings};
use iai_callgrind::{library_benchmark, library_benchmark_group};
use std::fmt::Write as _;
use ui::framework::testing::TestApp;

const FIRST_LINE: &str = "// typing fixture";

struct TypingFixture {
    app: Box<TestApp<CodeEditorShell>>,
    file: String,
    original: String,
    expected: String,
    history_len: usize,
    before_scene: vello::Scene,
    input_at: ui::time::Instant,
    pending_work_at_boundary: Option<bool>,
    pending_input_at_boundary: Option<bool>,
    // Keep the input file and private profile alive until after verification.
    _directory: tempfile::TempDir,
    _home: tilo_test_env::TestHome,
}

/// Where in the file the measured key lands: the end of its first line, or the
/// end of the file, where every walk from the start of the text is longest.
#[derive(Clone, Copy)]
enum Caret {
    FirstLine,
    End,
}

fn setup(functions: usize) -> Box<TypingFixture> {
    setup_with(functions, Caret::FirstLine)
}

fn setup_at_end(functions: usize) -> Box<TypingFixture> {
    setup_with(functions, Caret::End)
}

fn setup_with(functions: usize, caret: Caret) -> Box<TypingFixture> {
    let opened_at = ui::time::now();
    let input_at = opened_at
        .checked_add(ui::time::Duration::from_secs(2))
        .expect("input instant");
    ui::time::testing::with_monotonic_time(opened_at, || setup_at(functions, input_at, caret))
}

fn setup_at(functions: usize, input_at: ui::time::Instant, caret: Caret) -> Box<TypingFixture> {
    let home = tilo_test_env::TestHome::new().expect("private benchmark environment");
    ui::i18n::I18nManager::init("en-US");
    let directory = tempfile::tempdir().expect("input directory");
    let path = directory.path().join("typing.rs");
    let mut original = format!("{FIRST_LINE}\n");
    for index in 0..functions {
        writeln!(original, "pub fn step_{index:04}(value: u64) -> u64 {{").expect("format fixture");
        writeln!(original, "    value.wrapping_add({index})").expect("format fixture");
        writeln!(original, "}}\n").expect("format fixture");
    }
    std::fs::write(&path, &original).expect("write source file");
    let mut app = Box::new(
        TestApp::<CodeEditorShell>::launched(&[path.to_str().expect("UTF-8 path")])
            .expect("launch actual editor"),
    );
    app.settings::<CodeSettings>()
        .set(&CodeSettings {
            language_servers: false,
            ..Default::default()
        })
        .expect("deterministic editor settings");
    app.harness().resize((1280, 800)).expect("viewport");
    app.settle();
    app.harness()
        .perform_named("editor_view", &ui::a11y::Action::Focus)
        .expect("focus actual editor");
    if matches!(caret, Caret::End) {
        app.harness().set_modifiers(&["ctrl".to_owned()]);
    }
    app.harness().key("End");
    app.harness().set_modifiers(&[]);
    app.settle();
    // Finish opening/focus transitions before the measured interaction.
    ui::time::testing::with_monotonic_time(input_at, || {
        app.idle();
        let file = app
            .state()
            .files
            .iter()
            .find(|(_, file)| file.name == "typing.rs")
            .expect("opened file")
            .0
            .to_string();
        assert_eq!(
            app.read_document(|doc| benchmark_buffer_text(doc, &file)),
            Some(original.clone()),
        );
        let expected = match caret {
            Caret::FirstLine => format!(
                "{FIRST_LINE}x{}",
                original.strip_prefix(FIRST_LINE).expect("fixture prefix")
            ),
            Caret::End => format!("{original}x"),
        };
        let history_len = app.history().len();
        let before_scene = app.harness().frame_output().scene.clone();
        Box::new(TypingFixture {
            app,
            file,
            original,
            expected,
            history_len,
            before_scene,
            input_at,
            pending_work_at_boundary: None,
            pending_input_at_boundary: None,
            _directory: directory,
            _home: home,
        })
    })
}

fn verify(fixture: Box<TypingFixture>) {
    ui::time::testing::with_monotonic_time(fixture.input_at, || verify_at(fixture));
}

fn verify_at(fixture: Box<TypingFixture>) {
    // The settlement case still requires every queued host update to drain.
    assert_eq!(fixture.pending_work_at_boundary, Some(false));
    verify_state(fixture);
}

fn verify_key(fixture: Box<TypingFixture>) {
    ui::time::testing::with_monotonic_time(fixture.input_at, || {
        // Future durability/presence acknowledgements are not the key's scene.
        // The real input frame itself must be complete at the measured boundary.
        assert_eq!(fixture.pending_input_at_boundary, Some(false));
        verify_state(fixture);
    });
}

fn verify_state(mut fixture: Box<TypingFixture>) {
    assert_eq!(
        fixture
            .app
            .read_document(|doc| benchmark_buffer_text(doc, &fixture.file)),
        Some(fixture.expected.clone()),
        "the measured key must insert exactly one character at the caret",
    );
    assert_eq!(
        fixture.app.history().len(),
        fixture.history_len.checked_add(1).expect("history length")
    );
    let frame = fixture.app.harness().frame_output();
    assert!(
        !fixture.before_scene.encoding_eq(frame.scene),
        "the edit must reach scene encoding, not just change the document",
    );
    assert!(fixture.app.undo(), "the edit must remain undoable");
    assert_eq!(
        fixture
            .app
            .read_document(|doc| benchmark_buffer_text(doc, &fixture.file)),
        Some(fixture.original.clone()),
    );
    assert!(fixture.app.redo(), "the edit must remain redoable");
    assert_eq!(
        fixture
            .app
            .read_document(|doc| benchmark_buffer_text(doc, &fixture.file)),
        Some(fixture.expected.clone()),
    );
}

#[library_benchmark(setup = setup, teardown = verify)]
#[bench::small(10)]
#[bench::large(900)]
#[bench::large_at_end(args = (900), setup = setup_at_end)]
fn single_character_to_scene(mut fixture: Box<TypingFixture>) -> Box<TypingFixture> {
    ui::time::testing::with_monotonic_time(fixture.input_at, || {
        fixture.app.harness().key("x");
        // Match the live event loop without forcing frames or advancing idle timers.
        fixture.app.idle();
        // Drain queued work, not conservative dirtiness of otherwise idle leaves.
        // Failure to drain remains a verification failure, not a timing result.
        for _ in 0..32 {
            if !fixture.app.harness().frame_schedule().queued_work {
                break;
            }
            assert!(fixture.app.frame(), "a queued frame must complete");
        }
        fixture.pending_work_at_boundary = Some(fixture.app.harness().frame_schedule().queued_work);
    });
    fixture
}

#[library_benchmark(setup = setup, teardown = verify_key)]
#[bench::small(10)]
#[bench::large(900)]
#[bench::large_at_end(args = (900), setup = setup_at_end)]
fn single_key_to_scene(mut fixture: Box<TypingFixture>) -> Box<TypingFixture> {
    ui::time::testing::with_monotonic_time(fixture.input_at, || {
        fixture.app.harness().key("x");
        fixture.pending_input_at_boundary = Some(
            fixture
                .app
                .harness()
                .queued_work_sources()
                .contains(&"input frame pending"),
        );
    });
    fixture
}

library_benchmark_group!(name = typing; benchmarks = single_character_to_scene, single_key_to_scene);

/// The entry point iai-callgrind expects: it either runs the benchmarks
/// through its runner, or is re-entered by that runner to measure one of them.
mod entry {
    use iai_callgrind::{Callgrind, EntryPoint, LibraryBenchmarkConfig, main};

    use super::typing;

    main!(
        config = LibraryBenchmarkConfig::default()
            .pass_through_envs(["VALGRIND_LIB", "TMPDIR", "HOME", "XDG_RUNTIME_DIR"])
            .tool(Callgrind::with_args([
                "cache-sim=yes",
                "I1=32768,8,64",
                "D1=32768,8,64",
                "LL=8388608,16,64",
            ])
            // The default trailing wildcard also matches non-inlined closures:
            // entering one would toggle collection OFF around the actual edit.
            .entry_point(EntryPoint::Custom(
                "*::__iai_callgrind_wrapper_mod::*_to_scene".into(),
            )));
        library_benchmark_groups = typing
    );

    pub(super) fn run() {
        main();
    }
}

fn main() {
    // A test runner lists and runs every target by executing it. Only a
    // benchmark run, or the callgrind runner re-entering this binary, measures.
    if !std::env::args()
        .skip(1)
        .any(|arg| arg == "--bench" || arg == "--iai-run")
    {
        return;
    }
    entry::run();
}

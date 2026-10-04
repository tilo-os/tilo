#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! Launching a framework app on something: a path on its command line opens
//! that path's own record and takes the app's launch actions there through
//! the door every control uses; a file a host hands over reaches the app
//! only if it opens that type.

use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Sketch {
    pub source: String,
    pub notes: Collection<Note>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub text: String,
}

#[resource(kind = "test.sketch", label = "Sketch")]
impl Sketch {
    /// Trace over an image.
    #[action]
    pub fn trace(&mut self, source: String) {
        self.source = source;
    }

    /// Add a note.
    #[action]
    pub fn note(&mut self, cx: &mut ActionCx, text: String) {
        self.notes.push(cx.id(), Note { text });
    }
}

struct SketchApp;

impl ResourceApp for SketchApp {
    type Resource = Sketch;
    const ID: &'static str = "so.tilo.test-sketch";
    const TITLE: &'static str = "Sketch";
    const OPENS: &'static [&'static str] = &["text/plain"];
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Sketch>) -> Element {
        // The view shows what the launch named, as an app opens what its
        // launch actions made.
        let launch = cx.launch();
        let named = launch
            .file()
            .map(|file| file.name().to_owned())
            .or_else(|| launch.argument().map(str::to_owned))
            .unwrap_or_default();
        Box::new(
            Column::new()
                .push(Box::new(cx.text(|sketch| sketch.source.clone())))
                .push(Box::new(Text::dynamic(move || {
                    format!("Launched on {named}")
                }))),
        )
    }

    /// One sketch per traced image.
    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        input
            .argument()
            .map(|path| RecordId::derived("sketch", &RecordId::for_file(path)))
    }

    fn launch(input: &LaunchInput<'_>, created: bool) -> Vec<Invoke<Sketch>> {
        if let Some(file) = input.file() {
            let text = String::from_utf8_lossy(file.bytes()).into_owned();
            return vec![Sketch::NOTE.with(text)];
        }
        match input.argument() {
            Some(path) if created => vec![Sketch::TRACE.with(path.to_owned())],
            _ => Vec::new(),
        }
    }
}

#[test]
fn a_path_argument_opens_the_record_of_that_path() {
    let mut app = TestApp::<SketchApp>::launched(&["/pictures/harbour.png"]).unwrap();
    app.harness().relayout();
    assert_eq!(
        app.record(),
        &RecordId::derived("sketch", &RecordId::for_file("/pictures/harbour.png"))
    );
    assert_eq!(app.state().source, "/pictures/harbour.png");
    let frame = app.harness().inspection_json().unwrap();
    assert!(
        frame.contains("Launched on /pictures/harbour.png"),
        "{frame}"
    );

    let mut plain = TestApp::<SketchApp>::new().unwrap();
    assert_eq!(plain.record(), &RecordId::room_default());
    assert_eq!(plain.state().source, "");
    let frame = plain.harness().inspection_json().unwrap();
    assert!(
        frame.contains("Launched on") && !frame.contains("Launched on /"),
        "{frame}"
    );
}

#[test]
fn launch_actions_are_the_participants_own_undoable_edits() {
    let mut app = TestApp::<SketchApp>::launched(&["/pictures/harbour.png"]).unwrap();
    app.harness().relayout();
    assert_eq!(app.state().source, "/pictures/harbour.png");
    let versions = app.versions();
    let traced = versions
        .last()
        .and_then(|version| version.context.as_ref())
        .map(|context| context.action_label.as_str());
    assert_eq!(traced, Some("Trace over an image"), "{versions:?}");
    assert!(app.undo());
    assert_eq!(app.state().source, "");
}

#[test]
fn launching_on_a_record_again_keeps_its_edits_and_does_not_refill_it() {
    let mut app = TestApp::<SketchApp>::launched(&["/pictures/harbour.png"]).unwrap();
    app.harness().relayout();
    app.invoke(Sketch::NOTE.with("the lighthouse".into()))
        .unwrap();
    let first = app.record().clone();
    let versions = app.versions().len();

    let mut again = app.relaunch(&["/pictures/harbour.png"]).unwrap();
    again.harness().relayout();
    assert_eq!(again.record(), &first);
    assert_eq!(again.state().source, "/pictures/harbour.png");
    assert_eq!(again.state().notes.len(), 1);
    assert_eq!(again.versions().len(), versions, "nothing re-applied");

    let mut other = again.relaunch(&["/pictures/meadow.png"]).unwrap();
    other.harness().relayout();
    assert_ne!(other.record(), &first);
    assert_eq!(other.state().source, "/pictures/meadow.png");
    assert!(other.state().notes.is_empty());
}

#[test]
fn a_handed_file_reaches_the_app_and_learns_the_resource_that_holds_it() {
    let (file, receipt) =
        OpenFile::new("lighthouse.txt", "text/plain", b"keeper".to_vec()).unwrap();
    let mut app = TestApp::<SketchApp>::handed(file).unwrap();
    app.harness().relayout();
    let notes: Vec<String> = app
        .state()
        .notes
        .values()
        .map(|note| note.text.clone())
        .collect();
    assert_eq!(notes, ["keeper"]);
    assert_eq!(receipt.opened_resource().as_deref(), Some("test.sketch"));
    let frame = app.harness().inspection_json().unwrap();
    assert!(frame.contains("Launched on lighthouse.txt"), "{frame}");
}

#[test]
fn a_file_of_a_type_the_app_does_not_open_is_refused() {
    let (file, receipt) = OpenFile::new("chart.pdf", "application/pdf", b"%PDF".to_vec()).unwrap();
    assert!(TestApp::<SketchApp>::handed(file).is_err());
    assert_eq!(receipt.opened_resource(), None);
}

#[test]
fn the_definition_names_what_the_app_opens() {
    let definition = tilo_app::framework::definition::<SketchApp>();
    assert!(definition.opens_media_type("text/plain; charset=utf-8"));
    assert!(!definition.opens_media_type("application/pdf"));
}

#[test]
fn a_launch_naming_a_file_the_app_opens_hands_it_that_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lighthouse.txt");
    std::fs::write(&path, "keeper").unwrap();
    let path = path.to_str().unwrap();
    let mut app = TestApp::<SketchApp>::launched(&[path]).unwrap();
    app.harness().relayout();
    assert_eq!(
        app.record(),
        &RecordId::derived("sketch", &RecordId::for_file(path))
    );
    let notes: Vec<String> = app
        .state()
        .notes
        .values()
        .map(|note| note.text.clone())
        .collect();
    assert_eq!(notes, ["keeper"]);
}

/// An app that opens every file, and shows where it was launched.
struct InspectorApp;

impl ResourceApp for InspectorApp {
    type Resource = Sketch;
    const ID: &'static str = "so.tilo.test-inspector";
    const TITLE: &'static str = "Inspector";
    const OPENS: &'static [&'static str] = &[ANY_MEDIA_TYPE];
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Sketch>) -> Element {
        let launched = cx.launch_arguments().join(" ");
        Box::new(Text::new(&format!("Launched on {launched}")))
    }

    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Sketch>> {
        input
            .file()
            .map(|file| Sketch::NOTE.with(format!("{} {}", file.name(), file.media_type())))
            .into_iter()
            .collect()
    }
}

#[test]
fn an_app_that_opens_every_file_is_handed_one_of_no_known_type_and_its_view_sees_the_path() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("firmware");
    std::fs::write(&path, [0xde, 0xad]).unwrap();
    let path = path.to_str().unwrap();
    let definition = tilo_app::framework::definition::<InspectorApp>();
    assert!(definition.opens_media_type("application/pdf"));
    assert!(definition.opens_media_type("application/octet-stream"));

    let mut app = TestApp::<InspectorApp>::launched(&[path]).unwrap();
    app.harness().relayout();
    let notes: Vec<String> = app
        .state()
        .notes
        .values()
        .map(|note| note.text.clone())
        .collect();
    assert_eq!(notes, ["firmware application/octet-stream"]);
    let expected = format!("Launched on {path}");
    assert!(
        app.harness().inspection_json().unwrap().contains(&expected),
        "the view reads the launch's arguments"
    );

    // A folder is no file to hand over.
    let mut plain =
        TestApp::<InspectorApp>::launched(&[directory.path().to_str().unwrap()]).unwrap();
    plain.harness().relayout();
    assert!(plain.state().notes.is_empty());
}

thread_local! {
    static VIEW_CONTENT: std::cell::RefCell<Option<tilo_content::ContentStore>> =
        const { std::cell::RefCell::new(None) };
}

/// Keeps a handed picture's bytes in the launch's content store and traces
/// over them by hash.
struct KeepApp;

impl ResourceApp for KeepApp {
    type Resource = Sketch;
    const ID: &'static str = "so.tilo.test-keep";
    const TITLE: &'static str = "Keep";
    const OPENS: &'static [&'static str] = &["image/png"];

    fn view(cx: &mut ViewCx<Sketch>) -> Element {
        VIEW_CONTENT.with(|content| *content.borrow_mut() = cx.content());
        Box::new(cx.text(|sketch| sketch.source.clone()))
    }

    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Sketch>> {
        let (Some(file), Some(content)) = (input.file(), input.content()) else {
            return Vec::new();
        };
        content
            .put(file.bytes())
            .map(|hash| Sketch::TRACE.with(hash))
            .into_iter()
            .collect()
    }
}

#[test]
fn a_handed_file_is_kept_in_the_content_store_the_view_reads() {
    let bytes = b"\x89PNG harbour".to_vec();
    let (file, _receipt) = OpenFile::new("harbour.png", "image/png", bytes.clone()).unwrap();
    let mut app = TestApp::<KeepApp>::handed(file).unwrap();
    app.harness().relayout();
    let hash = app.state().source;
    assert_eq!(
        hash,
        tilo_content::hash_bytes(&bytes),
        "the record names the bytes"
    );
    let content = VIEW_CONTENT
        .with(|content| content.borrow_mut().take())
        .expect("the view reads the content store the launch kept the bytes in");
    assert_eq!(content.read(&hash), Some(bytes));
}

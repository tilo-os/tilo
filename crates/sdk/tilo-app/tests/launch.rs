//! Launching a framework app on something: a path on its command line opens
//! that path's own record and takes the app's launch actions there through
//! the door every control uses; a file a host hands over reaches the app
//! only if it opens that type.

use tilo_app::prelude::*;

struct PrivateSketch;

impl ResourceApp for PrivateSketch {
    type Resource = Sketch;
    const ID: &'static str = "so.tilo.test-private-sketch";
    const TITLE: &'static str = "Private sketch";
    const PRIVATE_WINDOWS: bool = true;
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Sketch>) -> Element {
        Box::new(
            Text::new(&format!(
                "Private {}, arguments {}",
                cx.is_private(),
                cx.launch_arguments().join(" ")
            ))
            .with_id("private-launch"),
        )
    }
}

#[test]
fn private_headless_launch_uses_a_disposable_profile_and_consumes_its_flag() {
    let directory = tempfile::tempdir().unwrap();
    let mut shared = TestApp::<PrivateSketch>::open_data_dir(directory.path()).unwrap();
    shared
        .invoke(Sketch::TRACE.with("Personal sketch"))
        .unwrap();
    let mut private = shared.relaunch(&["--private", "private.png"]).unwrap();
    assert_eq!(private.state(), Sketch::default());
    private
        .harness()
        .expect(
            &tilo_app::headless::SemanticExpectation::named("private-launch")
                .name_contains("Private true, arguments private.png"),
        )
        .unwrap();
    private
        .invoke(Sketch::TRACE.with("Disposable sketch"))
        .unwrap();
    let shared = TestApp::<PrivateSketch>::open_data_dir(directory.path()).unwrap();
    assert_eq!(shared.state().source, "Personal sketch");
    drop(private);
    drop(shared);
    let shared = TestApp::<PrivateSketch>::open_data_dir(directory.path()).unwrap();
    assert_eq!(shared.state().source, "Personal sketch");
}

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
        .and_then(|context| match &context.what {
            Some(tilo_actions::Did::Verb(verb)) => Some(verb.action.action.as_str()),
            _ => None,
        });
    assert_eq!(traced, Some("trace"), "{versions:?}");
    assert!(app.undo());
    assert_eq!(app.state().source, "");
}

#[test]
fn launching_on_a_record_again_keeps_its_edits_and_does_not_refill_it() {
    let mut app = TestApp::<SketchApp>::launched(&["/pictures/harbour.png"]).unwrap();
    app.harness().relayout();
    app.invoke(Sketch::NOTE.with("the lighthouse")).unwrap();
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

/// An app with options of its own: how its window runs, beside what it
/// opens.
struct TunedApp;

impl ResourceApp for TunedApp {
    type Resource = Sketch;
    const ID: &'static str = "so.tilo.test-tuned";
    const TITLE: &'static str = "Tuned";
    const ARGUMENTS: usize = 1;
    const OPTIONS: &'static [&'static str] = &["--pen"];
    const FLAGS: &'static [&'static str] = &["--grid"];

    fn view(cx: &mut ViewCx<Sketch>) -> Element {
        let pen = cx.launch_option("--pen").unwrap_or("none").to_owned();
        let grid = cx.launch_flag("--grid");
        let launched = cx.launch_arguments().join(" ");
        Box::new(Text::new(&format!("Pen {pen}, grid {grid}, on {launched}")))
    }

    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Sketch>> {
        input
            .option("--pen")
            .map(|pen| Sketch::TRACE.with(format!("{pen} {}", input.flag("--grid"))))
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

#[test]
fn the_apps_own_options_reach_its_launch_and_its_view_apart_from_its_arguments() {
    let mut app = TestApp::<TunedApp>::launched(&["--pen=ink", "--grid", "harbour"]).unwrap();
    app.harness().relayout();
    assert_eq!(app.state().source, "ink true");
    assert!(
        app.harness()
            .inspection_json()
            .unwrap()
            .contains("Pen ink, grid true, on harbour"),
        "the view reads the options and the one argument"
    );

    let mut plain = TestApp::<TunedApp>::launched(&["--pen", "chalk"]).unwrap();
    plain.harness().relayout();
    assert_eq!(plain.state().source, "chalk false");
    assert!(
        plain
            .harness()
            .inspection_json()
            .unwrap()
            .contains("Pen chalk, grid false, on ")
    );
}

/// A player that shows what its launch asked it to play, without keeping any
/// of it in its record.
struct PlayerApp;

impl ResourceApp for PlayerApp {
    type Resource = Sketch;
    const ID: &'static str = "so.tilo.test-player";
    const TITLE: &'static str = "Player";
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Sketch>) -> Element {
        let launch = cx.launch();
        let shown = match (launch.argument(), launch.handoff()) {
            (Some(media), Some(handoff)) => format!(
                "{media} ({}) from {} ms, {}",
                handoff.title.as_deref().unwrap_or("untitled"),
                handoff.position_ms,
                if handoff.resume_playing {
                    "playing"
                } else {
                    "paused"
                }
            ),
            (Some(media), None) => format!("{media} from the start"),
            (None, _) => "nothing to play".to_owned(),
        };
        Box::new(Text::new(&shown))
    }
}

#[test]
fn the_view_reads_the_media_a_launch_names_and_the_playback_it_continues() {
    let shown = |app: &mut TestApp<PlayerApp>| app.harness().inspection_json().unwrap();

    let mut handed = TestApp::<PlayerApp>::handed_off(
        &["https://media.example/evening.mp4"],
        PlaybackHandoff {
            position_ms: 42_000,
            resume_playing: true,
            title: Some("Evening news".into()),
        },
    )
    .unwrap();
    assert!(
        shown(&mut handed)
            .contains("https://media.example/evening.mp4 (Evening news) from 42000 ms, playing")
    );
    assert_eq!(handed.state(), Sketch::default(), "the record is untouched");

    let mut launched =
        TestApp::<PlayerApp>::launched(&["https://media.example/evening.mp4"]).unwrap();
    assert!(shown(&mut launched).contains("https://media.example/evening.mp4 from the start"));

    let mut plain = TestApp::<PlayerApp>::new().unwrap();
    assert!(shown(&mut plain).contains("nothing to play"));
}

#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "test.gallery.effects")]
pub enum GalleryEffect {
    /// Show a picture.
    #[effect(kind = "read")]
    Show { picture: String },
}

thread_local! {
    /// The pictures shown, in order.
    static SHOWN: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The gallery's dispatcher, as its view was built.
    static GALLERY: std::cell::RefCell<Option<Dispatcher<Sketch>>> =
        const { std::cell::RefCell::new(None) };
}

/// Shows the picture a launch names, an effect rather than an edit.
struct GalleryApp;

impl ResourceApp for GalleryApp {
    type Resource = Sketch;
    const ID: &'static str = "so.tilo.test-gallery";
    const TITLE: &'static str = "Gallery";
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Sketch>) -> Element {
        cx.effects(|effect: GalleryEffect, _| {
            let GalleryEffect::Show { picture } = effect;
            SHOWN.with(|shown| shown.borrow_mut().push(picture));
            Ok(tilo_app::framework::Effected::done())
        });
        GALLERY.with(|gallery| *gallery.borrow_mut() = Some(cx.dispatcher()));
        Box::new(cx.text(|sketch| sketch.source.clone()))
    }

    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Sketch>> {
        input
            .argument()
            .map(|picture| {
                Invoke::effect(&GalleryEffect::Show {
                    picture: picture.to_owned(),
                })
            })
            .into_iter()
            .collect()
    }
}

#[test]
fn a_launch_invokes_an_effect_which_changes_no_document_and_a_viewer_reaches() {
    let mut app = TestApp::<GalleryApp>::launched(&["harbour.png"]).unwrap();
    app.harness().relayout();
    assert_eq!(SHOWN.with(|shown| shown.borrow().clone()), ["harbour.png"]);
    assert_eq!(app.state().source, "", "the launch edited nothing");
    assert!(!app.undo(), "nothing to take back");

    // A viewer may not edit, but an effect declared for readers reaches its
    // handler through the same door.
    let mut viewer = TestApp::<GalleryApp>::with_role(tilo_app::Role::Viewer).unwrap();
    let dispatcher = GALLERY
        .with(|gallery| gallery.borrow_mut().take())
        .expect("the viewer's view was built");
    dispatcher.submit(Invoke::effect(&GalleryEffect::Show {
        picture: "meadow.png".into(),
    }));
    viewer.settle();
    assert_eq!(
        SHOWN.with(|shown| shown.borrow().clone()),
        ["harbour.png", "meadow.png"]
    );
    assert!(dispatcher.refused().get().is_none(), "nothing was refused");
}

#[test]
fn a_window_opens_another_record_in_its_own_place() {
    let mut app = TestApp::<SketchApp>::new().unwrap();
    assert_eq!(app.record(), &RecordId::room_default());
    app.invoke(Sketch::NOTE.with("on the default")).unwrap();

    app.open_here(&["/pictures/harbour.png"]).unwrap();
    let harbour = RecordId::derived("sketch", &RecordId::for_file("/pictures/harbour.png"));
    assert_eq!(
        app.record(),
        &harbour,
        "the window shows the path's own record"
    );
    assert_eq!(
        app.state().source,
        "/pictures/harbour.png",
        "with the actions a launch on it takes"
    );
    assert!(
        app.state().notes.is_empty(),
        "and none of the other record's"
    );
    let frame = app.harness().inspection_json().unwrap();
    assert!(
        frame.contains("Launched on /pictures/harbour.png"),
        "the view is built for the launch: {frame}"
    );

    // A record that cannot be opened is refused with the reason, and the
    // window keeps the one it shows.
    let refused = app.open_here(&["/no/such/folder/notes.txt"]).unwrap_err();
    assert!(refused.contains("cannot be read"), "{refused}");
    assert_eq!(app.record(), &harbour);

    // Edits go to the record shown now, through the same door.
    app.invoke(Sketch::NOTE.with("on the harbour")).unwrap();
    assert_eq!(app.state().notes.len(), 1);

    // Each record kept its own edits, durably.
    let harbour_again = app.relaunch(&["/pictures/harbour.png"]).unwrap();
    assert_eq!(harbour_again.record(), &harbour);
    assert_eq!(harbour_again.state().notes.len(), 1);
    // A bare launch reopens the record the window showed last.
    let bare = harbour_again.relaunch(&[]).unwrap();
    assert_eq!(bare.record(), &harbour);
    assert_eq!(bare.state().notes.len(), 1);
}

/// A sketch whose record of a path starts traced over that path in its
/// genesis, and whose launch reads the record it opens.
struct GenesisApp;

impl ResourceApp for GenesisApp {
    type Resource = Sketch;
    const ID: &'static str = "so.tilo.test-genesis-sketch";
    const TITLE: &'static str = "Sketch";
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Sketch>) -> Element {
        Box::new(cx.text(|sketch| sketch.source.clone()))
    }

    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        input.argument().map(RecordId::for_file)
    }

    fn genesis(input: &LaunchInput<'_>) -> Vec<Invoke<Sketch>> {
        input
            .argument()
            .map(|path| Sketch::TRACE.with(path.to_owned()))
            .into_iter()
            .collect()
    }

    /// Reopened, a note says how many notes the record already held.
    fn launch(input: &LaunchInput<'_>, created: bool) -> Vec<Invoke<Sketch>> {
        let held = input.record::<Sketch>().map(|sketch| sketch.notes.len());
        match held {
            Some(held) if !created => vec![Sketch::NOTE.with(format!("reopened over {held}"))],
            _ => Vec::new(),
        }
    }
}

#[test]
fn a_launch_genesis_is_the_same_on_every_device_and_nobodys_edit() {
    let mut here = TestApp::<GenesisApp>::launched(&["/pictures/harbour.png"]).unwrap();
    let there = TestApp::<GenesisApp>::launched(&["/pictures/harbour.png"]).unwrap();
    assert_eq!(here.state().source, "/pictures/harbour.png");
    assert!(!here.undo(), "the genesis is no undo step");
    assert_eq!(
        here.read_document(|doc| doc.clone().get_heads()),
        there.read_document(|doc| doc.clone().get_heads()),
        "both devices wrote the same genesis"
    );
    let mut theirs = there.peer_document();
    here.merge_peer(&mut theirs).unwrap();
    assert_eq!(here.state().source, "/pictures/harbour.png");
}

#[test]
fn a_launch_reads_the_record_it_opens() {
    let mut app = TestApp::<GenesisApp>::launched(&["/pictures/harbour.png"]).unwrap();
    assert!(
        app.state().notes.is_empty(),
        "a created record is not reopened"
    );
    app.invoke(Sketch::NOTE.with("the lighthouse")).unwrap();
    let app = app.relaunch(&["/pictures/harbour.png"]).unwrap();
    let mut texts: Vec<String> = app
        .state()
        .notes
        .values()
        .map(|note| note.text.clone())
        .collect();
    texts.sort();
    assert_eq!(texts, ["reopened over 1", "the lighthouse"]);
}

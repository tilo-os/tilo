#![allow(clippy::unwrap_used, reason = "test assertions")]

//! Screenshot scenarios on the framework: actions stage the resource, views
//! and surfaces stage their own view state, and the capture shows both.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_app::advanced::{Affine, NodeId, Scene, TaffyTree, WindowEvent};
use tilo_app::headless::FrameHarness;
use tilo_app::prelude::*;
use tilo_app::winit::event::ElementState;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Playlist {
    pub songs: Collection<Song>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Song {
    pub title: String,
}

#[resource(kind = "test.playlist", label = "Playlist")]
impl Playlist {
    /// Add a song.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, title: String) {
        self.songs.push(cx.id(), Song { title });
    }
}

struct PlaylistApp;

/// What a capture shows in place of a live chart service.
struct TopChart(String);

/// The liner notes a scenario's fixture wrote to its scratch directory.
struct LinerNotes(std::path::PathBuf);

/// The cover art a scenario's fixture put in the content store.
struct Cover(String);

const MILES_AT_MS: i64 = 1_600_000_000_000;
const BILL_AT_MS: i64 = 1_600_000_600_000;

impl ResourceApp for PlaylistApp {
    type Resource = Playlist;
    const ID: &'static str = "so.tilo.test-playlist";
    const TITLE: &'static str = "Playlist";

    fn view(cx: &mut ViewCx<Playlist>) -> Element {
        // View-local state no action touches: which song is selected, and
        // whether the details dialog is open.
        let selected = cx.local(String::new());
        let dialog = cx.local(false);
        let (stage_selected, stage_dialog) = (selected.clone(), dialog.clone());
        let live = cx.live().clone();
        cx.on_stage(move |scenario| {
            if scenario == Some("details") {
                let first = live.with(|list| list.songs.values().next().map(|s| s.title.clone()));
                stage_selected.set(first.unwrap_or_default());
                stage_dialog.set(true);
            }
        });
        let chart = cx
            .fixture::<TopChart>()
            .map_or_else(|| "no chart".to_owned(), |chart| chart.0.clone());
        let notes = cx.fixture::<LinerNotes>().map_or_else(
            || "no notes".to_owned(),
            |notes| std::fs::read_to_string(&notes.0).unwrap_or_default(),
        );
        Box::new(
            Column::new()
                .push(Box::new(text_of(format!("Built for {:?}", cx.mode()))))
                .push(Box::new(text_of(format!("Chart: {chart}"))))
                .push(Box::new(text_of(format!("Notes: {notes}"))))
                .push(Box::new(
                    cx.text(|list| format!("{} songs", list.songs.len())),
                ))
                .push(Box::new(
                    Text::from_state(&selected, |title| format!("Selected: {title}"))
                        .with_id("selection"),
                ))
                .push(Box::new(
                    Text::from_state(&dialog, |open| {
                        if *open {
                            "Details open"
                        } else {
                            "Details closed"
                        }
                        .to_owned()
                    })
                    .with_id("dialog"),
                ))
                .push(Box::new(Text::from_state(&cx.save_status(), |status| {
                    format!("Record {status:?}")
                })))
                .push(cx.surface(Staged::default())),
        )
    }

    fn scenarios() -> &'static [&'static str] {
        &["details", "narrow", "clicked"]
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some("narrow") => (360, 720),
            _ => (1024, 720),
        }
    }

    fn fixture(fixture: &mut Fixture<Playlist>) {
        if fixture.scenario() != Some("details") {
            return;
        }
        fixture.provide(TopChart("Kind of Blue".into()));
        let notes = fixture.directory().unwrap().join("liner-notes.txt");
        std::fs::write(&notes, "Recorded in two sessions").unwrap();
        fixture.provide(LinerNotes(notes));
        let cover = fixture.content().put(b"cover art").unwrap();
        fixture.provide(Cover(cover));
        // Two people built the playlist before the capture.
        fixture.version(
            "miles",
            MILES_AT_MS,
            Playlist::ADD.with("Freddie Freeloader"),
        );
        fixture.version("bill", BILL_AT_MS, Playlist::ADD.with("Flamenco Sketches"));
        fixture.collaborator("miles", ResourceSelection::Item("second-take".into()));
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Playlist>> {
        match name {
            Some("details") => vec![
                Playlist::ADD.with("Blue in Green"),
                Playlist::ADD.with("So What"),
            ],
            _ => Vec::new(),
        }
    }

    fn scenario_input(name: Option<&str>, input: &mut FrameHarness) -> Result<(), String> {
        if name != Some("clicked") {
            return Ok(());
        }
        let surface = *input
            .targets()
            .get("surface")
            .ok_or("the surface is not laid out")?;
        input.click_at(surface.center().x, surface.center().y);
        Ok(())
    }
}

thread_local! {
    static STAGED: RefCell<Vec<Option<String>>> = const { RefCell::new(Vec::new()) };
}

/// A custom surface that records the scenarios it stages, what it had been
/// presented and laid out at by then, and what it was built for; its cover
/// "decodes" over a few frames, which a capture waits for.
struct Staged {
    inner: Text,
    label: LocalState<String>,
    songs: usize,
    mode: Option<RunMode>,
    cover: String,
    decoding: u32,
}

impl Default for Staged {
    fn default() -> Self {
        let label = LocalState::new("Surface not staged".to_owned());
        Self {
            inner: Text::from_state(&label, Clone::clone).with_id("surface"),
            label,
            songs: 0,
            mode: None,
            cover: String::new(),
            decoding: 0,
        }
    }
}

impl Widget for Staged {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner.build_layout(taffy, theme, nodes)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.inner.apply_layout(taffy, node, origin);
    }

    fn sync(&mut self, state: &tilo_app::widget::AppModel, path: &tilo_app::widget::ModelPath) {
        if self.decoding > 0 {
            self.decoding = self.decoding.saturating_sub(1);
            if self.decoding == 0 {
                self.label
                    .set(format!("{} · cover {}", self.label.get(), self.cover));
            }
        }
        self.inner.sync(state, path);
    }

    fn has_pending_visual_work(&self) -> bool {
        self.decoding > 0
    }

    fn handle_event(&mut self, event: &WindowEvent, _ctx: &mut tilo_app::widget::EventCtx<'_>) {
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            ..
        } = event
        {
            self.label.set(format!("{} · clicked", self.label.get()));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.inner.draw(scene, transform, text, theme, presence);
    }

    fn name(&self) -> &'static str {
        "Staged"
    }

    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.inner]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.inner]
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.inner.take_structure_dirty()
    }
}

impl ResourceSurface<Playlist> for Staged {
    fn present(&mut self, state: &Rc<Playlist>) {
        self.songs = state.songs.len();
    }

    fn attach(&mut self, cx: SurfaceCx<Playlist>) {
        self.mode = Some(cx.mode());
        if let (Some(cover), Some(content)) = (cx.fixture::<Cover>(), cx.content()) {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut content.open(&cover.0).unwrap(), &mut bytes).unwrap();
            self.cover = String::from_utf8(bytes).unwrap();
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        STAGED.with(|staged| staged.borrow_mut().push(scenario.map(str::to_owned)));
        self.label.set(format!(
            "Surface staged with {} songs in {:?} at {} px",
            self.songs,
            self.mode,
            self.get_rect().width()
        ));
        if !self.cover.is_empty() {
            self.decoding = 3;
        }
    }
}

/// Text fixed when the view is built.
fn text_of(text: String) -> Text {
    Text::dynamic(move || text.clone())
}

fn shown(app: &mut TestApp<PlaylistApp>) -> String {
    app.harness().inspection_json().unwrap()
}

#[test]
fn scenarios_stage_the_resource_then_the_views_deterministically() {
    // phase: a_scenario_stages_the_resource_and_then_the_views
    {
        // Each case starts from an empty staging record, as its own test thread did.
        STAGED.with(|staged| staged.borrow_mut().clear());
        let mut app = TestApp::<PlaylistApp>::capture(Some("details")).unwrap();
        let frame = shown(&mut app);
        assert!(frame.contains("4 songs"), "{frame}");
        assert!(frame.contains("Selected: Freddie Freeloader"), "{frame}");
        assert!(frame.contains("Details open"), "{frame}");
        // The surface staged after it was presented the scenario's actions.
        assert!(frame.contains("Surface staged with 4 songs"), "{frame}");
        assert_eq!(
            STAGED.with(|staged| staged.borrow().clone()),
            [Some("details".to_owned())]
        );
        // View state is not resource state: only the actions were written.
        assert_eq!(app.state().songs.len(), 4);
        assert!(frame.contains("Record Saved"), "{frame}");
    }
    // phase: a_plain_capture_stages_with_no_scenario
    {
        // Each case starts from an empty staging record, as its own test thread did.
        STAGED.with(|staged| staged.borrow_mut().clear());
        let mut app = TestApp::<PlaylistApp>::capture(None).unwrap();
        assert!(shown(&mut app).contains("0 songs"));
        assert!(shown(&mut app).contains("Details closed"));
        assert_eq!(STAGED.with(|staged| staged.borrow().clone()), [None]);
    }
    // phase: a_scenario_composes_the_same_scene_every_time
    {
        // Each case starts from an empty staging record, as its own test thread did.
        STAGED.with(|staged| staged.borrow_mut().clear());
        let print = || {
            let mut app = TestApp::<PlaylistApp>::capture(Some("details")).unwrap();
            let encoding = app.harness().scene().encoding().clone();
            (encoding.path_data, encoding.draw_data, encoding.n_paths)
        };
        assert_eq!(print(), print());
    }
}

#[test]
fn a_scenarios_input_meets_the_staged_scene() {
    let mut app = TestApp::<PlaylistApp>::capture(Some("clicked")).unwrap();
    let frame = shown(&mut app);
    // The click reached the surface through the window's event path, after
    // the surface had staged.
    assert!(
        frame.contains("Surface staged with 0 songs in Some(Capture)"),
        "{frame}"
    );
    assert!(frame.contains("· clicked"), "{frame}");
    // A scenario without input is left as staged.
    let mut plain = TestApp::<PlaylistApp>::capture(Some("narrow")).unwrap();
    assert!(!shown(&mut plain).contains("clicked"));
}

#[test]
fn views_know_what_they_are_built_for() {
    // A window someone uses reads live sources: no fixture reaches it.
    let mut live = TestApp::<PlaylistApp>::new().unwrap();
    let frame = shown(&mut live);
    assert!(frame.contains("Built for Live"), "{frame}");
    assert!(frame.contains("Chart: no chart"), "{frame}");

    // A capture reads the scenario's stand-ins, in views and surfaces.
    let mut capture = TestApp::<PlaylistApp>::capture(Some("details")).unwrap();
    let frame = shown(&mut capture);
    assert!(frame.contains("Built for Capture"), "{frame}");
    assert!(frame.contains("Chart: Kind of Blue"), "{frame}");
    assert!(frame.contains("Notes: Recorded in two sessions"), "{frame}");
    assert!(frame.contains("in Some(Capture)"), "{frame}");

    // History rebuilds the view over a past version: it starts nothing and
    // reads no fixture.
    let first = capture.versions()[1].hash;
    let past = capture.past_version(&[first]).unwrap();
    let texts = past.texts();
    assert!(
        texts.iter().any(|text| text == "Built for Past"),
        "{texts:?}"
    );
    assert!(
        texts.iter().any(|text| text == "Chart: no chart"),
        "{texts:?}"
    );
}

#[test]
fn a_capture_waits_for_its_surfaces_to_settle() {
    let mut app = TestApp::<PlaylistApp>::capture(Some("details")).unwrap();
    // The cover came from the fixture's content store and finished decoding
    // before the capture returned.
    let frame = shown(&mut app);
    assert!(frame.contains("cover cover art"), "{frame}");
}

#[test]
fn a_scenario_is_drawn_at_its_own_viewport_after_layout() {
    for (scenario, viewport) in [(Some("narrow"), [360, 720]), (None, [1024, 720])] {
        let mut app = TestApp::<PlaylistApp>::capture(scenario).unwrap();
        assert_eq!(app.harness().inspection().viewport, viewport);
        let frame = shown(&mut app);
        let staged_width = frame
            .split(" px")
            .next()
            .and_then(|before| before.rsplit(' ').next())
            .and_then(|px| px.parse::<f64>().ok())
            .unwrap_or_else(|| panic!("no staged width in {frame}"));
        assert!(staged_width > 0.0, "{frame}");
    }
}

#[test]
fn a_scenario_shows_other_peoples_versions_and_presence() {
    let mut app = TestApp::<PlaylistApp>::capture(Some("details")).unwrap();
    let authored: Vec<(String, i64)> = app
        .versions()
        .iter()
        .filter_map(|version| {
            let invoker = version.context.as_ref()?.invoker.as_ref()?;
            Some((invoker.actor.clone(), version.time_ms))
        })
        .collect();
    assert!(
        authored.contains(&("miles".to_owned(), MILES_AT_MS)),
        "{authored:?}"
    );
    assert!(
        authored.contains(&("bill".to_owned(), BILL_AT_MS)),
        "{authored:?}"
    );
    // Other people's versions are not the capture's own undo steps: undo
    // takes back the scenario's actions and stops at their work.
    while app.undo() {}
    let titles: Vec<String> = app
        .state()
        .songs
        .values()
        .map(|s| s.title.clone())
        .collect();
    assert_eq!(titles, ["Freddie Freeloader", "Flamenco Sketches"]);

    let (key, _) =
        ResourceSelection::Item("second-take".into()).presence::<Playlist>(PlaylistApp::ID);
    let miles = tilo_app::permissions::UserId("miles".into());
    let presence = app.harness().presence();
    assert!(presence.selection_for(&key, &miles).is_some());
    assert_eq!(presence.display_name(&miles), "miles");
}

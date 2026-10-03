#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! Screenshot scenarios on the framework: actions stage the resource, views
//! and surfaces stage their own view state, and the capture shows both.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_app::prelude::*;

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
        Box::new(
            Column::new()
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
                .push(cx.surface(Staged::default())),
        )
    }

    fn scenarios() -> &'static [&'static str] {
        &["details"]
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Playlist>> {
        match name {
            Some("details") => vec![
                Playlist::ADD.with("Blue in Green".into()),
                Playlist::ADD.with("So What".into()),
            ],
            _ => Vec::new(),
        }
    }
}

thread_local! {
    static STAGED: RefCell<Vec<Option<String>>> = const { RefCell::new(Vec::new()) };
}

/// A custom surface that records the scenarios it stages, and what it had
/// been presented by then.
struct Staged {
    inner: Text,
    label: LocalState<String>,
    songs: usize,
}

impl Default for Staged {
    fn default() -> Self {
        let label = LocalState::new("Surface not staged".to_owned());
        Self {
            inner: Text::from_state(&label, Clone::clone),
            label,
            songs: 0,
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

    fn stage(&mut self, scenario: Option<&str>) {
        STAGED.with(|staged| staged.borrow_mut().push(scenario.map(str::to_owned)));
        self.label
            .set(format!("Surface staged with {} songs", self.songs));
    }
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
        let mut app = TestApp::<PlaylistApp>::new().unwrap();
        assert!(shown(&mut app).contains("Details closed"));
        app.stage(Some("details")).unwrap();
        let frame = shown(&mut app);
        assert!(frame.contains("2 songs"), "{frame}");
        assert!(frame.contains("Selected: Blue in Green"), "{frame}");
        assert!(frame.contains("Details open"), "{frame}");
        // The surface staged after it was presented the scenario's actions.
        assert!(frame.contains("Surface staged with 2 songs"), "{frame}");
        assert_eq!(
            STAGED.with(|staged| staged.borrow().clone()),
            [Some("details".to_owned())]
        );
        // View state is not resource state: only the actions were written.
        assert_eq!(app.state().songs.len(), 2);
    }
    // phase: a_plain_capture_stages_with_no_scenario
    {
        // Each case starts from an empty staging record, as its own test thread did.
        STAGED.with(|staged| staged.borrow_mut().clear());
        let mut app = TestApp::<PlaylistApp>::new().unwrap();
        app.stage(None).unwrap();
        assert!(shown(&mut app).contains("0 songs"));
        assert!(shown(&mut app).contains("Details closed"));
        assert_eq!(STAGED.with(|staged| staged.borrow().clone()), [None]);
    }
    // phase: a_scenario_composes_the_same_scene_every_time
    {
        // Each case starts from an empty staging record, as its own test thread did.
        STAGED.with(|staged| staged.borrow_mut().clear());
        let print = || {
            let mut app = TestApp::<PlaylistApp>::new().unwrap();
            app.stage(Some("details")).unwrap();
            let encoding = app.harness().scene().encoding().clone();
            (encoding.path_data, encoding.draw_data, encoding.n_paths)
        };
        assert_eq!(print(), print());
    }
}

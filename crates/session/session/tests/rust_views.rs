//! A Rust app's view on the canvas: a Tube channel placed as a place draws the app's own `page` —
//! its name, its avatar, its subscribe action and its videos — in the shell, exactly as a tilolang
//! app's view draws (one-app-model.md §2).
//! Tube declares `Channel` as a resource and `page` as its default view. The channel is placed
//! through the arrangement's published `place` , as an agent or the person does; the shell's
//! runtime draws the channel's reading through Tube's view, and what stands on screen is read from
//! what the shell lays out for assistive technology, never from pixels.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use serde_json::{Map, Value, json};
use tilo_actions::DoorError;
use tilo_actions::ResourceSpec;
use tilo_compositor::OutputConfig;
use tilo_service::{Service, Started};
use tilo_session::{ActionPlane, Options, Session};

/// The channel the test places: a labelled sample channel with the videos its page lists.
const CHANNEL: &str = "tube.channel:ch_lowlevel";
/// What the channel is called, as its page shows it.
const NAME: &str = "Low Level Learning";
/// A video the channel published, as its page lists it (the sample catalogue's title).
const VIDEO: &str = "I rewrote my window compositor in Rust and it changed EVERYTHING!!!";

/// The Tube app's channels as a session publishes them: each a resource of the app `tube`,
/// through its own `page` view, read from the sample catalogue (`tube`'s labelled fixtures).
struct Channels {
    model: tube::model::TubeModel,
}

impl Channels {
    fn new() -> Self {
        Self {
            model: tube::sample::model(),
        }
    }

    /// The sample channel `id` , as its page's reading.
    fn channel(&self, id: &str) -> Option<tube::views::Channel> {
        tube::views::Channel::from_model(&self.model, id)
    }
}

impl Service for Channels {
    fn resources(&self) -> Vec<ResourceSpec> {
        let channels = self.model.channels.iter().map(|channel| {
            let id = format!("tube.channel:{}", channel.id);
            let mut spec = ResourceSpec::new(id, "tube.channel", channel.name.clone()).with_view(
                tilo_actions::ResourceViewSpec::new("page", channel.name.clone(), "open"),
            );
            spec.app_id = Some("tube".to_owned());
            spec
        });
        // Each video is a resource of the app too: what a channel's page opens is one, and a place
        // takes the resource a press names.
        let videos = self.model.videos.iter().map(|video| {
            let mut spec = ResourceSpec::new(
                format!("tube.video:{}", video.id),
                "tube.video",
                video.title.clone(),
            );
            spec.app_id = Some("tube".to_owned());
            spec
        });
        channels.chain(videos).collect()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        if let Some(id) = resource.strip_prefix("tube.channel:") {
            return self
                .channel(id)
                .map(|channel| serde_json::to_value(channel).expect("a channel is JSON"));
        }
        if let Some(id) = resource.strip_prefix("tube.video:") {
            return self
                .model
                .videos
                .iter()
                .find(|video| video.id == id)
                .map(|video| serde_json::to_value(video).expect("a video is JSON"));
        }
        None
    }

    fn invoke(
        &mut self,
        _resource: &str,
        _action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// A session with the shell as the whole screen and the Tube channels published, its test hooks at
/// `root` , the framed canvas in front.
fn session(root: &std::path::Path) -> Session {
    let program = std::env::current_exe().expect("this test's program");
    let apps = common::seeded_apps();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: common::PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![Box::new(apps), Box::new(Channels::new())],
            questions: Some(tilo_service::Questions::new()),
            action_plane: ActionPlane::At(root.join("plane.sock")),
            agent_door: Some(root.join("agent.sock")),
            admit_repl: vec![program],
            test_control: Some(root.join("test-control.sock")),
            ..Options::default()
        },
        &[OutputConfig::new(
            "HEADLESS-1",
            common::WIDTH,
            common::HEIGHT,
        )],
    )
    .expect("session");
    common::show_preset(&mut session, "Frames");
    common::until(&mut session, "the canvas", |session| {
        common::on_screen(session, common::CANVAS_ENTRY).is_some()
    });
    session
}

/// Place `resource` on the canvas of the workspace in front, through the arrangement's published
/// `place` , as an agent puts it there.
fn place(session: &mut Session, resource: &str) -> u64 {
    let (workspace, row, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.slot.0.to_string(),
            workspace.strips.first().expect("a row").id.0,
            session.handle().mint_id(),
            session.handle().mint_id(),
        )
    };
    let arguments: Map<String, Value> = json!({
        "workspace": workspace,
        "row": row,
        "strip": strip,
        "place": place,
        "resource": resource,
        "x": 80,
        "y": 60,
        "width": 480,
        "height": 360,
    })
    .as_object()
    .unwrap()
    .clone();
    session
        .invoke("shell:arrangement", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    place
}

/// Save a snapshot of what the session shows to `name` under `$TILO_RUST_VIEWS_SHOT` , when the
/// variable names a directory: how a person sees the page, taken over the test-control socket
/// `tilo-control snapshot` uses, never from a private path.
fn shot(session: &mut Session, root: &std::path::Path, name: &str) {
    let Some(dir) = std::env::var_os("TILO_RUST_VIEWS_SHOT").filter(|dir| !dir.is_empty()) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the shots directory");
    let png = dir.join(name);
    let socket = root.join("test-control.sock");
    let answer = common::control(
        session,
        &socket,
        &json!({"op": "snapshot", "path": png.to_str().unwrap()}),
    );
    assert!(
        answer.get("ok").is_some(),
        "the snapshot is taken: {answer}"
    );
}

#[test]
fn a_tube_channel_placed_on_the_canvas_draws_its_own_page() {
    let _settled = ui::motion::testing::settled();
    let root = common::private_home();
    let mut session = session(root.path());
    place(&mut session, CHANNEL);
    common::until(&mut session, "the channel's page", |session| {
        session.shell_says(NAME)
    });
    // The channel's own view is on the canvas: its name, its handle and its videos, each the
    // component Tube's page composed.
    assert!(
        session.shell_says(NAME),
        "the channel's page shows its name: {:?}",
        session.accessible_labels()
    );
    assert!(
        session.shell_says("@lowlevellearning"),
        "the page shows its handle: {:?}",
        session.accessible_labels()
    );
    common::until(&mut session, "a video of the channel's", |session| {
        session.shell_says(VIDEO)
    });
    assert!(
        session.shell_says(VIDEO),
        "the page lists the channel's videos: {:?}",
        session.accessible_labels()
    );
    // The subscribe action the page offers is the type's: pressing it does not refuse.
    assert!(
        session.shell_control("Subscribe").is_some()
            || session.shell_control("Subscribed").is_some(),
        "the page offers the subscribe action: {:?}",
        session.accessible_labels()
    );
    // How the person sees it: a real frame of the session, when a shots directory is named.
    shot(&mut session, root.path(), "channel-page.png");
}

#[test]
fn pressing_a_videos_row_places_the_video() {
    let _settled = ui::motion::testing::settled();
    let root = common::private_home();
    let mut session = session(root.path());
    place(&mut session, CHANNEL);
    common::until(&mut session, "the channel's page", |session| {
        session.shell_says(VIDEO)
    });
    // Pressing the video's row shows the video: the place the press asks for stands on the
    // arrangement, as a press on a tilolang view's row does.
    common::press(&mut session, VIDEO);
    common::until(&mut session, "the video is placed", |session| {
        common::places(session)
            .iter()
            .any(|place| place.record.starts_with("tube.video:"))
    });
    assert!(
        common::places(&session)
            .iter()
            .any(|place| place.record.starts_with("tube.video:")),
        "a video's row places the video: {:?}",
        common::places(&session)
            .iter()
            .map(|place| place.record.clone())
            .collect::<Vec<_>>()
    );
}

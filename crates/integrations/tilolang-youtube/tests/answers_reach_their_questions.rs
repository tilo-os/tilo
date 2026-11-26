//! What the service answers reaches the record the question was about, whatever order the
//! service answers in. An instance hands its origins their effects from one frame, and each is
//! asked on a thread of its own, so a stream asked beside a search can come back first: its
//! answer is the stream's, and the video it was asked for keeps the address its bytes play from.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use tilolang_tilo::host::Host;
use tilolang_tilo::origin::{Origin, OriginCx};
use tilolang_tilo::runtime::{Actor, ResourceRef, Value};
use tilolang_youtube::{
    Branding, ChannelPage, CommentsPage, PlaylistPage, Scripted, SearchFilter, SearchPage,
    SegmentPage, Service, ServiceError, TrendingPage, VideoItem, Votes,
};

const TUBE: &str = "so.tilo.tube";
const VIDEO: &str = "v-compositor";
const TITLE: &str = "I rewrote my window compositor in Rust";
const STREAM: &str = "https://media.example/v-compositor.mp4";

/// The scripted YouTube, with the second search held until the test lets it go: the stream asked
/// beside it answers first, as a connector can.
struct Held {
    scripted: Scripted,
    searches: AtomicUsize,
    release: Mutex<bool>,
    released: Condvar,
}

impl Held {
    fn new() -> Self {
        Self {
            scripted: Scripted {
                searches: [(
                    "compositor".to_owned(),
                    SearchPage {
                        query: "compositor".to_owned(),
                        channels: Vec::new(),
                        playlists: Vec::new(),
                        videos: vec![VideoItem {
                            key: VIDEO.to_owned(),
                            title: TITLE.to_owned(),
                            published: "2026-11-01T12:00:00Z".to_owned(),
                            ..VideoItem::default()
                        }],
                    },
                )]
                .into_iter()
                .collect(),
                streams: [(VIDEO.to_owned(), STREAM.to_owned())]
                    .into_iter()
                    .collect(),
                ..Scripted::default()
            },
            searches: AtomicUsize::new(0),
            release: Mutex::new(false),
            released: Condvar::new(),
        }
    }

    /// Let the search that is held answer.
    fn release(&self) {
        *self
            .release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.released.notify_all();
    }
}

impl Service for Held {
    fn search(&self, query: &str, filter: &SearchFilter) -> Result<SearchPage, ServiceError> {
        if self.searches.fetch_add(1, Ordering::SeqCst) > 0 {
            let mut go = self
                .release
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while !*go {
                go = self
                    .released
                    .wait(go)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            drop(go);
        }
        self.scripted.search(query, filter)
    }

    fn stream(&self, key: &str, quality: &str) -> Result<String, tilolang_youtube::ServiceError> {
        self.scripted.stream(key, quality)
    }

    fn channel(&self, _: &str) -> Result<ChannelPage, ServiceError> {
        Err(ServiceError("no channel was asked for".to_owned()))
    }

    fn playlist(&self, _: &str) -> Result<PlaylistPage, ServiceError> {
        Err(ServiceError("no playlist was asked for".to_owned()))
    }

    fn trending(&self) -> Result<TrendingPage, ServiceError> {
        Err(ServiceError("no trending list was asked for".to_owned()))
    }

    fn comments(&self, _: &str) -> Result<CommentsPage, ServiceError> {
        Err(ServiceError("no comments were asked for".to_owned()))
    }

    fn segments(&self, _: &str) -> Result<SegmentPage, ServiceError> {
        Err(ServiceError("no segments were asked for".to_owned()))
    }

    fn branding(&self, _: &str) -> Result<Branding, ServiceError> {
        Err(ServiceError("no branding was asked for".to_owned()))
    }

    fn votes(&self, _: &str) -> Result<Votes, ServiceError> {
        Err(ServiceError("no votes were asked for".to_owned()))
    }
}

/// Ada's workspace with the tilolang Tube, as the app's window opens it.
fn opened() -> Host {
    let host = Host::new(
        Actor::Member(tilolang_tilo::runtime::ids::MemberId::person("Ada")),
        || None,
    )
    .expect("the workspace");
    // The session the app plays through, which its `use` names.
    host.load_builtin(
        include_str!("../../../../apps/tl/playback/app.tl"),
        include_str!("../../../../apps/tl/playback/app.ids"),
    )
    .expect("the session");
    let package = (
        include_str!("../../../../apps/tl/tube/app.tl"),
        include_str!("../../../../apps/tl/tube/app.ids"),
    );
    host.load_builtin(package.0, package.1).expect("the client");
    host
}

/// The id of the field `field` of the package's type `ty`.
fn field_id(host: &Host, ty: &str, field: &str) -> String {
    let packages = host.packages();
    let package = packages
        .iter()
        .find(|package| package.meta.app == TUBE)
        .expect("the client");
    package
        .meta
        .types
        .iter()
        .find(|t| t.name == ty)
        .and_then(|t| t.fields.iter().find(|f| f.name == field))
        .unwrap_or_else(|| panic!("{ty}.{field}"))
        .id
        .clone()
}

/// What the videos the client holds read for `field`.
fn videos(host: &Host, field: &str) -> Vec<Value> {
    let root = host.root_of(TUBE).expect("the root");
    let Ok(Value::List(held)) = host
        .runtime()
        .read(&root, &field_id(host, "YouTube", "videos"))
    else {
        panic!("the root holds its videos");
    };
    let id = field_id(host, "Video", field);
    held.into_iter()
        .map(|item| {
            let Value::Ref(at) = item else {
                panic!("a video is a resource");
            };
            let at: ResourceRef = (*at.target).clone();
            host.runtime().read(&at, &id).expect("the field")
        })
        .collect()
}

/// Poll the origin once, as the instance's frame does, with the workspace it writes into.
fn poll(host: &Host, origin: &std::rc::Rc<std::cell::RefCell<tilolang_youtube::YouTubeOrigin>>) {
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let cx = OriginCx {
        host,
        app: TUBE,
        waker: &wake,
        now: String::new(),
    };
    origin.borrow_mut().poll(&cx);
}

/// A stream asked beside a search whose answer is held until after the stream's keeps the
/// address it was answered with: the video's bytes play from it, though the search answered
/// last.
#[test]
fn a_streams_answer_reaches_the_video_though_another_question_answers_first() {
    let host = opened();
    let service = Arc::new(Held::new());
    let woken = Arc::new(AtomicBool::new(false));
    let (answered, answers) = std::sync::mpsc::channel::<()>();
    let answered = Mutex::new(answered);
    let origin = tilolang_youtube::origin(service.clone(), Arc::clone(&woken));
    let origin = tilolang_youtube::waking(
        origin,
        Arc::new(move || {
            let _ = answered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .send(());
        }),
    );

    // The search that finds the video is answered and written.
    origin
        .borrow_mut()
        .deliver("youtube.search", &serde_json::json!("compositor"));
    // A deliver wakes the frame once itself, and once more when its answer is in.
    wait(&answers, 2);
    poll(&host, &origin);
    assert!(
        videos(&host, "key")
            .iter()
            .any(|key| key == &Value::text(VIDEO)),
        "the search wrote the video it found"
    );

    // The video's bytes are asked for while another search is asked beside it and held: the
    // stream answers first, and its address is the video's.
    origin
        .borrow_mut()
        .deliver("youtube.search", &serde_json::json!("compositor"));
    origin
        .borrow_mut()
        .deliver("youtube.stream", &serde_json::json!([VIDEO]));
    // The two delivers' wakes and the stream's answer, which the held search has not given.
    wait(&answers, 3);
    service.release();
    // What the held search answers now.
    wait(&answers, 1);
    poll(&host, &origin);
    assert!(
        videos(&host, "stream")
            .iter()
            .any(|stream| stream == &Value::text(STREAM)),
        "the video plays from the address the service answered with"
    );
}

/// Wait for `count` wakes from the origin: each deliver's own and each answer's.
fn wait(answers: &std::sync::mpsc::Receiver<()>, count: usize) {
    for _ in 0..count {
        answers
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the service answers");
    }
}

//! What an app's call does through the service: it joins, is listened to
//! muted, says why it cannot be heard, and ends when it leaves or when its
//! app's connection does.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use runtime_collab::native_media::{
    Command, Input, Issue, Picture, PictureLayer, PictureSource, Request, Shown, Snapshot, State,
};

use super::{Caller, NativeMediaService};
use crate::media::{CallTarget, MediaFactory, MediaSession, Prepare};

/// A carrier that puts this device into any channel at once, with the
/// microphone `inputs` names.
struct Carrier {
    inputs: Vec<Input>,
    state: Arc<CallState>,
}

impl MediaFactory for Carrier {
    fn prepare(&self, _target: CallTarget) -> Prepare {
        let inputs = self.inputs.clone();
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            Ok(Box::new(Call {
                inputs,
                state,
                camera: false,
                screen: false,
            }) as Box<dyn MediaSession>)
        })
    }
}

/// What the app and the call exchange about pictures: what it shows, and
/// what arrived for it.
#[derive(Default)]
struct CallState {
    shown: Mutex<Vec<Shown>>,
    pictures: Mutex<Vec<Picture>>,
    arrived: Arc<tokio::sync::Notify>,
}

struct Call {
    inputs: Vec<Input>,
    state: Arc<CallState>,
    camera: bool,
    screen: bool,
}

impl MediaSession for Call {
    fn tick(&mut self, _now: Instant) -> Result<State, Issue> {
        Ok(State::Live)
    }

    fn inputs(&self) -> Vec<Input> {
        self.inputs.clone()
    }

    fn start_capture(&mut self, _input: &str) -> Result<(), Issue> {
        Ok(())
    }

    fn show(&mut self, pictures: Vec<Shown>) {
        *self.state.shown.lock().expect("the app's pictures") = pictures;
    }

    fn set_camera(&mut self, on: bool) -> Result<(), Issue> {
        self.camera = on;
        Ok(())
    }

    fn set_screen(&mut self, on: bool) -> Result<(), Issue> {
        self.screen = on;
        Ok(())
    }

    fn pictures(&mut self, after: u64) -> (Vec<Picture>, u64) {
        let taken = std::mem::take(
            &mut *self
                .state
                .pictures
                .lock()
                .expect("the pictures that arrived"),
        );
        let cursor = after.saturating_add(taken.len() as u64);
        (taken, cursor)
    }

    fn pictures_changed(&self) -> Option<Arc<tokio::sync::Notify>> {
        Some(Arc::clone(&self.state.arrived))
    }
}

/// A carrier whose call publishes no pictures: it says so instead of showing
/// one nobody receives.
struct Silent;

impl MediaFactory for Silent {
    fn prepare(&self, _target: CallTarget) -> Prepare {
        Box::pin(async move { Ok(Box::new(NoPictures) as Box<dyn MediaSession>) })
    }
}

struct NoPictures;

impl MediaSession for NoPictures {
    fn tick(&mut self, _now: Instant) -> Result<State, Issue> {
        Ok(State::Live)
    }
}

fn microphone() -> Input {
    Input {
        id: "mic".to_owned(),
        label: "Microphone".to_owned(),
    }
}

fn caller(current: Arc<AtomicBool>) -> Caller {
    Caller {
        peer: 7,
        app: "so.tilo.chat".to_owned(),
        workspace: "room".to_owned(),
        record: "chat".to_owned(),
        current: Arc::new(move || {
            if current.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err("this app is no longer connected".to_owned())
            }
        }),
    }
}

fn request(intent: u64, command: Command) -> Request {
    Request {
        version: 1,
        intent,
        command,
    }
}

fn join(channel: &str) -> Command {
    Command::Join {
        channel_id: channel.to_owned(),
    }
}

/// One command as an app sends it: through the same dispatch the wire uses,
/// so its answer carries the rosters and a watcher of the call wakes.
fn send(service: &NativeMediaService, who: &Caller, request: Request) -> Result<Snapshot, String> {
    let route = (who.workspace.clone(), who.app.clone(), who.record.clone());
    service.dispatch(who.clone(), request, &route)
}

/// An unsettled call: the carrier was handed a channel and has not said
/// where the call stands yet.
fn service(inputs: Vec<Input>) -> NativeMediaService {
    service_with(inputs).0
}

/// A service and what its carrier's call took in and handed back.
fn service_with(inputs: Vec<Input>) -> (NativeMediaService, Arc<CallState>) {
    let state = Arc::new(CallState::default());
    let service = NativeMediaService::new();
    assert!(service.install_factory(Arc::new(Carrier {
        inputs,
        state: Arc::clone(&state),
    })));
    (service, state)
}

/// The call's status as an app reads it, once its carrier says where it
/// stands.
async fn settled(service: &NativeMediaService, who: &Caller) -> Snapshot {
    for _ in 0..200 {
        let status = send(service, who, request(0, Command::Status)).expect("the call's status");
        if status.state == State::Live {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the call never became live");
}

#[tokio::test]
async fn a_call_joins_listening_muted_and_leaving_ends_it() {
    let service = service(vec![microphone()]);
    let who = caller(Arc::new(AtomicBool::new(true)));

    let joined =
        send(&service, &who, request(1, join("c_voice"))).expect("an app joins a voice channel");
    assert_eq!(joined.state, State::Joining);
    assert!(joined.muted, "a joined call listens and is not heard");

    let live = settled(&service, &who).await;
    assert_eq!(live.state, State::Live);
    assert_eq!(live.inputs, [microphone()]);
    assert_eq!(
        service.own_call().map(|call| call.channel),
        Some("c_voice".to_owned()),
        "this device's call is the channel it joined"
    );

    let left = send(&service, &who, request(2, Command::Leave)).expect("an app leaves the call");
    assert_eq!(left.state, State::Ended);
    assert!(
        service.own_call().is_none(),
        "the call is no longer this device's"
    );
}

/// A command at or below the last intent of this route changes nothing: an
/// old join cannot start a call the person already left.
#[tokio::test]
async fn an_intent_the_route_already_carried_is_refused() {
    let service = service(vec![microphone()]);
    let who = caller(Arc::new(AtomicBool::new(true)));

    send(&service, &who, request(5, join("c_voice"))).expect("the first join");
    let refused = send(&service, &who, request(5, join("c_other")));
    assert!(refused.is_err(), "the same intent twice changes nothing");
    let refused = send(&service, &who, request(4, Command::Leave));
    assert!(
        refused.is_err(),
        "a leave older than the join is not a leave"
    );
    assert_eq!(
        service.own_call().map(|call| call.channel),
        Some("c_voice".to_owned())
    );
}

/// Unmuting without a microphone to send leaves the person in the call,
/// muted, and the status says why.
#[tokio::test]
async fn a_call_with_no_microphone_stays_muted_and_says_so() {
    let service = service(Vec::new());
    let who = caller(Arc::new(AtomicBool::new(true)));

    send(&service, &who, request(1, join("c_voice"))).expect("an app joins a voice channel");
    settled(&service, &who).await;

    let unmuted = send(
        &service,
        &who,
        request(2, Command::SetMuted { muted: false }),
    )
    .expect("an app asks to be heard");
    assert!(unmuted.muted, "nothing is captured without a microphone");
    assert_eq!(unmuted.issue, Some(Issue::SourceLost));
    assert_eq!(
        unmuted.state,
        State::Live,
        "the person is still in the call"
    );
}

/// A microphone to send: unmuting captures it, the status says the call is
/// heard, and muting stops it again.
#[tokio::test]
async fn a_call_that_is_heard_says_which_microphone_it_sends() {
    let service = service(vec![microphone()]);
    let who = caller(Arc::new(AtomicBool::new(true)));

    send(&service, &who, request(1, join("c_voice"))).expect("an app joins a voice channel");
    settled(&service, &who).await;

    let heard = send(
        &service,
        &who,
        request(2, Command::SetMuted { muted: false }),
    )
    .expect("an app asks to be heard");
    assert!(!heard.muted);
    assert!(heard.capture_active, "the call sends the microphone");
    assert_eq!(heard.input_id, Some("mic".to_owned()));
    assert_eq!(heard.issue, None);

    let muted = send(
        &service,
        &who,
        request(3, Command::SetMuted { muted: true }),
    )
    .expect("an app mutes");
    assert!(muted.muted);
    assert!(!muted.capture_active);
}

/// Deafening mutes and says it in the call; undeafening puts the microphone
/// back as it was.
#[tokio::test]
async fn deafening_mutes_and_undeafening_restores_the_microphone() {
    let service = service(vec![microphone()]);
    let who = caller(Arc::new(AtomicBool::new(true)));

    send(&service, &who, request(1, join("c_voice"))).expect("an app joins a voice channel");
    settled(&service, &who).await;
    send(
        &service,
        &who,
        request(2, Command::SetMuted { muted: false }),
    )
    .expect("an app asks to be heard");

    let deaf = send(
        &service,
        &who,
        request(3, Command::SetDeafened { deafened: true }),
    )
    .expect("an app deafens");
    assert!(deaf.deafened);
    assert!(deaf.muted, "a deafened call is muted too");
    assert!(!deaf.capture_active);

    let back = send(
        &service,
        &who,
        request(4, Command::SetDeafened { deafened: false }),
    )
    .expect("an app undeafens");
    assert!(!back.deafened);
    assert!(!back.muted, "the microphone comes back as it was");
    assert!(back.capture_active);
}

/// A call whose app went away ends on the carrier's next look, so no
/// microphone is left open for an app nobody is talking through.
#[tokio::test]
async fn a_call_ends_when_the_app_that_joined_it_disconnects() {
    let service = service(vec![microphone()]);
    let connected = Arc::new(AtomicBool::new(true));
    let who = caller(Arc::clone(&connected));

    send(&service, &who, request(1, join("c_voice"))).expect("an app joins a voice channel");
    settled(&service, &who).await;

    connected.store(false, Ordering::Release);
    for _ in 0..200 {
        if service.own_call().is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the call outlived its app");
}

/// A camera or a screen the call cannot publish says so instead of showing
/// a picture nobody receives.
#[tokio::test]
async fn a_camera_the_call_cannot_publish_says_so() {
    let service = NativeMediaService::new();
    assert!(service.install_factory(Arc::new(Silent)));
    let who = caller(Arc::new(AtomicBool::new(true)));

    send(&service, &who, request(1, join("c_voice"))).expect("an app joins a voice channel");
    settled(&service, &who).await;

    let camera = send(&service, &who, request(2, Command::SetCamera { on: true }))
        .expect("an app turns its camera on");
    assert_eq!(camera.issue, Some(Issue::CameraLost));
    assert!(!camera.camera);
    let screen = send(&service, &who, request(3, Command::SetScreen { on: true }))
        .expect("an app shares its screen");
    assert_eq!(screen.issue, Some(Issue::ScreenLost));
    assert!(!screen.screen);
}

/// The camera and the screen an app turns on say so, and what it shows
/// reaches the call as it said it.
#[tokio::test]
async fn an_app_says_which_pictures_it_shows_and_their_state_says_which_it_sends() {
    let (service, state) = service_with(vec![microphone()]);
    let who = caller(Arc::new(AtomicBool::new(true)));
    send(&service, &who, request(1, join("c_voice"))).expect("an app joins a voice channel");
    settled(&service, &who).await;

    let camera = send(&service, &who, request(2, Command::SetCamera { on: true }))
        .expect("a call that publishes a camera says so");
    assert!(camera.camera, "the call's camera is on");
    assert_eq!(camera.issue, None);
    let screen = send(&service, &who, request(3, Command::SetScreen { on: true }))
        .expect("a call that shares a screen says so");
    assert!(screen.screen, "the call's screen is shared");
    assert_eq!(screen.issue, None);

    let shown = vec![Shown {
        person: "ab".repeat(32),
        source: PictureSource::Camera,
        layer: PictureLayer::Low,
    }];
    send(
        &service,
        &who,
        request(
            0,
            Command::Show {
                pictures: shown.clone(),
            },
        ),
    )
    .expect("an app shows what its tiles hold");
    assert_eq!(
        *state.shown.lock().expect("the app's pictures"),
        shown,
        "the call receives exactly the pictures the app shows"
    );

    let off = send(&service, &who, request(4, Command::SetCamera { on: false }))
        .expect("an app turns its camera off");
    assert!(!off.camera, "the call stops sending a camera nobody shows");
}

/// Pictures are answered as the call receives them, with the cursor the app
/// reads the next ones from.
#[tokio::test]
async fn pictures_are_answered_as_they_arrive_and_move_the_cursor() {
    let (service, state) = service_with(vec![microphone()]);
    let who = caller(Arc::new(AtomicBool::new(true)));
    send(&service, &who, request(1, join("c_voice"))).expect("an app joins a voice channel");
    settled(&service, &who).await;

    let picture = Picture {
        person: "ab".repeat(32),
        stream: "cd".repeat(32),
        source: PictureSource::Screen,
        object: vec![1; 64],
    };
    let waiting = service.pictures(who.clone(), 0);
    let arriving = {
        let state = Arc::clone(&state);
        let picture = picture.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            state
                .pictures
                .lock()
                .expect("the pictures that arrived")
                .push(picture);
            state.arrived.notify_waiters();
        }
    };
    let (answered, ()) = tokio::join!(waiting, arriving);
    let answered = answered.expect("the pictures that arrived");
    assert_eq!(answered.pictures, vec![picture]);
    assert_eq!(answered.pictures_cursor, 1, "the next read starts after it");

    // Nothing new arrived: the answer is empty and the cursor stays put.
    let empty = service
        .pictures(who, answered.pictures_cursor)
        .await
        .expect("an answer with no pictures");
    assert!(empty.pictures.is_empty());
    assert_eq!(empty.pictures_cursor, 1);
}

/// A watch is answered the moment the call's status moves, not on a poll,
/// and never later than its hold.
#[tokio::test]
async fn a_watch_is_answered_when_the_call_changes() {
    let service = service(vec![microphone()]);
    let who = caller(Arc::new(AtomicBool::new(true)));
    send(&service, &who, request(1, join("c_voice"))).expect("an app joins a voice channel");
    let live = settled(&service, &who).await;

    let watching = service.watch(
        who.clone(),
        live.revision,
        ("room".to_owned(), "chat".to_owned(), "chat".to_owned()),
    );
    let leaving = {
        let service = service.clone();
        let who = who.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            send(&service, &who, request(2, Command::Leave)).expect("the app leaves");
        }
    };
    let (watched, ()) = tokio::join!(watching, leaving);
    let watched = watched.expect("the watch is answered");
    assert_eq!(
        watched.state,
        State::Idle,
        "the leave woke the watch, which no longer sees a call"
    );
}

/// The service hands back what a voice channel shows: nothing at all until
/// its carrier says who is in the record's channels.
#[tokio::test]
async fn a_status_carries_the_rosters_its_carrier_knows() {
    let service = service(vec![microphone()]);
    let who = caller(Arc::new(AtomicBool::new(true)));
    let status = send(&service, &who, request(0, Command::Status)).expect("the call's status");
    assert_eq!(status.state, State::Idle);
    assert!(status.rosters.is_empty());
}

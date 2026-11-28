//! A call moves from the person's laptop to their phone: the phone asks, takes the call as it
//! stood, and the laptop leaves only once the phone's call is live. A no, no answer, or a call that
//! does not come up on the phone leave it on the laptop, and the person hears why.

#![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_actions::remote::native_media::{Command, Input, Issue, Request, State};
use tilo_native_media::{
    CallTarget, Caller, MediaFactory, MediaSession, NativeMediaService, Prepare,
};
use tilo_resource::{AppNamed, Replica};
use tilo_service::{AskReceiver, AskSender, asks::Answer};
use tilo_workspace_service::workspaces::person_workspace::devices::Devices;

use super::{CallMoves, Effect, Here, KIND};

const APP: &str = "so.tilo.chat";
const WORKSPACE: &str = "the-workspace";
const RECORD: &str = "chat-record";
const PLACE: &str = "voice/lounge";

/// A carrier that puts a device into any channel at once, live, or never when `stuck`.
struct Carrier {
    stuck: bool,
}

impl MediaFactory for Carrier {
    fn prepare(&self, _target: CallTarget) -> Prepare {
        let stuck = self.stuck;
        Box::pin(async move { Ok(Box::new(Call { stuck }) as Box<dyn MediaSession>) })
    }
}

struct Call {
    stuck: bool,
}

impl MediaSession for Call {
    fn tick(&mut self, _now: Instant) -> Result<State, Issue> {
        Ok(if self.stuck {
            State::Joining
        } else {
            State::Live
        })
    }

    fn inputs(&self) -> Vec<Input> {
        vec![Input {
            id: "mic".to_owned(),
            label: "Microphone".to_owned(),
        }]
    }

    fn start_capture(&mut self, _input: &str) -> Result<(), Issue> {
        Ok(())
    }

    fn set_camera(&mut self, _on: bool) -> Result<(), Issue> {
        Ok(())
    }
}

/// One of the person's devices: its key, its calls, where its person is asked, and its side of
/// the hand-offs.
struct Device {
    key: String,
    raw: [u8; 32],
    media: NativeMediaService,
    asker: AskSender,
    questions: AskReceiver,
    moves: CallMoves,
    /// The app's own count of its call steps.
    intent: u64,
}

impl Device {
    fn new(raw: [u8; 32], stuck: bool) -> Self {
        let media = NativeMediaService::new();
        assert!(media.install_factory(Arc::new(Carrier { stuck })));
        let (asker, questions) = tilo_service::asks::channel();
        Self {
            key: hex::encode(raw),
            raw,
            media,
            asker,
            questions,
            moves: CallMoves::default(),
            intent: 0,
        }
    }

    /// Chat's own connection to this device's calls.
    fn chat() -> Caller {
        Caller::connected(7, APP, WORKSPACE, RECORD, Arc::new(|| Ok(())))
    }

    /// A step Chat takes in its call, as its own.
    fn step(&mut self, command: Command) -> tilo_actions::remote::native_media::Snapshot {
        let intent = if matches!(command, Command::Status) {
            0
        } else {
            self.intent = self.intent.checked_add(1).expect("a call takes few steps");
            self.intent
        };
        self.media
            .command_for(
                Self::chat(),
                Request {
                    version: 1,
                    intent,
                    command,
                },
            )
            .unwrap()
    }

    /// What Chat's call view does as the call service tells it: a call handed here is joined the
    /// way Chat joins a call.
    fn chat_follows(&mut self) {
        if let Some(channel) = self.step(Command::Status).handed {
            self.step(Command::Join {
                channel_id: channel,
            });
        }
    }

    /// Take the hand-offs a step further, `at_ms` ms since the epoch, and carry out what they
    /// asked in `record`. What they asked to show, and to tell the person.
    fn follow(
        &mut self,
        record: &mut Replica<Devices, AppNamed>,
        at_ms: i64,
    ) -> (usize, Vec<String>) {
        let state = record.state().unwrap();
        let here = Here {
            record: &state,
            local: &self.key,
            media: &self.media,
            asker: &self.asker,
            now_ms: at_ms,
        };
        let effects = self.moves.follow(&here);
        self.apply(record, effects)
    }

    fn apply(
        &self,
        record: &mut Replica<Devices, AppNamed>,
        effects: Vec<Effect>,
    ) -> (usize, Vec<String>) {
        let (mut opened, mut told) = (0_usize, Vec::new());
        for effect in effects {
            match effect {
                Effect::Offer(offer) => {
                    record.apply(Devices::offering_continuity(&offer)).unwrap();
                }
                Effect::Answer {
                    from,
                    at_ms,
                    taken,
                    reason,
                } => {
                    record
                        .apply(Devices::answering_continuity(
                            &from,
                            at_ms,
                            &self.raw,
                            taken,
                            reason.as_deref(),
                        ))
                        .unwrap();
                }
                Effect::Open(open) => {
                    assert_eq!(
                        (
                            open.app.as_str(),
                            open.record.as_str(),
                            open.resource.as_str()
                        ),
                        (APP, RECORD, format!("tl:{APP}:{PLACE}").as_str()),
                        "the call's place is shown in its app"
                    );
                    opened = opened.saturating_add(1);
                }
                Effect::Tell { title, body } => told.push(format!("{title}: {body}")),
            }
        }
        (opened, told)
    }

    /// The person here answers the question about the call handed here.
    fn answer(&self, yes: bool) {
        let question = self
            .questions
            .pending()
            .into_iter()
            .find(|question| question.kind == KIND)
            .expect("the person is asked");
        if yes {
            assert!(self.questions.answer(&question.handle, Answer::default()));
        } else {
            assert!(self.questions.decline(&question.handle));
        }
    }

    async fn live(&self) {
        for _ in 0..400 {
            if self
                .media
                .own_call()
                .is_some_and(|call| call.state == State::Live)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the call never became live");
    }
}

/// The person's laptop, heard and on camera in the Lounge, and their phone; the record of their
/// devices keeps both.
async fn laptop_in_a_call(phone_stuck: bool) -> (Replica<Devices, AppNamed>, Device, Device) {
    let (mut laptop, phone) = (
        Device::new([1; 32], false),
        Device::new([2; 32], phone_stuck),
    );
    laptop.step(Command::Join {
        channel_id: PLACE.to_owned(),
    });
    laptop.step(Command::SetMuted { muted: false });
    laptop.live().await;
    laptop.step(Command::SetCamera { on: true });
    let mut record = Replica::<Devices>::new().in_app("so.tilo.model", "Model");
    for key in [&laptop.key, &phone.key] {
        record
            .apply(Devices::KEEP_DEVICE.with((key.clone(), "00".to_owned())))
            .unwrap();
    }
    (record, laptop, phone)
}

/// The laptop hands its call to the phone at `at_ms`.
fn continue_on_phone(
    record: &mut Replica<Devices, AppNamed>,
    laptop: &mut Device,
    phone: &Device,
    at_ms: i64,
) {
    let state = record.state().unwrap();
    let here = Here {
        record: &state,
        local: &laptop.key,
        media: &laptop.media,
        asker: &laptop.asker,
        now_ms: at_ms,
    };
    let call = laptop.media.own_call().unwrap();
    let effects = laptop.moves.send(&here, &call, &phone.key);
    assert!(
        laptop.moves.moving().is_some(),
        "the card says the call is moving"
    );
    laptop.apply(record, effects);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_moves_to_the_phone_as_it_stood_and_leaves_the_laptop_once_live() {
    let (mut record, mut laptop, mut phone) = laptop_in_a_call(false).await;
    continue_on_phone(&mut record, &mut laptop, &phone, 1_000);

    // The phone asks its person once, and nothing moves before they answer.
    phone.follow(&mut record, 2_000);
    phone.follow(&mut record, 2_100);
    assert_eq!(
        phone.questions.pending().len(),
        1,
        "the person is asked once whether to take the call"
    );
    assert!(phone.media.own_call().is_none());

    phone.answer(true);
    let (opened, _) = phone.follow(&mut record, 3_000);
    assert_eq!(opened, 1, "the yes shows the call's place in Chat");
    let (_, told) = laptop.follow(&mut record, 3_000);
    assert!(told.is_empty());
    assert!(
        laptop.media.own_call().is_some(),
        "the laptop stays in the call until the phone's is live"
    );

    // Chat on the phone joins the call handed to it as Chat joins a call.
    phone.chat_follows();
    phone.live().await;
    phone.follow(&mut record, 4_000);
    let (_, told) = laptop.follow(&mut record, 4_000);
    assert!(told.is_empty(), "a call that moved needs no word: {told:?}");
    assert!(
        laptop.media.own_call().is_none(),
        "the laptop left the call"
    );
    assert!(laptop.moves.moving().is_none());
    let on_phone = phone.media.own_call().unwrap();
    assert_eq!(on_phone.channel, PLACE);
    assert!(
        !on_phone.muted,
        "the person is heard on the phone as on the laptop"
    );
    assert!(on_phone.camera, "and on camera");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_the_phone_turns_down_stays_on_the_laptop_and_says_so() {
    let (mut record, mut laptop, mut phone) = laptop_in_a_call(false).await;
    continue_on_phone(&mut record, &mut laptop, &phone, 1_000);
    phone.follow(&mut record, 2_000);
    phone.answer(false);
    phone.follow(&mut record, 3_000);
    let (_, told) = laptop.follow(&mut record, 3_000);
    assert_eq!(told.len(), 1, "the person hears the call stayed: {told:?}");
    assert!(
        laptop.media.own_call().is_some(),
        "the call stays on the laptop"
    );
    assert!(phone.media.own_call().is_none());
    assert_eq!(phone.media.handed(), None, "nothing waits on the phone");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_nobody_takes_in_time_stays_on_the_laptop() {
    let (mut record, mut laptop, phone) = laptop_in_a_call(false).await;
    continue_on_phone(&mut record, &mut laptop, &phone, 1_000);
    let (_, told) = laptop.follow(&mut record, 30_000);
    assert!(told.is_empty(), "the phone still has time");
    let (_, told) = laptop.follow(&mut record, 1_000 + super::HAND_MS);
    assert_eq!(told.len(), 1, "the person hears nobody took it: {told:?}");
    assert!(laptop.media.own_call().is_some());
    assert!(
        laptop.moves.moving().is_none(),
        "the card offers to move it again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_that_never_comes_up_on_the_phone_is_given_back() {
    let (mut record, mut laptop, mut phone) = laptop_in_a_call(true).await;
    continue_on_phone(&mut record, &mut laptop, &phone, 1_000);
    phone.follow(&mut record, 2_000);
    phone.answer(true);
    phone.follow(&mut record, 3_000);
    phone.chat_follows();
    let (_, told) = phone.follow(&mut record, 30_000);
    assert!(told.is_empty(), "the call still has time to come up");
    laptop.follow(&mut record, 30_000);
    assert!(laptop.media.own_call().is_some());

    let (_, told) = phone.follow(&mut record, 1_000 + super::HAND_MS);
    assert_eq!(
        told.len(),
        1,
        "the phone's person hears it stayed: {told:?}"
    );
    assert!(
        phone.media.own_call().is_none(),
        "the call on its way in on the phone stops: the person is never in it twice"
    );
    let (_, told) = laptop.follow(&mut record, 1_000 + super::HAND_MS);
    assert_eq!(told.len(), 1, "and so does the laptop's: {told:?}");
    assert!(
        laptop.media.own_call().is_some(),
        "the call stays on the laptop"
    );
}

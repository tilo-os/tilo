//! A coding agent's program on the VM host that holds its workspace's machine.
//!
//! Where this server's machines are on VM hosts, a program it serves is started by the
//! member device of the one the placer names for the workspace, as a shell command is
//! ([`crate::placed_shells`]), and held there for this server
//! ([`tilo_actions::remote::environment_programs`]). This is the asking half: it starts the
//! program, takes what it sends, hands it what it is told, and ends it, always as this
//! server's own device asking as the program's own workspace.
//!
//! Looking at a program costs the member nothing it counts: a look spends none of the
//! changes a person may send a device. Telling does, and everything this server asks of one
//! member shares one allowance. So what a program is told is gathered and sent at most every
//! [`TELL_EVERY`], and a member that says to slow down is asked again later with the same
//! frames: each is numbered, so it is handed on once however often it is sent.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tilo_actions::remote::environment_programs::{
    self, Frames, ProgramEnded, ServeProgram, TellProgram,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use ush::environment_programs::{FromProgram, Heard, Over, Start, ToProgram, framed, unframed};

pub use crate::coding_agents::run::Machine;
use crate::placed_shells::{self, PlacedShells};

/// How long a start waits for a computer that can take the workspace's machine before it
/// gives up.
const PLACE_PATIENCE: Duration = Duration::from_secs(5 * 60);
/// How long one wait for the member to move is, so an end is noticed while the program is
/// followed.
const CHANGE_PATIENCE: Duration = Duration::from_millis(100);
/// How many such waits one look at the member is given.
const CHANGE_WAITS: u32 = 150;
/// How long the member is given to keep a cancel.
const CANCEL_PATIENCE: Duration = Duration::from_secs(10);
/// How long one telling is given to be answered.
const TELL_PATIENCE: Duration = Duration::from_secs(30);
/// After how long without an answer about it a program counts as lost.
const LOST_AFTER: Duration = Duration::from_secs(60);
/// The least time between two tellings of one program.
pub const TELL_EVERY: Duration = Duration::from_millis(400);
/// How long the member may wait to hear what was taken, when little was and nothing is told.
const TAKEN_EVERY: Duration = Duration::from_secs(3);
/// How long a telling waits after the member said to slow down, or did not answer.
const TELL_AGAIN: Duration = Duration::from_secs(1);
/// How many tellings in a row may go unanswered before the program counts as lost.
const TELL_TRIES: u32 = 30;
/// The most hex one telling carries, and always at least one frame.
const TOLD_AT_ONCE: usize = 256 * 1024;
/// How many frames taken are worth telling the member soon: it shows the next ones only
/// once it knows.
const MUCH_TAKEN: u64 = 16;
/// How much hex taken is worth telling the member soon.
const MUCH_HEX: u64 = 64 * 1024;

/// A member device, asked as this server's own device.
type Member = Arc<dyn ActionProvider + Send + Sync>;

/// What the two threads of one program share with whoever holds it.
struct Asking {
    member: Member,
    /// Who asks. A device elsewhere is asked as this server's own device, and this is only
    /// what the trait asks for.
    caller: Caller,
    /// The invocation that started the program: its name on the member.
    program: InvocationId,
    /// How many of the program's frames were taken here.
    taken: AtomicU64,
    /// How much hex those frames were.
    taken_hex: AtomicU64,
    /// It is over: nothing more is told to it.
    over: AtomicBool,
    /// Whoever held it let go of it: it is ended on the member.
    cut: AtomicBool,
}

/// What the telling thread is handed.
enum Told {
    Frame(ToProgram),
    /// Frames of the program were taken: the member may be owed a word about it.
    Taken,
}

/// How a telling was answered.
enum Answered {
    Told,
    /// The member is there and says it was asked too fast.
    SlowDown,
    /// Nothing came back.
    Silent,
    /// The program is not the member's any more.
    Gone,
}

/// Things handed from one thread to another, in order, and waited for.
struct Handed<T> {
    waiting: Mutex<VecDeque<T>>,
    came: Condvar,
}

impl<T> Handed<T> {
    fn new() -> Self {
        Self {
            waiting: Mutex::new(VecDeque::new()),
            came: Condvar::new(),
        }
    }

    fn hand(&self, one: T) {
        self.waiting.lock_recover().push_back(one);
        self.came.notify_all();
    }

    /// The next one, waited for at most `wait`.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "it stays locked from being found empty to being waited on, so nothing handed \
                  in falls between"
    )]
    fn next(&self, wait: Duration) -> Option<T> {
        let waiting = self.waiting.lock_recover();
        let (mut waiting, _) = self
            .came
            .wait_timeout_while(waiting, wait, |waiting| waiting.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
        waiting.pop_front()
    }
}

/// A program served on another computer's member device, as this server holds it.
pub struct Carried {
    asking: Arc<Asking>,
    heard: Arc<Handed<Heard>>,
    told: Arc<Handed<Told>>,
}

/// Start `start` in `workspace`'s machine on the computer the placer names for it, and hold
/// it from here.
///
/// # Errors
///
/// The placer or the member could not be asked, no computer could take the workspace's
/// machine in time, or the member refused.
pub fn serve_placed(
    placed: &PlacedShells,
    workspace: &str,
    start: Start,
) -> Result<Arc<Carried>, String> {
    let (action_service, endpoint) = placed.asking()?;
    let until = Instant::now().checked_add(PLACE_PATIENCE);
    let late = move || until.is_none_or(|until| Instant::now() >= until);
    let mut waited = None;
    let Some(holder) = placed.placed(workspace, &mut waited, Some(&late))? else {
        return Err(
            "No computer could take this workspace's machine for its coding agent in time. \
             Write to it again in a few minutes."
                .to_owned(),
        );
    };
    let member = placed.members().get(&holder).ok_or_else(|| {
        format!("the placer named {holder}, which holds no machines this server knows of")
    })?;
    let address = placed_shells::member_address(member)?;
    let device = action_service.remote(endpoint, &address, workspace);
    serve(
        Arc::new(device),
        Caller::local_app("tilo.server"),
        &ServeProgram {
            argv: start.argv,
            env: start.env,
            directory: start.directory.to_string_lossy().into_owned(),
            endpoints: start.endpoints,
            stop_grace_ms: u64::try_from(start.stop_grace.as_millis()).unwrap_or(u64::MAX),
            agent_program: start.agent_program,
            account: placed.account().to_owned(),
            size: placed.size().as_str().to_owned(),
        },
    )
}

/// Start `asked` on `member` and hold it: `caller` asks, as the program's own workspace.
///
/// # Errors
///
/// The member did not answer, or refused the start.
pub fn serve(member: Member, caller: Caller, asked: &ServeProgram) -> Result<Arc<Carried>, String> {
    let program = InvocationId::new();
    let request = request(environment_programs::serve_target(), asked, program)?;
    let receipt = placed_shells::settled(member.invoke(&caller, request))?
        .map_err(|error| refused_start(&error))?;
    if let ReceiptStatus::Rejected { error } = receipt.status {
        return Err(refused_start(&error));
    }
    let asking = Arc::new(Asking {
        member,
        caller,
        program,
        taken: AtomicU64::new(0),
        taken_hex: AtomicU64::new(0),
        over: AtomicBool::new(false),
        cut: AtomicBool::new(false),
    });
    let (heard, told) = (Arc::new(Handed::new()), Arc::new(Handed::new()));
    let (following, hearing, owed) = (Arc::clone(&asking), Arc::clone(&heard), Arc::clone(&told));
    let (handing, telling) = (Arc::clone(&asking), Arc::clone(&told));
    let started = std::thread::Builder::new()
        .name("placed-program-hear".into())
        .spawn(move || follow(&following, &hearing, &owed))
        .and_then(|_| {
            std::thread::Builder::new()
                .name("placed-program-tell".into())
                .spawn(move || tell(&handing, &telling))
        });
    if let Err(error) = started {
        asking.cut.store(true, Ordering::SeqCst);
        return Err(format!(
            "the coding agent's program could not be followed: {error}"
        ));
    }
    Ok(Arc::new(Carried {
        asking,
        heard,
        told,
    }))
}

/// What a person reads when the member would not start the program.
fn refused_start(error: &ActionCallError) -> String {
    format!(
        "the computer that holds this workspace's machine refused the coding agent's program: \
         {error}"
    )
}

/// One request of a member: `asked` at `target`.
fn request(
    target: RemoteActionTarget,
    asked: &impl serde::Serialize,
    invocation: InvocationId,
) -> Result<InvokeRequest, String> {
    let serde_json::Value::Object(argument) =
        serde_json::to_value(asked).map_err(|error| error.to_string())?
    else {
        return Err("what a member is asked is an object".to_owned());
    };
    Ok(InvokeRequest {
        invocation,
        target,
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    })
}

/// An end nobody on the member said: `words` is why.
fn lost(words: &str) -> Over {
    Over {
        exit: 1,
        refusal: None,
        words: words.to_owned(),
    }
}

/// Follow the program on the member until it is over: hand on each frame it sent once, in
/// order, and then how it ended.
fn follow(asking: &Asking, hearing: &Handed<Heard>, owed: &Handed<Told>) {
    let mut silent: Option<Instant> = None;
    let mut ended = false;
    let over = loop {
        if !ended && asking.cut.load(Ordering::SeqCst) {
            // Asked once; the program is then followed to the end the member gives it.
            let _ = asking
                .member
                .cancel(&asking.caller, &asking.program)
                .wait_timeout(CANCEL_PATIENCE);
            ended = true;
        }
        wait_for_a_change(asking, ended);
        let Some(state) = asking.member.observe(&asking.caller, &asking.program) else {
            if silent.get_or_insert_with(Instant::now).elapsed() >= LOST_AFTER {
                break lost(
                    "the computer that holds this workspace's machine no longer answers for its \
                     program",
                );
            }
            std::thread::sleep(CHANGE_PATIENCE);
            continue;
        };
        silent = None;
        if let Some(over) = shown(asking, hearing, owed, state.update) {
            break over;
        }
    };
    asking.over.store(true, Ordering::SeqCst);
    hearing.hand(Heard::Over(over));
}

/// Wait until the member says the program moved, or a moment passed; no longer than it
/// takes to notice that whoever held the program let go of it.
fn wait_for_a_change(asking: &Asking, ended: bool) {
    let Some(mut change) = asking.member.next_change(&asking.caller, &asking.program) else {
        std::thread::sleep(CHANGE_PATIENCE);
        return;
    };
    for _ in 0..CHANGE_WAITS {
        match change.wait_timeout(CHANGE_PATIENCE) {
            Ok(_) => return,
            Err(again) => {
                if !ended && asking.cut.load(Ordering::SeqCst) {
                    return;
                }
                change = again;
            }
        }
    }
}

/// Take what `update` shows of the program; how it ended, when it did.
fn shown(
    asking: &Asking,
    hearing: &Handed<Heard>,
    owed: &Handed<Told>,
    update: InvocationUpdate,
) -> Option<Over> {
    match update {
        InvocationUpdate::Item { value, .. } => {
            if let Ok(frames) = serde_json::from_value::<Frames>(value)
                && take(asking, hearing, &frames) > 0
            {
                owed.hand(Told::Taken);
            }
            None
        }
        InvocationUpdate::Succeeded { output, .. } => {
            let ended = output
                .value
                .and_then(|value| serde_json::from_value::<ProgramEnded>(value).ok());
            Some(match ended {
                Some(ended) => {
                    take(asking, hearing, &ended.last);
                    Over {
                        exit: ended.exit,
                        refusal: ended.refusal,
                        words: ended.words,
                    }
                }
                None => lost("the member that held this program did not say how it ended"),
            })
        }
        InvocationUpdate::Failed { error, .. } => Some(lost(&error.message)),
        InvocationUpdate::Cancelled { message, .. } => Some(Over {
            exit: 130,
            refusal: None,
            words: message.unwrap_or_default(),
        }),
        InvocationUpdate::Accepted { .. } | InvocationUpdate::Progress { .. } => None,
    }
}

/// Hand on the frames of `frames` that were not taken yet, in order; how many there were.
fn take(asking: &Asking, hearing: &Handed<Heard>, frames: &Frames) -> u64 {
    let before = asking.taken.load(Ordering::SeqCst);
    let mut taken = before;
    let mut hex = 0_u64;
    let mut number = frames.first;
    for text in &frames.frames {
        if number == taken {
            if let Some(frame) = unframed::<FromProgram>(text) {
                hearing.hand(Heard::Frame(frame));
            }
            taken = taken.saturating_add(1);
            hex = hex.saturating_add(u64::try_from(text.len()).unwrap_or(u64::MAX));
        }
        number = number.saturating_add(1);
    }
    asking.taken.store(taken, Ordering::SeqCst);
    asking.taken_hex.fetch_add(hex, Ordering::SeqCst);
    taken.saturating_sub(before)
}

/// What the telling thread keeps between two tellings.
#[derive(Default)]
struct Telling {
    /// How many frames the member was handed for the program.
    sent: u64,
    /// How many of the program's frames the member knows were taken, and how much hex.
    said_taken: u64,
    said_hex: u64,
    last: Option<Instant>,
    /// How many tellings in a row went unanswered.
    unanswered: u32,
}

impl Telling {
    /// Whether the member is told now: the program is there, the last telling is long enough
    /// ago, and there is something to say. A program is there once its first frame came,
    /// which says its launcher is up.
    fn due(&self, asking: &Asking, something_waits: bool) -> bool {
        let taken = asking.taken.load(Ordering::SeqCst);
        let since = self.last.map(|last| last.elapsed());
        if taken == 0 || since.is_some_and(|since| since < TELL_EVERY) {
            return false;
        }
        if something_waits {
            return true;
        }
        let owed = taken.saturating_sub(self.said_taken);
        let owed_hex = asking
            .taken_hex
            .load(Ordering::SeqCst)
            .saturating_sub(self.said_hex);
        owed > 0
            && (owed >= MUCH_TAKEN
                || owed_hex >= MUCH_HEX
                || since.is_none_or(|since| since >= TAKEN_EVERY))
    }
}

/// Hand the member what the program is told, gathered and in order, until the program is
/// over or whoever held it let go of it.
fn tell(asking: &Asking, told: &Handed<Told>) {
    let mut waiting: Vec<String> = Vec::new();
    let mut telling = Telling::default();
    loop {
        match told.next(CHANGE_PATIENCE) {
            Some(Told::Frame(frame)) => waiting.extend(framed(&frame)),
            Some(Told::Taken) | None => {}
        }
        if asking.over.load(Ordering::SeqCst) || asking.cut.load(Ordering::SeqCst) {
            return;
        }
        if telling.due(asking, !waiting.is_empty())
            && !told_some(asking, &mut telling, &mut waiting)
        {
            asking.cut.store(true, Ordering::SeqCst);
            return;
        }
    }
}

/// Tell the member once: the first of `waiting` that fit, and what was taken. `false` when
/// the program is lost to this server.
fn told_some(asking: &Asking, telling: &mut Telling, waiting: &mut Vec<String>) -> bool {
    let count = at_once(waiting);
    let taken = asking.taken.load(Ordering::SeqCst);
    let hex = asking.taken_hex.load(Ordering::SeqCst);
    let asked = TellProgram {
        program: asking.program,
        first: telling.sent,
        frames: waiting.iter().take(count).cloned().collect(),
        taken,
    };
    match told_once(asking, &asked) {
        Answered::Told => {
            telling.sent = telling
                .sent
                .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
            telling.said_taken = taken;
            telling.said_hex = hex;
            telling.last = Some(Instant::now());
            telling.unanswered = 0;
            *waiting = waiting.split_off(count);
            true
        }
        Answered::SlowDown => {
            std::thread::sleep(TELL_AGAIN);
            true
        }
        Answered::Silent => {
            telling.unanswered = telling.unanswered.saturating_add(1);
            std::thread::sleep(TELL_AGAIN);
            telling.unanswered < TELL_TRIES
        }
        Answered::Gone => false,
    }
}

/// How many of `waiting` one telling carries: what fits [`TOLD_AT_ONCE`], and at least one
/// when there is one.
fn at_once(waiting: &[String]) -> usize {
    let mut hex = 0_usize;
    let mut count = 0_usize;
    for frame in waiting {
        if count > 0 && hex.saturating_add(frame.len()) > TOLD_AT_ONCE {
            break;
        }
        hex = hex.saturating_add(frame.len());
        count = count.saturating_add(1);
    }
    count
}

fn told_once(asking: &Asking, asked: &TellProgram) -> Answered {
    let Ok(request) = request(
        environment_programs::tell_target(),
        asked,
        InvocationId::new(),
    ) else {
        return Answered::Gone;
    };
    match asking
        .member
        .invoke(&asking.caller, request)
        .wait_timeout(TELL_PATIENCE)
    {
        Ok(Some(Ok(receipt))) => match receipt.status {
            ReceiptStatus::Rejected { error } => refused(&error),
            _ => Answered::Told,
        },
        Ok(Some(Err(error))) => refused(&error),
        _ => Answered::Silent,
    }
}

/// What a refused telling means: asked too fast, a member that cannot be asked just now, or
/// a program that is not the member's any more.
const fn refused(error: &ActionCallError) -> Answered {
    match error {
        ActionCallError::Refused { .. } => Answered::SlowDown,
        ActionCallError::Unavailable { .. } => Answered::Silent,
        _ => Answered::Gone,
    }
}

impl Machine for Carried {
    fn tell(&self, frame: &ToProgram) -> bool {
        if self.asking.over.load(Ordering::SeqCst) {
            return false;
        }
        self.told.hand(Told::Frame(frame.clone()));
        true
    }

    fn hear(&self, wait: Duration) -> Option<Heard> {
        self.heard.next(wait)
    }

    fn cut(&self) {
        self.asking.cut.store(true, Ordering::SeqCst);
    }
}

impl Drop for Carried {
    fn drop(&mut self) {
        // A program nobody holds has no server: it ends with its handle.
        self.asking.cut.store(true, Ordering::SeqCst);
    }
}

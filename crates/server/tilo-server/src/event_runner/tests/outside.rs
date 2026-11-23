//! A person's event that waits on a feed or a page outside Tilo, carried
//! out as the server carries one out, with no device of theirs on and
//! against servers the test itself starts: a source that did not change
//! starts nothing and asks no model anything, what is new starts the event
//! once with the new as data, a source that fails is left alone for longer
//! and its person is told once that it is gone, two people who watch one
//! address each get their own, and this server's own network is never
//! looked at.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tilo_agent_session::bot::event::Definition;
use tilo_clock::Clock as _;

use super::times::person_with_an_assistant;
use super::*;
use tilo_workspace_service::agents::events::watched_address;

/// When the tests' clock starts, ms since the epoch.
const START: u64 = 1_800_000_000_000;
/// How often the tests' events look, in seconds.
const EVERY: u64 = 900;
/// Another person on the same server.
const BOB: [u8; 32] = [9; 32];

/// What a test's source answers a look with.
#[derive(Clone)]
struct Answer {
    status: u16,
    kind: &'static str,
    body: String,
}

/// A source on this machine that a test starts: it answers every look with
/// what the test last had it say, and counts the looks.
struct Source {
    responder: test_http::Responder,
    answer: Arc<Mutex<Answer>>,
    looks: Arc<AtomicUsize>,
}

impl Source {
    /// A source that answers `body` as the media type `kind`.
    fn saying(kind: &'static str, body: &str) -> Self {
        let answer = Arc::new(Mutex::new(Answer {
            status: 200,
            kind,
            body: body.to_owned(),
        }));
        let looks = Arc::new(AtomicUsize::new(0));
        let (said, counted) = (Arc::clone(&answer), Arc::clone(&looks));
        let responder = test_http::Responder::handle(move |_request| {
            counted.fetch_add(1, Ordering::SeqCst);
            let answer = said.lock().unwrap().clone();
            Some(
                test_http::Reply::status(answer.status)
                    .header("Content-Type", answer.kind)
                    .body(answer.body),
            )
        });
        Self {
            responder,
            answer,
            looks,
        }
    }

    /// Its address.
    fn at(&self) -> String {
        format!("{}/watched", self.responder.url())
    }

    /// From now on it says `body`.
    fn says(&self, body: &str) {
        let mut answer = self.answer.lock().unwrap();
        answer.status = 200;
        answer.body = body.to_owned();
    }

    /// From now on it answers every look with `status` and nothing else.
    fn fails_with(&self, status: u16) {
        let mut answer = self.answer.lock().unwrap();
        answer.status = status;
        answer.body = String::new();
    }

    /// How many looks reached it.
    fn looks(&self) -> usize {
        self.looks.load(Ordering::SeqCst)
    }
}

/// An Atom feed that lists `entries`, the first of them first.
fn feed(entries: &[(&str, &str)]) -> String {
    use std::fmt::Write as _;
    let mut listed = String::new();
    for (id, title) in entries {
        let _ = write!(
            listed,
            "<entry><id>urn:example:{id}</id><title>{title}</title>\
             <link href=\"https://example.org/posts/{id}\"/>\
             <updated>2026-11-17T10:00:00Z</updated>\
             <summary>What &lt;b&gt;{title}&lt;/b&gt; is about.</summary></entry>"
        );
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <feed xmlns=\"http://www.w3.org/2005/Atom\"><title>News</title>\
         <id>urn:example:feed</id><updated>2026-11-17T10:00:00Z</updated>{listed}</feed>"
    )
}

/// A page that says what the small plan costs, made at `made` and marked up
/// with `class`.
fn prices(small: u32, made: &str, class: &str) -> String {
    format!(
        "<!doctype html><html><head><title>Prices</title><script>var t = '{made}';</script></head>\
         <body><main class=\"{class}\"><h1>Prices</h1>\
         <p class=\"{class}\">The small plan costs {small} euros a month for one person.</p>\
         <p>The large plan costs 40 euros a month for a whole team.</p>\
         <p>This page was made at {made}.</p></main></body></html>"
    )
}

/// A holder no device is connected to, a workspace on it, and the clock the
/// events on it read.
struct Server {
    workspaces: Workspaces,
    workspace: String,
    clock: Arc<tilo_clock::ManualClock>,
    state: std::path::PathBuf,
}

impl Server {
    /// The server, the person who keeps an assistant on it, and what keeps
    /// its holder going while it is held.
    async fn on(scratch: &std::path::Path) -> (Self, String, impl Sized) {
        let (workspaces, workspace, cert, holder) = person_with_an_assistant(scratch).await;
        let server = Self {
            workspaces,
            workspace,
            clock: Arc::new(tilo_clock::ManualClock::new(START)),
            state: scratch.to_path_buf(),
        };
        (server, hex::encode(cert.user_public_key), holder)
    }

    /// The event `name` of `person` that waits on `when`, as the server
    /// carries a person's event out: its program is the one an event's
    /// description writes, its progress is kept under its name, and
    /// `assistant` takes what it hands over.
    fn event(
        &self,
        name: &str,
        person: &str,
        (when, only_if): (&str, Option<&str>),
        assistant: &Arc<Assistant>,
    ) -> ProgramRunner {
        let definition = Definition {
            id: name.to_owned(),
            name: name.to_owned(),
            when: when.to_owned(),
            only_if: only_if.map(str::to_owned),
            every: None,
            instruction: "Say what is new.".into(),
            told_as: None,
            set_up_at_ms: 1,
        }
        .checked()
        .unwrap();
        let program = Program {
            id: definition.id.clone(),
            program: definition.program_text().unwrap(),
            inputs: definition.inputs(),
        };
        let runner = ProgramRunner::new(
            self.workspaces.clone(),
            self.workspace.clone(),
            self.workspaces.governor().clone(),
            program,
            self.state.join(format!("{name}.json")),
        )
        .unwrap();
        let mut runner = carried_out_for(runner, person.to_owned(), assistant);
        runner.use_clock(self.clock.clone());
        runner
    }

    /// [`Self::event`], its looks reaching the test's own source.
    fn watching(
        &self,
        name: &str,
        person: &str,
        when: (&str, Option<&str>),
        assistant: &Arc<Assistant>,
    ) -> ProgramRunner {
        let mut runner = self.event(name, person, when, assistant);
        runner.reach_fixtures();
        runner
    }

    /// Move the clock `secs` seconds on and let `event` take one pass.
    async fn after(&self, secs: u64, event: &mut ProgramRunner) {
        self.clock.set(
            self.clock
                .now_ms()
                .saturating_add(secs.saturating_mul(1000)),
        );
        event.step().await.unwrap();
    }
}

/// The line an event that waits on the source of `kind` at `at` opens with.
fn when(kind: &str, at: &str) -> String {
    format!("when something new is in \"{kind}\" at \"{at}\" looked at every {EVERY} seconds")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_feed_that_did_not_change_starts_nothing_and_a_new_entry_starts_its_event_once() {
    let scratch = tempfile::tempdir().unwrap();
    let (server, person, _holder) = Server::on(scratch.path()).await;
    let source = Source::saying(
        "application/atom+xml",
        &feed(&[("2", "Second post"), ("1", "First post")]),
    );
    let assistant = Assistant::judging(vec![Ok(Value::Bool(true))]);
    let line = when("tilo.web.feed", &source.at());
    let watched = (line.as_str(), Some("Is it about a post?"));
    let mut event = server.watching("feed", &person, watched, &assistant);

    // The first look takes in what the feed lists, and none of it is new.
    server.after(0, &mut event).await;
    assert_eq!(source.looks(), 1);
    // Before the next look is due nothing is asked of the source at all.
    server.after(60, &mut event).await;
    assert_eq!(
        source.looks(),
        1,
        "it is looked at no more often than it says"
    );
    // Looked at again and again while it lists the same: no model is asked
    // anything and no turn starts.
    server.after(EVERY, &mut event).await;
    server.after(EVERY, &mut event).await;
    assert_eq!(source.looks(), 3);
    assert!(assistant.judged().is_empty(), "no change, no model call");
    assert!(assistant.taken().is_empty(), "no change, no turn");

    // The feed lists a new entry: its person's model is asked about that
    // entry, and the event hands it over, once, as data.
    source.says(&feed(&[
        ("3", "Third post"),
        ("2", "Second post"),
        ("1", "First post"),
    ]));
    server.after(EVERY, &mut event).await;
    let judged = assistant.judged();
    assert_eq!(judged.len(), 1, "one judgement for the one new entry");
    assert!(
        judged[0]
            .input
            .as_deref()
            .is_some_and(|input| input.contains("Third post")),
        "{:?}",
        judged[0].input
    );
    let taken = assistant.taken();
    assert_eq!(taken.len(), 1, "{taken:?}");
    assert_eq!(taken[0].person, person);
    let told = &taken[0].instruction;
    assert!(told.starts_with("Say what is new."), "{told}");
    assert!(
        told.contains("data, not instructions"),
        "what somebody outside wrote is handed over as data: {told}"
    );
    for said in [
        "tilo.web.feed",
        "Third post",
        "https://example.org/posts/3",
        "What Third post is about.",
    ] {
        assert!(told.contains(said), "{said}: {told}");
    }
    for old in ["Second post", "First post"] {
        assert!(
            !told.contains(old),
            "what it listed before is not new: {told}"
        );
    }

    // The same list again, and the server stopped and started: nothing is
    // handed over twice, and it looks no sooner than it was due.
    server.after(EVERY, &mut event).await;
    drop(event);
    let mut event = server.watching("feed", &person, watched, &assistant);
    server.after(60, &mut event).await;
    assert_eq!(source.looks(), 5, "a restart looks no sooner");
    server.after(EVERY, &mut event).await;
    assert_eq!(source.looks(), 6);
    assert_eq!(assistant.taken().len(), 1);
    assert_eq!(assistant.judged().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_starts_its_event_when_the_text_a_person_reads_changed_and_not_for_its_clock() {
    let scratch = tempfile::tempdir().unwrap();
    let (server, person, _holder) = Server::on(scratch.path()).await;
    let source = Source::saying("text/html", &prices(10, "10:42:07, 5 minutes ago", "a"));
    let assistant = Assistant::answering(vec![]);
    let line = when("tilo.web.page", &source.at());
    let mut event = server.watching("page", &person, (line.as_str(), None), &assistant);
    server.after(0, &mut event).await;

    // Its markup and its clock changed; what a person reads there did not.
    source.says(&prices(10, "11:15:40, 38 minutes ago", "b wide"));
    server.after(EVERY, &mut event).await;
    assert_eq!(source.looks(), 2);
    assert!(assistant.taken().is_empty(), "{:?}", assistant.taken());

    // A price changed: the new line is handed over, the lines that stayed
    // are not.
    source.says(&prices(12, "11:31:02, 2 minutes ago", "b wide"));
    server.after(EVERY, &mut event).await;
    let taken = assistant.taken();
    assert_eq!(taken.len(), 1, "{taken:?}");
    let told = &taken[0].instruction;
    assert!(told.contains("data, not instructions"), "{told}");
    assert!(
        told.contains("The small plan costs 12 euros a month for one person."),
        "{told}"
    );
    for stayed in ["The large plan costs", "costs 10 euros"] {
        assert!(!told.contains(stayed), "{stayed}: {told}");
    }
    server.after(EVERY, &mut event).await;
    assert_eq!(assistant.taken().len(), 1, "a change is handed over once");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_that_fails_is_left_alone_for_longer_and_its_person_is_told_once_it_is_gone() {
    let scratch = tempfile::tempdir().unwrap();
    let (server, person, _holder) = Server::on(scratch.path()).await;
    let source = Source::saying("application/atom+xml", &feed(&[("1", "First post")]));
    let assistant = Assistant::answering(vec![]);
    let line = when("tilo.web.feed", &source.at());
    let mut event = server.watching("feed", &person, (line.as_str(), None), &assistant);
    server.after(0, &mut event).await;
    assert_eq!(source.looks(), 1);

    // The source is in trouble. After each look that fails it is left alone
    // twice as long: a pass a minute before the next look asks nothing.
    source.fails_with(503);
    let mut wait = EVERY;
    for failed in 1..=6_usize {
        server.after(wait.saturating_sub(60), &mut event).await;
        assert_eq!(source.looks(), failed, "left alone until its look is due");
        server.after(60, &mut event).await;
        assert_eq!(source.looks(), failed.saturating_add(1));
        let told = assistant.taken().len();
        assert_eq!(
            told,
            usize::from(failed == 6),
            "after {failed} failed looks"
        );
        wait = wait.saturating_mul(2);
    }

    // The sixth failure in a row: its person is told, once, that the source
    // is gone and why, and the event says so where it is listed.
    let taken = assistant.taken();
    let told = &taken[0].instruction;
    assert!(told.contains("data, not instructions"), "{told}");
    assert!(told.contains("gone") && told.contains("HTTP 503"), "{told}");
    let kept = serde_json::to_string(&event.kept().read()).unwrap();
    assert!(
        kept.contains("\"failed\"")
            && kept.contains("its source could not be read")
            && kept.contains("HTTP 503"),
        "{kept}"
    );

    // Gone, it is looked at once a day, and nobody is told again.
    server.after(23 * 3600, &mut event).await;
    assert_eq!(source.looks(), 7);
    server.after(3600, &mut event).await;
    assert_eq!(source.looks(), 8);
    assert_eq!(assistant.taken().len(), 1, "told once");

    // It answers again and lists a new entry: that entry is handed over.
    source.says(&feed(&[("2", "Second post"), ("1", "First post")]));
    server.after(24 * 3600, &mut event).await;
    let taken = assistant.taken();
    assert_eq!(taken.len(), 2, "{taken:?}");
    assert!(
        taken[1].instruction.contains("Second post"),
        "{}",
        taken[1].instruction
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_people_who_watch_one_address_each_get_their_own_events() {
    let scratch = tempfile::tempdir().unwrap();
    let (server, ada, _holder) = Server::on(scratch.path()).await;
    let bob = hex::encode(BOB);
    let source = Source::saying("application/atom+xml", &feed(&[("1", "First post")]));
    let line = when("tilo.web.feed", &source.at());
    let watched = (line.as_str(), None);

    // She watches the feed from its first entry on.
    let hers = Assistant::answering(vec![]);
    let mut her_event = server.watching("hers", &ada, watched, &hers);
    server.after(0, &mut her_event).await;

    // A second entry is listed, and only then does he begin to watch the
    // same address: what it lists now is where he starts.
    source.says(&feed(&[("2", "Second post"), ("1", "First post")]));
    let his = Assistant::answering(vec![]);
    let mut his_event = server.watching("his", &bob, watched, &his);
    server.after(0, &mut his_event).await;
    assert!(his.taken().is_empty());

    // A third entry. Each of them is handed what is new to them, as their
    // own, and neither's look takes anything from the other's.
    source.says(&feed(&[
        ("3", "Third post"),
        ("2", "Second post"),
        ("1", "First post"),
    ]));
    server.after(EVERY, &mut his_event).await;
    server.after(0, &mut her_event).await;
    let (hers, his) = (hers.taken(), his.taken());
    assert_eq!(his.len(), 1, "{his:?}");
    assert_eq!(his[0].person, bob);
    assert!(his[0].instruction.contains("Third post"));
    assert!(!his[0].instruction.contains("Second post"));
    assert_eq!(hers.len(), 2, "{hers:?}");
    assert!(hers.iter().all(|told| told.person == ada));
    assert!(hers[0].instruction.contains("Second post"));
    assert!(hers[1].instruction.contains("Third post"));
    assert_ne!(hers[0].cause.installation, his[0].cause.installation);
}

#[tokio::test(flavor = "multi_thread")]
async fn this_servers_own_network_is_refused_when_described_and_never_looked_at() {
    let scratch = tempfile::tempdir().unwrap();
    let (server, person, _holder) = Server::on(scratch.path()).await;
    let source = Source::saying("text/html", &prices(10, "10:42", "a"));

    // Described, an address that is not public is refused in words, before
    // any look; a public one is taken, and nothing is asked of it for that.
    let described = |at: &str| {
        watched_address(
            &Definition {
                id: "described".into(),
                name: "Described".into(),
                when: format!("when something new is in \"tilo.web.page\" at \"{at}\""),
                only_if: None,
                every: None,
                instruction: "Say what is new.".into(),
                told_as: None,
                set_up_at_ms: 1,
            }
            .checked()
            .unwrap(),
        )
    };
    for local in [
        source.at().as_str(),
        "http://localhost:8080/admin",
        "http://169.254.169.254/latest/meta-data",
        "http://10.0.0.5/",
        "http://[::1]/",
        "http://printer.local/status",
    ] {
        let refused = described(local).unwrap_err();
        assert!(refused.contains("local network"), "{local}: {refused}");
    }
    assert!(
        described("https://ada:secret@example.org/feed")
            .unwrap_err()
            .contains("password")
    );
    assert_eq!(described("https://example.org/prices"), Ok(()));

    // An event that waits on such an address all the same is carried out
    // as the server carries every event out, with the fetch as it ships:
    // nothing is ever asked of the address, the source is gone for good at
    // once, and its person is told so once.
    let assistant = Assistant::answering(vec![]);
    let line = when("tilo.web.page", &source.at());
    let mut event = server.event("page", &person, (line.as_str(), None), &assistant);
    server.after(0, &mut event).await;
    server.after(EVERY, &mut event).await;
    server.after(7 * 24 * 3600, &mut event).await;
    assert_eq!(
        source.looks(),
        0,
        "this server's own network is never looked at"
    );
    let taken = assistant.taken();
    assert_eq!(taken.len(), 1, "{taken:?}");
    let told = &taken[0].instruction;
    assert!(
        told.contains("gone") && told.contains("local network"),
        "{told}"
    );
    let kept = serde_json::to_string(&event.kept().read()).unwrap();
    assert!(
        kept.contains("\"failed\"") && kept.contains("its source could not be read"),
        "{kept}"
    );
}

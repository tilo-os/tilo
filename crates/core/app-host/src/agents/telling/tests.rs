//! What a host tells its people outside Tilo, with no device of theirs on:
//! what is to reach a person is told once and the rest never; a device that
//! is looking is left to tell it; what was acknowledged or taken back is
//! not carried out; and a thing that must not be missed comes round again
//! until it is acknowledged.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "every test here keeps its host alive to the end"
)]

use tilo_person_services::person_telling::{Cause, Kind, Teller as ToldBy, WhyCode};

use super::*;
use tilo_person_services::{PersonServices, ToTell};
use tilo_workspace_service::workspaces::fixtures::{TestHost, hosting_device, test_host};

/// A person this host keeps a log for. No device of theirs is on.
const ADA: [u8; 32] = [8; 32];
/// The workspace her assistant works in.
const WORKSPACE: &str = "6f1f0f7e-3c1d-4a54-9b6e-2d1c5a7e9f10";
const SECOND: u64 = 1_000;

/// What was told outside Tilo, in order.
#[derive(Default)]
struct Heard {
    told: Mutex<Vec<([u8; 32], Told)>>,
}

impl Heard {
    /// The ids of the things told, in order.
    fn ids(&self) -> Vec<String> {
        self.told
            .lock()
            .unwrap()
            .iter()
            .map(|(_, told)| told.id.clone())
            .collect()
    }
}

impl Outside for Heard {
    fn name(&self) -> &'static str {
        "heard"
    }

    fn tell(&self, person: [u8; 32], told: Told) -> Pin<Box<dyn Future<Output = u32> + Send>> {
        self.told.lock().unwrap().push((person, told));
        Box::pin(async { 2 })
    }
}

/// A host with nobody's device on, and what it told outside.
struct Host {
    workspaces: Workspaces,
    heard: Arc<Heard>,
    _server: TestHost,
    _dir: tempfile::TempDir,
}

impl Host {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = hosting_device();
        let server = test_host(dir.path(), &key, &cert).await;
        server
            .workspaces()
            .create_workspace("Ada's", Some(WORKSPACE))
            .await
            .unwrap();
        Self {
            workspaces: server.workspaces().clone(),
            heard: Arc::default(),
            _server: server,
            _dir: dir,
        }
    }

    /// A teller that waits four seconds and counts a device as looking for
    /// eight, as a host's own does.
    fn teller(&self) -> Teller {
        Teller::new(self.workspaces.clone(), self.heard.clone())
    }

    /// What this host keeps for its people on its workspaces.
    fn services(&self) -> PersonServices {
        PersonServices::of(&self.workspaces)
    }
}

fn assistant() -> ToldBy {
    ToldBy {
        agent: "assistant".into(),
        name: "Assistant".into(),
    }
}

/// A reminder her assistant tells her.
fn reminder(words: &str, key: &str) -> ToTell {
    ToTell {
        kind: Kind::Reminder,
        words: words.into(),
        cause: Some(Cause {
            kind: "reminder".into(),
            label: words.into(),
            key: key.into(),
        }),
        from: Some(assistant()),
        ..ToTell::default()
    }
}

/// News her assistant tells her.
fn news(words: &str, key: &str) -> ToTell {
    ToTell {
        kind: Kind::News,
        words: words.into(),
        cause: Some(Cause {
            kind: "event".into(),
            label: "Mail arrives".into(),
            key: key.into(),
        }),
        from: Some(assistant()),
        ..ToTell::default()
    }
}

#[tokio::test]
async fn what_reaches_a_person_is_told_outside_once_and_news_never() {
    let host = Host::new().await;
    let teller = host.teller();
    let reminded = host
        .services()
        .tell(&ADA, reminder("Call Jo.", "r-1"))
        .unwrap();
    let unread = host
        .services()
        .tell(&ADA, news("A newsletter arrived.", "mail-2"))
        .unwrap();
    assert!(reminded.reaches);
    assert!(!unread.reaches);

    // Not at once: a device that is open hears of it first.
    let next = teller.step(reminded.at_ms.saturating_add(SECOND)).await;
    assert_eq!(next, Some(reminded.at_ms.saturating_add(4 * SECOND)));
    assert!(host.heard.ids().is_empty());

    // Four seconds on, with no device of hers looking: what reaches her is
    // told outside, once, and the news that reaches nobody is not.
    assert_eq!(
        teller.step(unread.at_ms.saturating_add(4 * SECOND)).await,
        None
    );
    assert_eq!(host.heard.ids(), std::slice::from_ref(&reminded.id));
    assert!(
        host.heard
            .told
            .lock()
            .unwrap()
            .iter()
            .all(|(person, _)| *person == ADA)
    );
    // That it was told is written down on the thing, which keeps its place.
    let log = host.services().told_after(&ADA, 0).told;
    let outside: Vec<(&str, u32)> = log
        .iter()
        .map(|told| {
            told.pushed
                .first()
                .map_or(("", 0), |pushed| (pushed.by.as_str(), pushed.took))
        })
        .collect();
    assert_eq!(outside, [("heard", 2), ("", 0)]);
    assert_eq!(log[0].mark.why.code, WhyCode::Reminder);

    // Never a second time, however long it waits unacknowledged.
    for later in [5 * SECOND, 60 * SECOND, 3_600 * SECOND] {
        assert_eq!(teller.step(unread.at_ms.saturating_add(later)).await, None);
    }
    assert_eq!(host.heard.ids().len(), 1);
}

#[tokio::test]
async fn a_person_whose_device_is_looking_is_not_told_outside_until_it_stops() {
    let host = Host::new().await;
    let lately = Duration::from_millis(200);
    let teller = host.teller().pacing(Pace {
        after: Duration::ZERO,
        watching: lately,
    });
    let reminded = host
        .services()
        .tell(&ADA, reminder("Call Jo.", "r-1"))
        .unwrap();

    // A device of hers reads her log: it shows the reminder itself, and
    // the teller looks again once that device may have stopped looking.
    let read = host.services().told_listed(&ADA, 0, None).await;
    assert_eq!(read.told, std::slice::from_ref(&reminded));
    let now = reminded.at_ms.saturating_add(SECOND);
    assert_eq!(teller.step(now).await, Some(now.saturating_add(200)));
    assert!(host.heard.ids().is_empty());

    // The device is gone and she did not acknowledge it: now it is told
    // outside.
    tokio::time::sleep(lately.saturating_add(Duration::from_millis(100))).await;
    assert_eq!(teller.step(now.saturating_add(SECOND)).await, None);
    assert_eq!(host.heard.ids(), [reminded.id]);
}

#[tokio::test]
async fn what_was_acknowledged_or_taken_back_is_not_told_outside() {
    let host = Host::new().await;
    let teller = host.teller();
    let seen = host
        .services()
        .tell(&ADA, reminder("Call Jo.", "r-1"))
        .unwrap();
    let gone = host
        .services()
        .tell(&ADA, reminder("Send the lease.", "r-2"))
        .unwrap();
    let waits = host
        .services()
        .tell(&ADA, reminder("Water the plants.", "r-3"))
        .unwrap();
    host.services()
        .acknowledge_told(&ADA, &seen.id, "Ada's phone", "dismissed")
        .unwrap();
    host.services().withdraw_told(&ADA, &gone.id).unwrap();

    assert_eq!(
        teller.step(waits.at_ms.saturating_add(4 * SECOND)).await,
        None
    );
    assert_eq!(host.heard.ids(), [waits.id]);
}

#[tokio::test]
async fn words_that_stay_on_the_host_do_not_leave_it() {
    let host = Host::new().await;
    let kept = host
        .services()
        .tell(
            &ADA,
            ToTell {
                words_stay: true,
                ..reminder("Sign the term sheet in the data room.", "r-1")
            },
        )
        .unwrap();
    let free = host
        .services()
        .tell(&ADA, reminder("Call Jo.", "r-2"))
        .unwrap();
    assert!(!kept.outside && free.outside);

    // What is told outside names who tells and says where to look, and
    // nothing of the words.
    assert_eq!(
        words_outside(&kept),
        ("Assistant".to_owned(), "Open Tilo to see it.".to_owned())
    );
    assert_eq!(
        words_outside(&free),
        ("Assistant".to_owned(), "Call Jo.".to_owned())
    );
    // A question says that somebody waits; a thing nobody is named for is
    // Tilo's.
    let question = Told {
        kind: Kind::Question,
        ..free.clone()
    };
    assert_eq!(words_outside(&question).0, "Assistant needs you");
    let nameless = Told { from: None, ..free };
    assert_eq!(words_outside(&nameless).0, "Tilo");
}

#[tokio::test]
async fn one_teller_tells_for_a_host() {
    let host = Host::new().await;
    assert!(host.services().claim_telling_outside());
    // A second one, started for the same workspaces, ends at once: nothing
    // is told outside twice.
    tokio::time::timeout(Duration::from_secs(5), host.teller().run())
        .await
        .expect("a second teller ends at once");
    assert!(!host.services().claim_telling_outside());
}

/// A reminder Ada said must not be missed: raised again every ten minutes,
/// six times at most.
fn must_not_be_missed(host: &Host) -> Told {
    let mut must = reminder("File the tax return.", "tax-1");
    must.insists = true;
    host.services().tell(&ADA, must).unwrap()
}

/// The default rhythm, in milliseconds: ten minutes.
const TEN_MINUTES: u64 = 600 * SECOND;

#[tokio::test]
async fn what_must_not_be_missed_is_told_again_until_it_is_acknowledged() {
    let host = Host::new().await;
    let teller = host.teller();
    let told = must_not_be_missed(&host);
    let began = told.at_ms;
    assert_eq!(
        told.insists
            .map(|insists| (insists.raised, insists.next_at_ms)),
        Some((1, Some(began.saturating_add(TEN_MINUTES))))
    );

    // Told outside the first time like anything that reaches her, and due
    // again in ten minutes.
    assert_eq!(
        teller.step(began.saturating_add(4 * SECOND)).await,
        Some(began.saturating_add(TEN_MINUTES))
    );
    assert_eq!(host.heard.ids().len(), 1);
    // Not before its time.
    assert_eq!(
        teller.step(began.saturating_add(30 * SECOND)).await,
        Some(began.saturating_add(TEN_MINUTES))
    );
    assert_eq!(host.heard.ids().len(), 1);

    // Ten minutes on she has not acknowledged it: it is raised again,
    // takes a new place so a device of hers that runs raises its notice
    // again, and is told outside again.
    assert_eq!(
        teller.step(began.saturating_add(TEN_MINUTES)).await,
        Some(began.saturating_add(2 * TEN_MINUTES))
    );
    assert_eq!(host.heard.ids(), [told.id.clone(), told.id.clone()]);
    let raised = host.services().told_after(&ADA, told.seq).told;
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].insists.map(|insists| insists.raised), Some(2));

    // She acknowledges it, and it stops.
    host.services()
        .acknowledge_told(&ADA, &told.id, "Ada's phone", "opened")
        .unwrap();
    for later in [2, 3, 60].map(|minutes| minutes * TEN_MINUTES) {
        assert_eq!(teller.step(began.saturating_add(later)).await, None);
    }
    assert_eq!(host.heard.ids().len(), 2);
}

#[tokio::test]
async fn what_insists_is_raised_its_six_times_and_no_more() {
    let host = Host::new().await;
    let teller = host.teller();
    let told = must_not_be_missed(&host);
    let began = told.at_ms;

    // Six times in all, the first telling included, and then no more,
    // though she never acknowledges it.
    teller.step(began.saturating_add(4 * SECOND)).await;
    for times in 1..6 {
        teller.step(began.saturating_add(times * TEN_MINUTES)).await;
    }
    assert_eq!(host.heard.ids().len(), 6);
    for later in [6, 7, 60] {
        assert_eq!(
            teller.step(began.saturating_add(later * TEN_MINUTES)).await,
            None
        );
    }
    assert_eq!(host.heard.ids().len(), 6);
    let last = host.services().told_after(&ADA, 0).told;
    assert_eq!(
        last[0]
            .insists
            .map(|insists| (insists.raised, insists.next_at_ms)),
        Some((6, None))
    );
    assert!(last[0].waits(), "it still waits for her in Tilo");
}

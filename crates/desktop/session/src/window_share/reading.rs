//! What the shares' resource reads: the shares as they stand, and how far the live session they
//! travel came on this device.
//!
//! A shared window's pictures cross the workspace's live session, and a live session that is not open says
//! nothing by itself: a share started under it captures nothing, and a window watched under it
//! receives nothing. So the reading names each workspace's live session (`live`: open, or what it waits
//! for and which device it waits to reach), and every window shared or watched in a workspace whose
//! live session is not open says what it waits for on its own row. A row that waits for nothing is one
//! whose pictures can flow. An open live session also says what crossed it: the epoch it seals under, how
//! many objects it opened, the ones it could not open and why, and which of this device's windows
//! another device asks it for. Two devices in one live session that hear nothing of each other read there
//! whether they seal under keys the other lacks. A device in a live session on keys its host lent it
//! (`lent_keys`) sends what it shares and opens nothing: a window it watches says so. A live session reads
//! so many devices at once: a device that is in the live session beyond those is absent from presence, a
//! call and a shared window on this device without a word, so an open live session says how many those
//! are, why by the word a tool branches on, and in the person's words (`unheard`, `unheard_why`,
//! `not_heard`).
//!
//! An open live session and a stream's start still put nothing on a screen, so the rows say what crossed.
//! A window this device shows is `pictured` once its tile holds a decoded picture or its page's
//! whole frame, and until then its row waits for the first one. A window this device shares is
//! `streaming` while a member's shell asks for it and it goes out: a captured window as soon as it
//! is asked for, a window whose app draws its page once the app handed a whole page over. One that
//! a member shows and nothing of which went out says so (`waiting_for`), and says which link it
//! waits on: the page's way out, or, once its app was asked for a whole page and let
//! [`crate::page_track::HAND_OVER_BOUND`] pass, the app that has not handed it over. The device
//! showing the window says the same from its side: it waits for the page's first frame, and once
//! that is overdue, that the device running the window was asked for it and sent none.
//!
//! A window goes out as its page or as pictures of it, and the reading says which (`as_page`). A
//! window whose app draws its page is never the second: a share of it that was asked for before
//! its app offered the page stands among the ones that wait (`starting`), says that it waits for
//! the page (`awaits_page`, and in words in `waiting_for`), and sends nothing meanwhile
//! ([`super::paged`]).
//!
//! A window that runs only on its device stands on every other shell whether or not it is shared,
//! and its tab there says why it holds no picture. The reading says the same of each (`hosted`):
//! `shown` while a share of it stands in the tab, and until then `not-shared`, `awaited` with what
//! it waits for, `away` for a device nothing is heard of, or `left` for one the workspace no
//! longer admits ([`super::hosts`]).
//!
//! A relay tells the device it turns away nothing but that. So a live session that was refused says which
//! epoch's keys this device presented (`presented`), and the live session of the device that relays the
//! workspace lists the endpoints it turned away and has not let in since, each with why
//! (`turned_away`, as its daemon publishes them: [`tilo_live::link::TURNED_AWAY`]): the two
//! readings side by side say what the refused device alone cannot.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Value, json};
use tilo_desktop_model::TabId;
use tilo_live::live_session::{LiveSession, MAX_HEARD};
use tilo_session_core::desktop::i18n::{t, t_args};

use super::WindowShares;
use crate::live::{LiveState, Relayer};
use crate::sync::Client;

impl WindowShares {
    /// Read how far each workspace's live session came, and the live sessions that opened, once either
    /// moved: nothing is read on a pass that has nothing new to find.
    pub(super) fn follow_live(&mut self, sync: &Client) {
        let seen = sync.live_revision();
        if seen == self.live_seen {
            return;
        }
        self.live_seen = seen;
        self.live = sync.live_states();
        self.turned = sync.live_turned_away();
        // Held weakly: a live session that ended is its session's to drop.
        self.live_sessions = crate::calls::live_sessions_of(sync)
            .into_iter()
            .map(|(workspace, live_session)| (workspace, Arc::downgrade(&live_session)))
            .collect();
        self.changed();
    }

    /// Note which shared windows go out to a member's shell now: the ones captured for a shell that
    /// asks for them, and of the ones whose app draws its page (`pages`) those whose page was
    /// published since a shell asked for it. A page whose app was asked for a whole frame and has
    /// not handed one over in time goes out to nobody new, and is noted as withheld by its app.
    pub(super) fn follow_streams(
        &mut self,
        pages: &crate::page_track::PageTracks,
        now: std::time::Instant,
    ) {
        let sent = pages.sent();
        let withheld: BTreeSet<TabId> = pages
            .withheld()
            .into_iter()
            .filter(|tab| self.pages.contains_key(tab))
            .collect();
        let oversized: BTreeSet<TabId> = pages
            .oversized()
            .into_iter()
            .filter(|tab| self.pages.contains_key(tab))
            .collect();
        let streaming: BTreeSet<TabId> = self
            .hosted
            .iter()
            .filter(|(_, hosted)| hosted.shown)
            .map(|(tab, _)| *tab)
            .chain(
                self.pages
                    .keys()
                    .copied()
                    .filter(|tab| sent.contains(tab) && !withheld.contains(tab)),
            )
            .collect();
        if streaming != self.streaming || withheld != self.withheld || oversized != self.oversized {
            self.streaming = streaming;
            self.withheld = withheld;
            self.oversized = oversized;
            self.changed();
        }
        // The pass that finds an app overdue is one nothing else may ask for.
        let now_us = tilo_clock::unix_now_ms().saturating_mul(1000);
        self.page_due = pages.due_in(now_us).and_then(|wait| now.checked_add(wait));
    }

    /// What the pictures of a window shared or watched in `workspace` wait for; nothing while the
    /// workspace's live session is open on this device.
    fn waiting_for(&self, workspace: &str) -> String {
        waiting_for(self.live.get(workspace))
    }

    /// What the pictures of a window watched in `workspace` wait for: what its live session waits for,
    /// and in a live session this device is in on lent keys, that it opens nothing another device sends.
    pub(super) fn watching_waits_for(&self, workspace: &str) -> String {
        match self.live.get(workspace) {
            Some(LiveState::Open { lent: true, .. }) => t("session-window-share-live-lent"),
            state => waiting_for(state),
        }
    }

    /// What the window `stream` brings waits for: what its workspace's live session waits for, and under
    /// an open live session its first page or picture, until its tile holds one.
    pub(super) fn received_waits_for(&self, stream: &super::Received) -> String {
        let live_session = self
            .watching
            .iter()
            .find(|((_, tab), watched)| *tab == stream.tab && watched.host == stream.host)
            .map(|((workspace, _), _)| self.watching_waits_for(workspace))
            .unwrap_or_default();
        if !live_session.is_empty() || stream.pictured {
            return live_session;
        }
        if stream.page && stream.awaited == super::page::Awaited::Overdue {
            t("session-window-share-page-overdue")
        } else if stream.page {
            t("session-window-share-first-page")
        } else {
            t("session-window-share-first-picture")
        }
    }

    /// The windows this session shares, as the reading says them: the ones it captures, and the
    /// ones whose page it publishes.
    fn sharing_rows(&self) -> Vec<Value> {
        let sharing: Vec<Value> = self
            .hosted
            .iter()
            .map(|(tab, hosted)| {
                let viewers: Vec<Value> = self
                    .viewers(*tab)
                    .into_iter()
                    .map(|peer| {
                        json!({
                            "peer": peer,
                            "can_control": hosted.open || hosted.controlled.contains(&peer),
                        })
                    })
                    .collect();
                json!({
                    "tab": tab.to_string(),
                    "window": hosted.window,
                    "app_id": hosted.app_id,
                    // Kept as it is: the views that read this know the key.
                    "workspace": hosted.workspace,
                    "input_allowed": viewers.iter().any(|viewer| {
                        viewer["can_control"].as_bool().unwrap_or(false)
                    }),
                    "viewers": viewers,
                    // Whether a member's shell asks the live session for the window now, so it is
                    // captured and sent.
                    "streaming": self.goes_out(*tab),
                    // Pictures of the window are what goes out.
                    "as_page": false,
                    "waiting_for": self.waiting_for(&hosted.workspace),
                })
            })
            .collect();
        {
            let mut sharing = sharing;
            for (tab, page) in &self.pages {
                let viewers: Vec<Value> = self
                    .viewers(*tab)
                    .into_iter()
                    .map(|peer| {
                        json!({
                            "peer": peer,
                            "can_control": false,
                        })
                    })
                    .collect();
                // An announcement is no page: the window goes out once its app handed a whole
                // page over for a shell that asks for it. One a member shows and nothing of which
                // went out waits for that, one whose app was asked and did not answer in time
                // waits for its app, and one whose page is larger than the live session carries says so.
                let streaming = self.streaming.contains(tab);
                let live_session = self.waiting_for(&page.workspace);
                let waiting_for =
                    if !live_session.is_empty() || streaming || page.announced.is_empty() {
                        live_session
                    } else if self.oversized.contains(tab) {
                        t("session-window-share-page-too-large")
                    } else if self.withheld.contains(tab) {
                        t("session-window-share-page-withheld")
                    } else {
                        t("session-window-share-page-unsent")
                    };
                sharing.push(json!({
                    "tab": tab.to_string(),
                    "window": page.window,
                    "app_id": page.app_id,
                    "workspace": page.workspace,
                    "input_allowed": false,
                    "viewers": viewers,
                    "streaming": streaming,
                    // The page its app draws is what goes out, and no picture of the window.
                    "as_page": true,
                    "waiting_for": waiting_for,
                }));
            }
            sharing
        }
    }

    /// The resource's reading now.
    pub(crate) fn reading(&self) -> Value {
        let sharing = self.sharing_rows();
        let starting: Vec<Value> = self
            .starting
            .iter()
            .map(|start| {
                json!({
                    "tab": start.tab.to_string(),
                    "window": start.window,
                    "app_id": start.app_id,
                    "workspace": start.workspace,
                    // Whether it waits for its app to offer the window's page: such a window
                    // is not shared as pictures meanwhile.
                    "awaits_page": start.page,
                    "waiting_for": start.waiting_for,
                })
            })
            .collect();
        let watching: Vec<Value> = self
            .watching
            .iter()
            .map(|((workspace, tab), watched)| {
                json!({
                    "tab": tab.to_string(),
                    "workspace": workspace,
                    "host": watched.host.to_string(),
                    // The tab of this session that pictures it, which is the sharer's own where this
                    // shell holds no view of its own of the window.
                    "shown_in": self.pictured_in(*tab).to_string(),
                    "waiting_for": self.watching_waits_for(workspace),
                })
            })
            .collect();
        let received: Vec<Value> = self
            .received
            .iter()
            .map(|((from, surface), stream)| {
                json!({
                    "tab": stream.tab.to_string(),
                    "surface": surface,
                    "from": from,
                    "app_id": stream.start.app_id,
                    "width": stream.start.width,
                    "height": stream.start.height,
                    "codec": stream.start.codec,
                    // Whether the tile holds the window: a start alone pictures nothing.
                    "pictured": stream.pictured,
                    "waiting_for": self.received_waits_for(stream),
                })
            })
            .collect();
        let live: Vec<Value> = self
            .live
            .iter()
            .map(|(workspace, state)| {
                let live_session = self
                    .live_sessions
                    .get(workspace)
                    .and_then(std::sync::Weak::upgrade);
                let turned = self.turned.get(workspace).map_or(&[][..], Vec::as_slice);
                live_row(workspace, state, live_session.as_deref(), turned)
            })
            .collect();
        json!({
            "sharing": sharing,
            "starting": starting,
            "watching": watching,
            "received": received,
            "hosted": self.hosted_rows(),
            "live": live,
        })
    }
}

/// The word the reading gives a live session's state: what a tool branches on, the same in every
/// language.
const fn state_of(state: Option<&LiveState>) -> &'static str {
    match state {
        Some(LiveState::Open { .. }) => "open",
        Some(LiveState::WaitingForKeys) => "waiting-for-keys",
        Some(LiveState::Keyless) => "no-keys",
        Some(LiveState::Unread) => "membership-unread",
        Some(LiveState::NoRelay) => "no-relay",
        Some(LiveState::Unreached { .. }) => "relay-unreached",
        Some(LiveState::Busy { .. }) => "relay-busy",
        Some(LiveState::Refused { .. }) => "refused",
        None => "absent",
    }
}

/// What a relay is called where a person reads it: the name its person gave it, and for a device
/// they never named the start of its key, which is what tells it apart.
fn called(relay: &Relayer) -> String {
    if relay.name.is_empty() {
        hex::encode(relay.device).chars().take(8).collect()
    } else {
        relay.name.clone()
    }
}

/// What a workspace whose live session stands at `state` waits for, in the person's words; nothing
/// while it is open. `None` is a workspace this session runs no live session for.
fn waiting_for(state: Option<&LiveState>) -> String {
    match state {
        Some(LiveState::Open { .. }) => String::new(),
        Some(LiveState::WaitingForKeys) => t("session-window-share-live-keys"),
        Some(LiveState::Keyless) => t("session-window-share-live-keyless"),
        Some(LiveState::Unread) => t("session-window-share-live-unread"),
        Some(LiveState::NoRelay) => t("session-window-share-live-no-carrier"),
        Some(LiveState::Unreached { relay }) => t_args(
            "session-window-share-live-unreached",
            &[("device", called(relay).into())],
        ),
        Some(LiveState::Busy { relay }) => t_args(
            "session-window-share-live-busy",
            &[("device", called(relay).into())],
        ),
        Some(LiveState::Refused { relay, .. }) => t_args(
            "session-window-share-live-refused",
            &[("device", called(relay).into())],
        ),
        None => t("session-window-share-live-absent"),
    }
}

/// Why an open live session does not hear `unheard` devices that are in it, in the word a tool branches
/// on: they are beyond what a live session reads at once. Nothing while it hears every device in it.
const fn unheard_why(unheard: usize) -> &'static str {
    if unheard > 0 { "reading-bound" } else { "" }
}

/// What a live session that does not hear `unheard` of the devices in it says of them, in the person's
/// words: how many, and why. Nothing while it hears them all.
fn not_heard(unheard: usize) -> String {
    if unheard == 0 {
        return String::new();
    }
    t_args(
        "session-window-share-live-unheard-bound",
        &[("count", unheard.into()), ("bound", MAX_HEARD.into())],
    )
}

/// One workspace's live session as the reading says it: its state, the device that relays it or that it
/// waits to reach, and what it waits for; and of `live_session`, the one open here, the epoch it seals
/// under, whether it does so on keys its host lent it, how many objects of other devices it
/// opened, the ones it could not open by reason, how many devices that are in the live session it does
/// not hear and why, and the tabs of this device's windows another
/// device asks it for, by their pictures or by their page. A live session its relay refused says the
/// epoch whose keys this device presented, and `turned` are the endpoints this device's own relay
/// turned away from the workspace's live session, each with why.
fn live_row(
    workspace: &str,
    state: &LiveState,
    live_session: Option<&LiveSession>,
    turned: &[Value],
) -> Value {
    let relay = match state {
        LiveState::Open { relay, .. }
        | LiveState::Unreached { relay }
        | LiveState::Busy { relay }
        | LiveState::Refused { relay, .. } => Some(relay),
        _ => None,
    };
    let unheard = live_session.map_or(0, LiveSession::unheard);
    let epoch = match state {
        LiveState::Open { epoch, .. } => Some(*epoch),
        _ => None,
    };
    let presented = match state {
        LiveState::Refused { epoch, .. } => Some(*epoch),
        _ => None,
    };
    let asked_for: BTreeSet<TabId> = live_session
        .map(|live_session| {
            live_session
                .wanted_windows()
                .into_iter()
                .chain(live_session.wanted_pages())
                .collect()
        })
        .unwrap_or_default();
    let asked_for: Vec<String> = asked_for.iter().map(ToString::to_string).collect();
    json!({
        "workspace": workspace,
        "state": state_of(Some(state)),
        "relay": relay.map(|relay| hex::encode(relay.device)).unwrap_or_default(),
        "relay_name": relay.map(|relay| relay.name.clone()).unwrap_or_default(),
        "relay_here": relay.is_some_and(|relay| relay.here),
        "waiting_for": waiting_for(Some(state)),
        "epoch": epoch,
        "lent_keys": matches!(state, LiveState::Open { lent: true, .. }),
        "presented": presented,
        "turned_away": turned,
        "heard": live_session.map_or(0, LiveSession::heard_total),
        "refused": live_session.map(LiveSession::refusals).unwrap_or_default(),
        "asked_for": asked_for,
        "unheard": unheard,
        "unheard_why": unheard_why(unheard),
        "not_heard": not_heard(unheard),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

    fn relay(name: &str, here: bool) -> Relayer {
        Relayer {
            device: [0xab; 32],
            name: name.to_owned(),
            here,
        }
    }

    tilo_test_env::scenario! {
        fn a_live_sessions_state_reads_in_a_word_and_in_the_persons_words;

        phase an_open_live_session_waits_for_nothing_and_names_its_relay;
        // An open live session waits for nothing, and says which device carries it.
        {
            let row = live_row(
                WORKSPACE,
                &LiveState::Open {
                    relay: relay("Ada's desktop", true),
                    epoch: 3,
                    lent: false,
                },
                None,
                &[],
            );
            assert_eq!(row["epoch"], json!(3));
            assert_eq!(row["lent_keys"], json!(false));
            assert_eq!(row["presented"], Value::Null);
            assert_eq!(row["turned_away"], json!([]));
            assert_eq!(row["workspace"], json!(WORKSPACE));
            assert_eq!(row["state"], json!("open"));
            assert_eq!(row["waiting_for"], json!(""));
            assert_eq!(row["relay"], json!(hex::encode([0xab; 32])));
            assert_eq!(row["relay_name"], json!("Ada's desktop"));
            assert_eq!(row["relay_here"], json!(true));
        }

        phase a_live_session_on_lent_keys_says_so;
        // A device in a live session on keys its host lent it says so: it sends what it shares and opens
        // nothing, so that it hears nobody there is no fault, and what it shares waits for nothing.
        {
            let lent = LiveState::Open {
                relay: relay("Ada's desktop", false),
                epoch: 4,
                lent: true,
            };
            let row = live_row(WORKSPACE, &lent, None, &[]);
            assert_eq!(row["state"], json!("open"));
            assert_eq!(row["lent_keys"], json!(true));
            assert_eq!(row["waiting_for"], json!(""));
        }

        phase a_live_session_that_did_not_reach_its_relay_names_the_device_it_waits_for;
        // A live session that waits for the device relaying the workspace names that device, by the name its
        // person gave it or, for one they never named, by the start of its key; one that was refused
        // says so in other words than one that merely waits.
        {
            let named = LiveState::Unreached {
                relay: relay("Ada's desktop", false),
            };
            let row = live_row(WORKSPACE, &named, None, &[]);
            assert_eq!(
                row["epoch"],
                Value::Null,
                "a live session that is not open seals nothing"
            );
            assert_eq!(row["state"], json!("relay-unreached"));
            assert_eq!(row["relay_here"], json!(false));
            let waits = row["waiting_for"].as_str().expect("what it waits for");
            assert!(waits.contains("Ada's desktop"), "{waits}");

            let unnamed = waiting_for(Some(&LiveState::Unreached {
                relay: relay("", false),
            }));
            assert!(unnamed.contains("abababab"), "{unnamed}");

            let refused = LiveState::Refused {
                relay: relay("Ada's desktop", false),
                epoch: 3,
            };
            let row = live_row(WORKSPACE, &refused, None, &[]);
            assert_eq!(row["state"], json!("refused"));
            assert_eq!(
                row["presented"],
                json!(3),
                "a refused live session says whose epoch's keys it presented"
            );
            assert_eq!(row["epoch"], Value::Null);
            let refusal = row["waiting_for"].as_str().expect("what it waits for");
            assert!(refusal.contains("Ada's desktop"), "{refusal}");
            assert_ne!(refusal, waits);
        }

        phase a_live_session_whose_relay_was_busy_says_so_in_its_own_words;
        // A live session whose relay was admitting as many devices as it takes says that, by the relay's
        // name: it is a state and words of its own, apart from a relay that was not reached and from
        // one that refused this device, in English and in German.
        {
            let busy = LiveState::Busy {
                relay: relay("Ada's phone", false),
            };
            let row = live_row(WORKSPACE, &busy, None, &[]);
            assert_eq!(row["state"], json!("relay-busy"));
            assert_eq!(row["relay"], json!(hex::encode([0xab; 32])));
            assert_eq!(row["relay_name"], json!("Ada's phone"));
            assert_eq!(row["epoch"], Value::Null);
            assert_eq!(
                row["presented"],
                Value::Null,
                "a busy relay judged nothing this device presented"
            );
            let says = row["waiting_for"].as_str().expect("what it waits for");
            assert_eq!(
                says,
                "Ada's phone, the device that carries this workspace's calls and shared windows, is \
             taking in as many devices as it can at once; this device asks it again shortly"
            );
            let unreached = waiting_for(Some(&LiveState::Unreached {
                relay: relay("Ada's phone", false),
            }));
            let refused = waiting_for(Some(&LiveState::Refused {
                relay: relay("Ada's phone", false),
                epoch: 3,
            }));
            assert_ne!(says, unreached);
            assert_ne!(says, refused);

            ui::i18n::I18nManager::set_locale("de-DE");
            let german = waiting_for(Some(&busy));
            ui::i18n::I18nManager::set_locale("en-US");
            assert_eq!(
                german,
                "Ada's phone, das Gerät, das die Anrufe und geteilten Fenster dieses Arbeitsbereichs \
             überträgt, nimmt gerade so viele Geräte auf, wie es auf einmal kann; dieses Gerät \
             fragt gleich noch einmal"
            );
        }

        phase the_devices_a_live_session_does_not_hear_are_counted_and_said_in_words;
        // A live session that hears every device in it says nothing of the rest; one that does not says
        // how many it does not hear and why, by the word a tool branches on and in the person's
        // words, in English and in German.
        {
            assert_eq!(unheard_why(0), "");
            assert_eq!(not_heard(0), "");
            assert_eq!(unheard_why(3), "reading-bound");
            assert_eq!(
                not_heard(3),
                "3 more devices are in this workspace's calls and shared windows and are not heard \
             here: this device hears 32 others at once"
            );
            assert_eq!(
                not_heard(1),
                "One more device is in this workspace's calls and shared windows and is not heard \
             here: this device hears 32 others at once"
            );

            ui::i18n::I18nManager::set_locale("de-DE");
            let german = (not_heard(3), not_heard(1));
            ui::i18n::I18nManager::set_locale("en-US");
            assert_eq!(
                german.0,
                "3 weitere Geräte sind in den Anrufen und geteilten Fenstern dieses Arbeitsbereichs \
             und werden hier nicht gehört: Dieses Gerät hört 32 andere gleichzeitig"
            );
            assert_eq!(
                german.1,
                "Ein weiteres Gerät ist in den Anrufen und geteilten Fenstern dieses Arbeitsbereichs \
             und wird hier nicht gehört: Dieses Gerät hört 32 andere gleichzeitig"
            );
        }

        phase every_live_session_that_is_not_open_says_what_it_waits_for;
        // Every state but an open one says something, each its own, and a workspace no live session runs
        // for says so too: a row never reads as flowing by saying nothing.
        {
            let said: Vec<(&str, String)> = [
                Some(LiveState::WaitingForKeys),
                Some(LiveState::Keyless),
                Some(LiveState::Unread),
                Some(LiveState::NoRelay),
                None,
            ]
            .iter()
            .map(|state| (state_of(state.as_ref()), waiting_for(state.as_ref())))
            .collect();
            let states: BTreeSet<&str> = said.iter().map(|(state, _)| *state).collect();
            let words: BTreeSet<&str> = said.iter().map(|(_, words)| words.as_str()).collect();
            assert_eq!(states.len(), said.len(), "{said:?}");
            assert_eq!(words.len(), said.len(), "{said:?}");
            assert!(
                said.iter()
                    .all(|(_, words)| !words.trim().is_empty()
                        && !words.starts_with("session-window-share")),
                "{said:?}"
            );
        }
    }

    /// The reading of an open live session says what the live session itself says of the devices it does not
    /// hear: with three devices more in a workspace than a live session reads, this device's row counts
    /// three, by why, and says so, while the live session is open and waits for nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_open_live_session_with_more_devices_than_it_reads_says_how_many_it_does_not_hear() {
        use tilo_live::relay::{Admission, Denied, Peer, Relay, RelayAuthority};

        /// A relay with no one to admit: a local live session is its own relay's member.
        struct Nobody;
        impl RelayAuthority for Nobody {
            fn admit<'a>(&'a self, _peer: Peer, _proof: &'a [u8]) -> Admission<'a> {
                Box::pin(std::future::ready(Err(Denied)))
            }
        }
        let carrying = Relay::new(Arc::new(Nobody));
        let workspace = collab_types::WorkspaceId::parse(WORKSPACE).expect("a workspace id");
        let keys = key_group::KeyGroup::create(
            workspace,
            key_group::MemberKey::generate("owner").expect("a member key"),
        )
        .expect("the workspace's key group")
        .content_keys()
        .expect("the workspace's keys");
        let beyond = 3;
        let devices: Vec<[u8; 32]> = (1..=u8::try_from(MAX_HEARD + 1 + beyond).expect("few"))
            .map(|device| [device; 32])
            .collect();
        let live_sessions: Vec<LiveSession> = devices
            .iter()
            .map(|device| {
                LiveSession::local(
                    &carrying,
                    &tilo_live::live_session::Join {
                        scope: collab_types::scope::Scope::workspace(WORKSPACE.to_owned()),
                        device: *device,
                        keys: keys.clone().into(),
                        roster: tilo_live::roster::Roster::new(devices.iter().copied()),
                        publish: true,
                        presence: true,
                    },
                )
                .expect("a local live session")
            })
            .collect();
        let here = &live_sessions[0];
        let deadline = tilo_test_env::hang_deadline();
        while here.unheard() != beyond {
            assert!(
                std::time::Instant::now() < deadline,
                "the live session counts the devices beyond what it reads: {:?}",
                here.unheard()
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let open = LiveState::Open {
            relay: relay("Ada's desktop", true),
            epoch: 1,
            lent: false,
        };
        let row = live_row(WORKSPACE, &open, Some(here), &[]);
        assert_eq!(row["state"], json!("open"));
        assert_eq!(row["waiting_for"], json!(""));
        assert_eq!(row["unheard"], json!(3));
        assert_eq!(row["unheard_why"], json!("reading-bound"));
        assert_eq!(row["not_heard"], json!(not_heard(3)));
        assert!(
            row["not_heard"]
                .as_str()
                .is_some_and(|says| says.starts_with("3 more devices")),
            "{row}"
        );
        // A live session this session does not hold open says nothing of whom it does not hear.
        let row = live_row(WORKSPACE, &open, None, &[]);
        assert_eq!(row["unheard"], json!(0));
        assert_eq!(row["unheard_why"], json!(""));
        assert_eq!(row["not_heard"], json!(""));
    }
}

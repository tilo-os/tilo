//! Telling this page's person when someone mentions them, wherever in the
//! workspace: every record of the room the page holds is looked at as a
//! peer's change reaches it, open in a window or not
//! ([`ui::mentions::in_record`]), and each new mention of the person is one
//! notice through the host's notifications. Choosing it opens the place
//! the mention is in, as any open goes: through the desktop's session, as
//! this person.
//!
//! What was told is kept in the page's storage, per room and person, so a
//! reload or a record arriving again tells nothing twice; what the room
//! held before this device first followed one of its records is taken as
//! seen.

use std::sync::{Arc, Mutex, PoisonError};

use tilo_actions::remote::{ActionHost as _, InvokeArgument, InvokeRequest};
use ui::mentions::{Look, MentionInbox, Place, SCHEME};

use super::Desktop;

/// Least time between two looks, in milliseconds: changes arriving in a
/// burst (a replay, someone typing) are looked at together.
const LOOK_EVERY_MS: f64 = 750.0;

/// One person's mentions on this page.
pub(super) struct Mentions {
    inbox: MentionInbox,
    /// Where what was told is kept.
    key: String,
    last_ms: f64,
    /// Records reached since the last look, each with whether every change
    /// that reached it came before the room had replayed what it held.
    pending: std::collections::BTreeMap<(String, String), bool>,
    /// Places the person chose from a notice, for the next frame.
    opens: Arc<Mutex<Vec<Place>>>,
}

impl Mentions {
    pub(super) fn new(room: &str, author: &str) -> Self {
        let key = format!("tilo.mentions.{room}.{author}");
        let mut inbox = MentionInbox::new([author.to_owned()]);
        if let Some(state) = storage_get(&key) {
            inbox.restore(&state);
        }
        let opens: Arc<Mutex<Vec<Place>>> = Arc::default();
        let chosen = Arc::clone(&opens);
        ui::notify::set_activation_handler(SCHEME, move |uri| {
            if let Some(place) = Place::from_activation(uri) {
                chosen
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(place);
            }
        });
        Self {
            inbox,
            key,
            last_ms: f64::NEG_INFINITY,
            pending: std::collections::BTreeMap::new(),
            opens,
        }
    }
}

impl Desktop {
    /// Look for new mentions of this person in what reached the page, and
    /// open what they chose from a notice.
    pub(super) fn tell_mentions(&self) {
        self.open_chosen_mentions();
        let Some(peer) = self.asks.peer else {
            return;
        };
        let mut mentions = self.mentions.borrow_mut();
        let (ready, touched) = crate::replication::take_touched(peer);
        for record in touched {
            *mentions.pending.entry(record).or_insert(true) &= !ready;
        }
        if mentions.pending.is_empty() {
            return;
        }
        let now = js_sys::Date::now();
        if now - mentions.last_ms < LOOK_EVERY_MS {
            // Looked at on a frame once the pause is over.
            self.shared.wake();
            return;
        }
        mentions.last_ms = now;
        let touched = std::mem::take(&mut mentions.pending);
        let notifier = ui::host::current().notifications;
        let name = |user: &str| self.person_name(user);
        let mut changed = false;
        for ((app, record), replayed) in touched {
            let Some(doc) = crate::replication::record_document(peer, &app, &record) else {
                continue;
            };
            let tree = ui::mentions::chain_of(&record)
                .and_then(|chain| crate::replication::record_document(peer, &app, chain));
            let kind = crate::desktop::application(&app)
                .map(|definition| definition.record_kind.as_str())
                .unwrap_or_default();
            // What the room held before this device followed the record is
            // not news.
            let quiet = replayed && !mentions.inbox.knows(&app, &record);
            let told = mentions.inbox.look(
                &Look {
                    app: &app,
                    record: &record,
                    kind,
                    doc: &doc,
                    tree: tree.as_ref(),
                    quiet,
                },
                &name,
            );
            for tag in &told.withdraw {
                // A host without notifications has nothing to take back.
                drop(notifier.withdraw(tag));
            }
            for notice in &told.show {
                drop(notifier.show(&notice.notification()));
            }
            changed |= !told.show.is_empty() || !told.withdraw.is_empty() || quiet;
        }
        if changed {
            storage_set(&mentions.key, &mentions.inbox.told_state());
        }
    }

    /// Open, as this person, each place they chose from a mention's notice:
    /// the app on the record, showing the item the mention is in.
    fn open_chosen_mentions(&self) {
        let chosen = std::mem::take(
            &mut *self
                .mentions
                .borrow()
                .opens
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for place in chosen {
            let target = if place.record.is_empty() {
                tilo_actions::remote::application_action_target(
                    &self.room,
                    &place.app,
                    place.resource.clone(),
                    tilo_actions::remote::APPLICATION_OPEN_ACTION,
                )
            } else {
                tilo_actions::remote::application_record_target(
                    &self.room,
                    &place.app,
                    &place.record,
                    place.resource.clone(),
                    tilo_actions::remote::APPLICATION_OPEN_ACTION,
                )
            };
            let request = InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target,
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
            };
            // A refused open is the session's to report.
            drop(self.session.invoke(&self.caller(), request));
            self.shared.wake();
        }
    }
}

fn local_storage() -> Option<js_sys::Object> {
    js_sys::Reflect::get(&js_sys::global(), &"localStorage".into())
        .ok()
        .filter(|storage| !storage.is_undefined() && !storage.is_null())
        .map(Into::into)
}

fn storage_get(key: &str) -> Option<String> {
    let storage = local_storage()?;
    let get: js_sys::Function = js_sys::Reflect::get(&storage, &"getItem".into())
        .ok()?
        .into();
    get.call1(&storage, &key.into()).ok()?.as_string()
}

fn storage_set(key: &str, value: &str) {
    let Some(storage) = local_storage() else {
        return;
    };
    let Ok(set) = js_sys::Reflect::get(&storage, &"setItem".into()) else {
        return;
    };
    let set: js_sys::Function = set.into();
    // A full or refused storage forgets what was told: at worst a reload
    // tells it again.
    drop(set.call2(&storage, &key.into(), &value.into()));
}

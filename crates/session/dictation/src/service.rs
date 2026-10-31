//! Dictation as the session hosts it: the `dictation:session` resource, its
//! actions, the reading the sheet and the palette follow, and the session's
//! own handle on it.
//!
//! The link is how the session drives a dictation — starting one, telling the
//! sheet what it heard, inserting the final text into the bound field, keeping
//! a transcript for copying. Actions reach the link as requests, which the
//! session carries out where the compositor is at hand; nothing an action asks
//! for can allow the microphone.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

use serde_json::{Map, Value};
use settings_store::personal_dictionary::{Reader, Vocabulary};
use tilo_actions::{ActionAddress, ActionOutput, DoorError, ResourceSpec};
use tilo_service::{Service, Started, decode, offer, Wake};
use zeroize::Zeroizing;

use crate::effects::DictationAction;
use crate::session::Dictation;
use crate::status::{DictationError, DictationStatus, SessionId};
use crate::target::Target;

/// The resource holding where dictation stands.
pub const DICTATION: &str = "dictation:session";
/// The kind of [`DICTATION`].
pub const DICTATION_KIND: &str = "so.tilo.dictation.session";

/// The `dictation:session` resource, with the actions a person can take on it.
#[must_use]
pub fn dictation_resource() -> ResourceSpec {
    offer::<DictationAction>(ResourceSpec::new(DICTATION, DICTATION_KIND, "Dictation"))
}

/// What the session was asked of dictation: by the person's key, by the
/// palette, or by an app through the door.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// The dictation key: start one, or finish the one listening.
    Toggle,
    /// Start a dictation: the person is asked to allow the microphone.
    Start,
    /// Stop recording and insert what was heard.
    Finish,
    /// Stop recording and throw away what was heard.
    Cancel,
    /// Throw away a transcript that was not inserted.
    DiscardTranscript,
}

/// The session's handle on dictation, shared with the service that publishes
/// it. Cheap to clone; everything it says happens at once.
#[derive(Clone)]
pub struct DictationLink {
    dictation: Rc<RefCell<Dictation>>,
    requests: Rc<RefCell<Vec<Request>>>,
}

impl Default for DictationLink {
    fn default() -> Self {
        Self {
            dictation: Rc::new(RefCell::new(Dictation::default())),
            requests: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl std::fmt::Debug for DictationLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DictationLink")
            .field("status", &self.status())
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

impl DictationLink {
    /// Where the dictation stands.
    #[must_use]
    pub fn status(&self) -> DictationStatus {
        self.dictation.borrow().status()
    }

    /// Which dictation this is about, if any.
    #[must_use]
    pub fn id(&self) -> Option<SessionId> {
        self.dictation.borrow().id()
    }

    /// Why the last dictation did not run.
    #[must_use]
    pub fn failure(&self) -> Option<DictationError> {
        self.dictation.borrow().failure()
    }

    /// The live hypothesis of `id`, for the sheet the person is looking at.
    /// It is theirs alone: it never reaches a reading, a log or history.
    #[must_use]
    pub fn preview(&self, id: SessionId) -> Option<Zeroizing<String>> {
        self.dictation
            .borrow()
            .preview(id)
            .map(|draft| Zeroizing::new(draft.to_owned()))
    }

    /// Ask for a dictation; nothing records until the person allows it.
    ///
    /// # Errors
    /// [`DictationError::Busy`] while another dictation or its transcript owns
    /// the seat.
    pub fn begin(
        &self,
        now: Instant,
    ) -> Result<SessionId, DictationError> {
        self.dictation.borrow_mut().begin(now)
    }

    /// Step the dictation on: the field, what the recognizer heard, deadlines.
    pub fn advance(&self, target: &mut dyn Target, now: Instant) {
        self.dictation.borrow_mut().advance(target, now);
    }

    /// The person allowed the microphone for `id`, once, in this field.
    ///
    /// # Errors
    /// [`DictationError::StaleSession`] when `id` is not the dictation asked
    /// about, and the field's own refusal.
    pub fn allow(
        &self,
        id: SessionId,
        target: &mut dyn Target,
        now: Instant,
    ) -> Result<(), DictationError> {
        self.dictation.borrow_mut().allow(id, target, now)
    }

    /// Stop the microphone and ask for the final text.
    ///
    /// # Errors
    /// [`DictationError::StaleSession`] unless `id` is the dictation listening.
    pub fn stop_recording(&self, id: SessionId) -> Result<(), DictationError> {
        self.dictation.borrow_mut().stop_recording(id)
    }

    /// Insert the final text into the field the person was in, once.
    ///
    /// # Errors
    /// The stale, changed, timed-out and refused failures.
    pub fn insert(
        &self,
        id: SessionId,
        target: &mut dyn Target,
        now: Instant,
    ) -> Result<(), DictationError> {
        self.dictation.borrow_mut().finish(id, target, now)
    }

    /// Stop what `id` asked for; `false` when it was over already.
    pub fn cancel(&self, id: SessionId) -> bool {
        self.dictation.borrow_mut().cancel(id)
    }

    /// Stop everything and keep nothing: the session is locking.
    pub fn discard(&self) {
        self.dictation.borrow_mut().discard();
    }

    /// The transcript kept for the person to copy, if `id` names it.
    #[must_use]
    pub fn recovery(&self, id: SessionId) -> Option<Zeroizing<String>> {
        self.dictation
            .borrow()
            .recovery(id)
            .map(|text| Zeroizing::new(text.to_owned()))
    }

    /// Note what became of copying the kept transcript.
    pub fn copied(&self, id: SessionId, copied: bool) {
        self.dictation.borrow_mut().note_copied(id, copied);
    }

    /// Throw away the kept transcript of `id`.
    pub fn discard_recovery(&self, id: SessionId) -> bool {
        self.dictation.borrow_mut().discard_recovery_of(id)
    }

    /// The person's vocabulary, read by the recognizer's own thread.
    pub fn set_vocabulary(&self, reader: Option<Arc<dyn Vocabulary>>) {
        self.dictation.borrow_mut().set_vocabulary(reader);
    }

    /// Ask the session to do something with dictation.
    pub fn request(&self, request: Request) {
        self.requests.borrow_mut().push(request);
    }

    /// Take what was asked since the last call.
    #[must_use]
    pub fn take_requests(&self) -> Vec<Request> {
        std::mem::take(&mut *self.requests.borrow_mut())
    }

    /// The reading a sheet or the palette follows: where the dictation
    /// stands, which one it is and why nothing ran. Never the words.
    #[must_use]
    pub fn reading(&self) -> Value {
        let dictation = self.dictation.borrow();
        let status = dictation.status();
        let recovery = match status {
            DictationStatus::Recovery {
                reason,
                copied,
                copy_failed,
            } => serde_json::json!({
                "reason": reason.word(),
                "copied": copied,
                "copy_failed": copy_failed,
            }),
            _ => Value::Null,
        };
        serde_json::json!({
            "status": status.word(),
            "session": dictation.id().map(SessionId::get),
            "failure": dictation.failure().map(DictationError::word),
            "recovery": recovery,
        })
    }
}

/// The dictation service, published beside the session's other services.
pub struct DictationService {
    link: DictationLink,
    vocabulary: VocabularyBinding,
}

impl DictationService {
    /// The service, and the session's handle on it.
    #[must_use]
    pub fn start(wake: Wake) -> (Self, DictationLink) {
        let link = DictationLink::default();
        let vocabulary = VocabularyBinding::new(wake);
        (Self {
            link: link.clone(),
            vocabulary,
        }, link)
    }

    /// The handle this service publishes for.
    #[must_use]
    pub fn link(&self) -> &DictationLink {
        &self.link
    }
}

impl Service for DictationService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![dictation_resource()]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        match resource {
            DICTATION => Some(self.link.reading()),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource != DICTATION {
            return Err(DoorError::Unknown(ActionAddress::new(resource, action)));
        }
        let published = self.resources();
        let action = decode::<DictationAction>(&published, resource, action, arguments)?;
        self.link.request(match action {
            DictationAction::Start => Request::Start,
            DictationAction::Finish => Request::Finish,
            DictationAction::Cancel => Request::Cancel,
            DictationAction::DiscardTranscript => Request::DiscardTranscript,
        });
        Ok(Started::Done(ActionOutput::default()))
    }

    fn poll(&mut self) -> bool {
        self.vocabulary.refresh(&self.link)
    }
}

/// The host's personal dictionary, delivered to the recognizer's own thread.
/// Nothing of it is read here: a change only wakes the loop, which rebinds the
/// reader before the next word is recognized.
struct VocabularyBinding {
    service: Option<Weak<settings_store::personal_dictionary::Service>>,
    reader: Option<Reader>,
    changed: Arc<AtomicBool>,
    _listener: Arc<settings_store::Listener>,
}

impl VocabularyBinding {
    fn new(wake: Wake) -> Self {
        let service = settings_store::installed_source()
            .and_then(|source| source.dictionary_service());
        let changed = Arc::new(AtomicBool::new(true));
        let pending = Arc::clone(&changed);
        let listener: Arc<settings_store::Listener> = Arc::new(move || {
            pending.store(true, Ordering::Release);
            wake();
        });
        if let Some(service) = &service {
            service.changes().listen(&listener);
        }
        Self {
            service: service.map(|service| Arc::downgrade(&service)),
            reader: None,
            changed,
            _listener: listener,
        }
    }

    /// Rebind the reader when the host's binding changed. Word edits keep the
    /// binding, so the recognizer can offer the draft a person already saw.
    fn refresh(&mut self, link: &DictationLink) -> bool {
        if !self.changed.swap(false, Ordering::AcqRel) {
            return false;
        }
        let reader = self
            .service
            .as_ref()
            .and_then(Weak::upgrade)
            .and_then(|service| service.reader());
        let same = match (&self.reader, &reader) {
            (Some(previous), Some(next)) => previous.same_binding(next),
            (None, None) => true,
            _ => false,
        };
        if same {
            return false;
        }
        self.reader.clone_from(&reader);
        link.set_vocabulary(reader.map(|reader| Arc::new(reader) as Arc<dyn Vocabulary>));
        true
    }
}

impl std::fmt::Debug for DictationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DictationService")
            .field("link", &self.link)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reading_says_where_a_dictation_stands_and_never_what_it_heard() {
        let link = DictationLink::default();
        assert_eq!(
            link.reading(),
            serde_json::json!({
                "status": "idle",
                "session": null,
                "failure": null,
                "recovery": null,
            })
        );
        link.request(Request::Start);
        assert_eq!(link.take_requests(), vec![Request::Start]);
        assert!(link.take_requests().is_empty());
    }

    #[test]
    fn an_action_asks_for_a_dictation_and_never_consents_to_one() {
        let (mut service, link) = DictationService::start(Arc::new(|| {}));
        let started = service
            .invoke(DICTATION, "start", &Map::new())
            .expect("start is offered");
        assert_eq!(started, Started::Done(ActionOutput::default()));
        assert_eq!(link.take_requests(), vec![Request::Start]);
        assert_eq!(
            service.invoke(DICTATION, "allow", &Map::new()),
            Err(DoorError::Unknown(ActionAddress::new(DICTATION, "allow"))),
            "there is no action that allows the microphone"
        );
    }
}

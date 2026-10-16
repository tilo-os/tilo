//! Live delivery of the installed host's existing person-bound capability.
//! Notifications wake the native loop; no dictionary bytes are read here.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use settings_store::personal_dictionary::{Reader, Service, Vocabulary};

use super::State;

pub(crate) struct VocabularyBinding {
    service: Option<Weak<Service>>,
    reader: Option<Reader>,
    changed: Arc<AtomicBool>,
    _listener: Arc<settings_store::Listener>,
}

impl VocabularyBinding {
    pub(crate) fn new(wake: crate::session_runtime::Wake) -> Self {
        let service =
            settings_store::installed_source().and_then(|source| source.dictionary_service());
        Self::from_service(service.as_ref(), wake)
    }

    pub(super) fn from_service(
        service: Option<&Arc<Service>>,
        wake: crate::session_runtime::Wake,
    ) -> Self {
        let changed = Arc::new(AtomicBool::new(true));
        let pending = Arc::clone(&changed);
        let listener: Arc<settings_store::Listener> = Arc::new(move || {
            pending.store(true, Ordering::Release);
            wake();
        });
        if let Some(service) = service {
            service.changes().listen(&listener);
        }
        Self {
            service: service.map(Arc::downgrade),
            reader: None,
            changed,
            _listener: listener,
        }
    }

    /// Apply identity replacement before handling input or queued transcripts.
    /// Word edits retain the binding so the actor can offer its visible draft.
    pub(crate) fn refresh(&mut self, state: &mut State) {
        if !self.changed.swap(false, Ordering::AcqRel) {
            return;
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
            return;
        }
        self.reader.clone_from(&reader);
        state
            .set_dictation_vocabulary(reader.map(|reader| Arc::new(reader) as Arc<dyn Vocabulary>));
    }
}

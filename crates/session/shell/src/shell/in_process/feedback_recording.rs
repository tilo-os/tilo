//! Recordings of the window a report is about, made on this device
//! (`docs/architecture/session-replay.md`).
//!
//! The window's record is read as its workspace committed it, the state
//! History and Activity read too; its last
//! [`RECORDING_WINDOW_MS`](tilo_app::feedback::recording::RECORDING_WINDOW_MS)
//! become a recording only once the person includes one in a review. The
//! recording's bytes are kept where the destination Chat's content lives
//! on this device, so whoever may read that Chat fetches them on its route
//! like the report's picture.

use tilo_app::feedback::recording::{LIMITS, RECORDING_WINDOW_MS};
use tilo_app::feedback::{Error, MaskedScreenshot, Recorded, RecordingScope, ReviewText, Subject};
use tilo_desktop::feedback::transport::{Capture, NoCapture};
use tilo_resource::model::Doc;

/// The saved document of `record` of `app` in `workspace`, as the workspace
/// committed it, when this device holds it.
pub(super) type Documents = Box<dyn Fn(&str, &str, &str) -> Option<Vec<u8>>>;
/// Whether windows of an app are withheld from captures.
pub(super) type Protected = Box<dyn Fn(&str) -> bool>;
/// Keep bytes where the destination Chat's content lives; their hash.
pub(super) type Keep = Box<dyn Fn(&[u8]) -> Result<String, String>>;

/// Records the window a report is about; takes no screenshots.
pub(super) struct WindowRecorder {
    pub(super) documents: Documents,
    pub(super) protected: Protected,
    pub(super) keep: Keep,
    /// Milliseconds since the Unix epoch, now.
    pub(super) now_ms: Box<dyn Fn() -> i64>,
}

impl WindowRecorder {
    /// The document of the record `subject`'s window shows, unless its app
    /// is withheld from captures.
    fn document(&self, subject: &Subject) -> Option<Doc> {
        let shown = subject.record.as_ref()?;
        if (self.protected)(&subject.app) {
            return None;
        }
        let bytes = (self.documents)(&shown.workspace, &subject.app, &shown.record)?;
        Doc::load(&bytes).ok()
    }

    fn name(subject: &Subject) -> &str {
        if subject.name.trim().is_empty() {
            &subject.app
        } else {
            &subject.name
        }
    }

    fn window_start(&self) -> i64 {
        (self.now_ms)().saturating_sub(RECORDING_WINDOW_MS)
    }
}

/// The kind the record's identity names, else its app.
fn kind(doc: &Doc, app: &str) -> String {
    use automerge::{ROOT, ReadDoc as _, Value};
    doc.get(ROOT, "tilo:record")
        .ok()
        .flatten()
        .and_then(|(_, meta)| doc.get(&meta, "kind").ok().flatten())
        .and_then(|(value, _)| match value {
            Value::Scalar(scalar) => scalar.as_str().map(str::to_owned),
            Value::Object(_) => None,
        })
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or_else(|| app.to_owned())
}

impl Capture for WindowRecorder {
    fn offers_screenshots(&self) -> bool {
        NoCapture.offers_screenshots()
    }
    fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
        NoCapture.context()
    }
    fn capture(&mut self) -> Result<MaskedScreenshot, Error> {
        NoCapture.capture()
    }
    fn persist(&mut self, image: &MaskedScreenshot) -> Result<(), Error> {
        NoCapture.persist(image)
    }
    fn recording_scope(&mut self, subject: &Subject) -> Option<RecordingScope> {
        let mut doc = self.document(subject)?;
        let heads = doc.get_heads();
        let planned =
            tilo_resource::recording::plan(&doc, self.window_start(), &heads, &LIMITS).ok()?;
        Some(RecordingScope {
            app_name: Self::name(subject).to_owned(),
            from_ms: planned.from_ms,
            to_ms: planned.to_ms,
            steps: planned.steps,
        })
    }
    fn record(&mut self, subject: &Subject) -> Result<Recorded, Error> {
        let mut doc = self.document(subject).ok_or(Error::Invalid("recording"))?;
        let heads = doc.get_heads();
        let recording =
            tilo_resource::recording::record(&doc, self.window_start(), &heads, &LIMITS)
                .map_err(|_| Error::Invalid("recording"))?;
        Recorded::new(
            &subject.app,
            Self::name(subject),
            &kind(&doc, &subject.app),
            &recording,
        )
    }
    fn persist_recording(&mut self, recording: &Recorded) -> Result<(), Error> {
        let kept = (self.keep)(recording.bytes()).map_err(|_| Error::NotDurable)?;
        if kept != recording.reference().hash {
            return Err(Error::NotDurable);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilo_app::feedback::ShownRecord;
    use tilo_resource::history::commit_stamped_at;

    /// A note: its identity, then edits at seconds 10, 700, 800 and 900.
    fn note() -> Vec<u8> {
        use automerge::transaction::Transactable as _;
        let mut doc = Doc::new();
        let meta = doc
            .put_object(ROOT, "tilo:record", automerge::ObjType::Map)
            .unwrap();
        doc.put(&meta, "kind", "notes.note").unwrap();
        for (second, body) in [(10, "a"), (700, "ab"), (800, "abc"), (900, "abcd")] {
            doc.put(ROOT, "body", body).unwrap();
            commit_stamped_at(&mut doc, second).unwrap();
        }
        doc.save()
    }

    fn recorder(kept: Rc<RefCell<Vec<Vec<u8>>>>) -> WindowRecorder {
        let note = note();
        WindowRecorder {
            documents: Box::new(move |workspace, app, record| {
                (workspace == "w" && app == "so.tilo.notes" && record == "note-1")
                    .then(|| note.clone())
            }),
            protected: Box::new(|app| app == "so.tilo.passwords"),
            keep: Box::new(move |bytes| {
                kept.borrow_mut().push(bytes.to_vec());
                Ok(blake3::hash(bytes).to_hex().to_string())
            }),
            now_ms: Box::new(|| 1_000_000),
        }
    }

    fn showing(app: &str, record: &str) -> Subject {
        Subject {
            record: Some(ShownRecord {
                workspace: "w".into(),
                record: record.into(),
            }),
            ..Subject::of_app(app, "Notes")
        }
    }

    #[test]
    fn the_window_records_the_last_minutes_of_its_record_and_keeps_them_once_sent() {
        let kept = Rc::new(RefCell::new(Vec::new()));
        let mut recorder = recorder(kept.clone());
        let subject = showing("so.tilo.notes", "note-1");
        let scope = recorder.recording_scope(&subject).unwrap();
        assert_eq!(
            scope,
            RecordingScope {
                app_name: "Notes".into(),
                from_ms: 400_000,
                to_ms: 900_000,
                steps: 3,
            }
        );
        assert!(kept.borrow().is_empty(), "offering keeps nothing");
        let made = recorder.record(&subject).unwrap();
        assert_eq!(made.scope(), scope);
        assert_eq!(made.reference().kind, "notes.note");
        recorder.persist_recording(&made).unwrap();
        assert_eq!(*kept.borrow(), vec![made.bytes().to_vec()]);
    }

    #[test]
    fn no_record_another_record_or_a_protected_app_is_never_recorded() {
        let mut recorder = recorder(Rc::default());
        assert!(
            recorder
                .recording_scope(&Subject::of_app("so.tilo.notes", "Notes"))
                .is_none()
        );
        assert!(
            recorder
                .recording_scope(&showing("so.tilo.notes", "note-2"))
                .is_none()
        );
        assert!(
            recorder
                .recording_scope(&showing("so.tilo.passwords", "note-1"))
                .is_none()
        );
        assert!(
            recorder
                .record(&showing("so.tilo.passwords", "note-1"))
                .is_err()
        );
    }

    #[test]
    fn bytes_kept_under_another_address_are_not_kept() {
        let mut recorder = recorder(Rc::default());
        recorder.keep = Box::new(|_| Ok("00".repeat(32)));
        let made = recorder
            .record(&showing("so.tilo.notes", "note-1"))
            .unwrap();
        assert!(matches!(
            recorder.persist_recording(&made),
            Err(Error::NotDurable)
        ));
    }
}

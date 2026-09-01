//! Automerge document layer for the Install Media app.
//!
//! Shared plan and job state lives in the CRDT so the selected source/target,
//! install options, and live progress are undoable and replicate to
//! collaborators. Destructive confirmation is deliberately peer-local: a
//! collaborator cannot arm a write on this machine. Machine-derived blobs (the
//! probed target list, recent sources, the active install
//! profile) are serialised as JSON string scalars — they are not edited
//! character-by-character, so a plain scalar is the right granularity.
//!
//! ```text
//! root
//! ├── "targets"      (Str)   # serde_json of Vec<TargetDrive>
//! ├── "releases"     (Str)   # serde_json of Vec<QosRelease>
//! ├── "source"       (Str)   # serde_json of Option<Source>
//! ├── "recent"       (Str)   # serde_json of Vec<Source>
//! ├── "profile"      (Str)   # serde_json of InstallProfile (the live edits)
//! ├── "progress"     (Str)   # serde_json of Progress
//! ├── "selected"     (Int)   # index into targets (or -1)
//! ├── "action"       (Str)   # flash | install
//! └── "status"       (Str)   # last status-line message
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{
    Action, InstallProfile, Progress, QosRelease, Source, Step, TargetDrive, sample_recent_sources,
    sample_releases, sample_source, sample_targets,
};

pub struct ImagerDoc;

impl ImagerDoc {
    /// True when the document has never been seeded.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        !matches!(doc.get(&ROOT, "targets"), Ok(Some((Value::Scalar(_), _))))
    }

    /// Seed the document with **empty** collections + default UI state — no
    /// fabricated image, no fake recent history, no invented drives. The live
    /// app starts genuinely blank and fills `targets` from a real `lsblk`
    /// probe; sample/demo content is opt-in via [`Self::seed_demo`].
    /// Idempotent: only writes fields that are missing.
    pub fn seed(doc: &mut AutoCommit, targets: &[TargetDrive]) {
        if Self::get_str(doc, "targets").is_none() {
            Self::write_targets(doc, targets);
        }
        if Self::get_str(doc, "releases").is_none() {
            Self::write_json(doc, "releases", &Vec::<QosRelease>::new());
        }
        if Self::get_str(doc, "source").is_none() {
            Self::set_source(doc, None);
        }
        if Self::get_str(doc, "recent").is_none() {
            Self::write_json(doc, "recent", &Vec::<Source>::new());
        }
        if Self::get_str(doc, "profile").is_none() {
            Self::write_json(doc, "profile", &InstallProfile::default());
        }
        if Self::get_str(doc, "progress").is_none() {
            Self::write_json(doc, "progress", &Progress::default());
        }
        if !matches!(doc.get(&ROOT, "selected"), Ok(Some((Value::Scalar(_), _)))) {
            // Start with NO drive selected: the wizard's Drive step must gate
            // on a deliberate user choice before it will advance.
            let _ = doc.put(&ROOT, "selected", -1i64);
        }
        if Self::get_str(doc, "action").is_none() {
            let _ = doc.put(&ROOT, "action", "flash");
        }
        if Self::get_str(doc, "step").is_none() {
            let _ = doc.put(&ROOT, "step", Step::Source.key());
        }
        if Self::get_str(doc, "status").is_none() {
            let _ = doc.put(&ROOT, "status", "");
        }
    }

    /// Seed a fully-populated **demo** document: the sample drives, the sample
    /// qOS releases, a pre-selected qOS image, and a short recent-sources
    /// history. This is the single home of the imager's sample content — used
    /// only by `--shots`, the `--demo` flag, and tests, never by the live app
    /// (which stays blank per [`Self::seed`]).
    pub fn seed_demo(doc: &mut AutoCommit) {
        Self::seed(doc, &sample_targets());
        Self::write_json(doc, "releases", &sample_releases());
        Self::set_source(doc, Some(&sample_source()));
        Self::write_json(doc, "recent", &sample_recent_sources());
    }

    // ---- targets ----------------------------------------------------------
    pub fn write_targets(doc: &mut AutoCommit, targets: &[TargetDrive]) {
        Self::write_json(doc, "targets", &targets.to_vec());
    }
    /// The probed drive list. A missing or unreadable blob yields **nothing** —
    /// the imager offers a drive to erase only when it has actually seen one.
    pub fn targets(doc: &AutoCommit) -> Vec<TargetDrive> {
        Self::read_json(doc, "targets").unwrap_or_default()
    }

    // ---- qOS releases -------------------------------------------------------
    /// Releases offered as a write source. Empty unless something actually put
    /// one here: there is no built-in list to fall back on, because a release
    /// row is a write source for a drive-erasing job.
    pub fn releases(doc: &AutoCommit) -> Vec<QosRelease> {
        Self::read_json(doc, "releases").unwrap_or_default()
    }
    pub fn set_releases(doc: &mut AutoCommit, releases: &[QosRelease]) {
        Self::write_json(doc, "releases", &releases.to_vec());
    }

    // ---- source -----------------------------------------------------------
    pub fn set_source(doc: &mut AutoCommit, source: Option<&Source>) {
        let json = serde_json::to_string(&source.cloned()).unwrap_or_else(|_| "null".into());
        let _ = doc.put(&ROOT, "source", json);
    }
    pub fn source(doc: &AutoCommit) -> Option<Source> {
        Self::read_json::<Option<Source>>(doc, "source").flatten()
    }

    // ---- recent sources ---------------------------------------------------
    pub fn recent(doc: &AutoCommit) -> Vec<Source> {
        Self::read_json(doc, "recent").unwrap_or_default()
    }
    pub fn set_recent(doc: &mut AutoCommit, recent: &[Source]) {
        Self::write_json(doc, "recent", &recent.to_vec());
    }

    // ---- live install profile --------------------------------------------
    pub fn profile(doc: &AutoCommit) -> InstallProfile {
        Self::read_json(doc, "profile").unwrap_or_default()
    }
    pub fn set_profile(doc: &mut AutoCommit, profile: &InstallProfile) {
        Self::write_json(doc, "profile", profile);
    }

    // ---- progress ---------------------------------------------------------
    pub fn progress(doc: &AutoCommit) -> Progress {
        Self::read_json(doc, "progress").unwrap_or_default()
    }
    pub fn set_progress(doc: &mut AutoCommit, p: &Progress) {
        Self::write_json(doc, "progress", p);
    }

    // ---- scalar UI fields -------------------------------------------------
    pub fn selected(doc: &AutoCommit) -> Option<usize> {
        match doc.get(&ROOT, "selected") {
            Ok(Some((Value::Scalar(v), _))) => {
                let i = v.to_i64().unwrap_or(-1);
                if i < 0 { None } else { Some(i as usize) }
            }
            _ => None,
        }
    }
    pub fn set_selected(doc: &mut AutoCommit, idx: Option<usize>) {
        let v = idx.map_or(-1, |i| i as i64);
        let _ = doc.put(&ROOT, "selected", v);
    }

    pub fn action(doc: &AutoCommit) -> Action {
        Action::from_key(&Self::get_str(doc, "action").unwrap_or_default())
    }
    pub fn set_action(doc: &mut AutoCommit, a: Action) {
        let _ = doc.put(&ROOT, "action", a.key());
    }

    pub fn step(doc: &AutoCommit) -> Step {
        Step::from_key(&Self::get_str(doc, "step").unwrap_or_default())
    }
    pub fn set_step(doc: &mut AutoCommit, s: Step) {
        let _ = doc.put(&ROOT, "step", s.key());
    }

    pub fn status(doc: &AutoCommit) -> String {
        Self::get_str(doc, "status").unwrap_or_default()
    }
    pub fn set_status(doc: &mut AutoCommit, s: &str) {
        let _ = doc.put(&ROOT, "status", s);
    }

    /// Whether internal/fixed disks are revealed on the Drive step. Presentation
    /// state (like `selected`/`step`), persisted for instant-on. Defaults off so
    /// a fresh document only ever shows removable drives.
    pub fn show_internal(doc: &AutoCommit) -> bool {
        match doc.get(&ROOT, "show_internal") {
            Ok(Some((Value::Scalar(v), _))) => matches!(v.as_ref(), ScalarValue::Boolean(true)),
            _ => false,
        }
    }
    pub fn set_show_internal(doc: &mut AutoCommit, on: bool) {
        let _ = doc.put(&ROOT, "show_internal", on);
    }

    // ---- helpers ----------------------------------------------------------
    fn write_json<T: serde::Serialize>(doc: &mut AutoCommit, key: &str, val: &T) {
        let json = serde_json::to_string(val).unwrap_or_else(|_| "null".into());
        let _ = doc.put(&ROOT, key, json);
    }
    fn read_json<T: serde::de::DeserializeOwned>(doc: &AutoCommit, key: &str) -> Option<T> {
        let raw = Self::get_str(doc, key)?;
        serde_json::from_str(&raw).ok()
    }
    fn get_str(doc: &AutoCommit, key: &str) -> Option<String> {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstallMode, SourceKind};

    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        assert!(ImagerDoc::is_empty(&doc));
        ImagerDoc::seed(&mut doc, &sample_targets());
        doc
    }

    #[test]
    fn seed_starts_blank_no_fake_content() {
        // The default seed is EMPTY: real drives fill in from a live probe, and
        // there is no fabricated image or recent history.
        let doc = seeded();
        assert!(!ImagerDoc::is_empty(&doc));
        assert_eq!(
            ImagerDoc::targets(&doc).len(),
            2,
            "the caller's drives, verbatim"
        );
        assert!(ImagerDoc::source(&doc).is_none(), "no fabricated image");
        assert!(
            ImagerDoc::recent(&doc).is_empty(),
            "no fabricated recent history"
        );
        assert_eq!(ImagerDoc::action(&doc), Action::Flash);
        // A fresh document starts with no drive chosen — the Drive step gates on
        // a deliberate selection.
        assert_eq!(ImagerDoc::selected(&doc), None);
    }

    #[test]
    fn seed_demo_populates_sample_content() {
        // The opt-in demo seed is the only place sample content comes from.
        let mut doc = AutoCommit::new();
        ImagerDoc::seed_demo(&mut doc);
        assert_eq!(ImagerDoc::targets(&doc).len(), 2);
        assert!(ImagerDoc::source(&doc).is_some());
        assert!(!ImagerDoc::recent(&doc).is_empty());
        assert_eq!(ImagerDoc::releases(&doc).len(), 2);
    }

    /// Reading a document that holds no inventory must not conjure one. Both of
    /// these lists name something the user can point a drive-erasing write at,
    /// so a "helpful" fallback is a fabricated destination and a fabricated
    /// source — the two halves of the one destructive operation.
    #[test]
    fn an_unseeded_document_names_no_drive_and_no_release() {
        let doc = AutoCommit::new();
        assert!(ImagerDoc::is_empty(&doc));
        assert!(ImagerDoc::targets(&doc).is_empty(), "no fabricated drives");
        assert!(
            ImagerDoc::releases(&doc).is_empty(),
            "no fabricated releases"
        );
    }

    #[test]
    fn plain_seed_offers_no_release() {
        // The live path seeds with a real (possibly empty) probe result and
        // nothing else: releases stay empty until something actually finds one.
        let doc = seeded();
        assert!(ImagerDoc::releases(&doc).is_empty());
    }

    #[test]
    fn source_round_trips() {
        let mut doc = seeded();
        assert!(ImagerDoc::source(&doc).is_none());
        let sample = sample_source();
        ImagerDoc::set_source(&mut doc, Some(&sample));
        let src = ImagerDoc::source(&doc).unwrap();
        assert_eq!(src.kind, SourceKind::QosImage);
        ImagerDoc::set_source(&mut doc, None);
        assert!(ImagerDoc::source(&doc).is_none());
    }

    #[test]
    fn profile_edits_persist() {
        let mut doc = seeded();
        let mut prof = ImagerDoc::profile(&doc);
        prof.hostname = "edited-host".into();
        prof.mode = InstallMode::Persistent;
        prof.encrypt = true;
        ImagerDoc::set_profile(&mut doc, &prof);
        let back = ImagerDoc::profile(&doc);
        assert_eq!(back.hostname, "edited-host");
        assert_eq!(back.mode, InstallMode::Persistent);
        assert!(back.encrypt);
    }

    #[test]
    fn seed_is_idempotent() {
        let mut doc = seeded();
        ImagerDoc::set_selected(&mut doc, Some(1));
        ImagerDoc::set_action(&mut doc, Action::Install);
        ImagerDoc::seed(&mut doc, &sample_targets()); // re-seed must not clobber
        assert_eq!(ImagerDoc::selected(&doc), Some(1));
        assert_eq!(ImagerDoc::action(&doc), Action::Install);
    }

    #[test]
    fn status_round_trips_without_a_replicated_confirmation() {
        let mut doc = seeded();
        ImagerDoc::set_status(&mut doc, "ready");
        assert_eq!(ImagerDoc::status(&doc), "ready");
        assert!(doc.get(&ROOT, "confirm").unwrap().is_none());
    }

    #[test]
    fn step_defaults_to_source_and_round_trips() {
        let mut doc = seeded();
        assert_eq!(ImagerDoc::step(&doc), Step::Source);
        ImagerDoc::set_step(&mut doc, Step::Write);
        assert_eq!(ImagerDoc::step(&doc), Step::Write);
    }
}

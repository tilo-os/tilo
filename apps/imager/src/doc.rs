//! Automerge document layer for the Install Media app.
//!
//! All UI + job state lives in the CRDT so the selected source/target, the
//! install options, pending confirmations and live progress are undoable
//! and replicate to collaborators for free. Machine-derived blobs (the
//! probed target list, recent sources, saved profiles, the active install
//! profile) are serialised as JSON string scalars — they are not edited
//! character-by-character, so a plain scalar is the right granularity.
//!
//! ```text
//! root
//! ├── "targets"      (Str)   # serde_json of Vec<TargetDrive>
//! ├── "source"       (Str)   # serde_json of Option<Source>
//! ├── "recent"       (Str)   # serde_json of Vec<Source>
//! ├── "profile"      (Str)   # serde_json of InstallProfile (the live edits)
//! ├── "profiles"     (Str)   # serde_json of Vec<(String, InstallProfile)>
//! ├── "progress"     (Str)   # serde_json of Progress
//! ├── "selected"     (Int)   # index into targets (or -1)
//! ├── "action"       (Str)   # flash | install
//! ├── "confirm"      (Str)   # the typed device-name confirmation
//! └── "status"       (Str)   # last status-line message
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ReadDoc, ROOT, ScalarValue, Value};

use crate::model::{
    sample_profiles, sample_recent_sources, sample_source, sample_targets, Action, InstallProfile,
    Progress, Source, Step, TargetDrive,
};

pub struct ImagerDoc;

impl ImagerDoc {
    /// True when the document has never been seeded.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        !matches!(doc.get(&ROOT, "targets"), Ok(Some((Value::Scalar(_), _))))
    }

    /// Seed the document with the sample data + default UI state.
    /// Idempotent: only writes fields that are missing.
    pub fn seed(doc: &mut AutoCommit, targets: &[TargetDrive]) {
        if Self::get_str(doc, "targets").is_none() {
            Self::write_targets(doc, targets);
        }
        if Self::get_str(doc, "source").is_none() {
            Self::set_source(doc, Some(&sample_source()));
        }
        if Self::get_str(doc, "recent").is_none() {
            Self::write_json(doc, "recent", &sample_recent_sources());
        }
        if Self::get_str(doc, "profile").is_none() {
            Self::write_json(doc, "profile", &InstallProfile::default());
        }
        if Self::get_str(doc, "profiles").is_none() {
            Self::write_json(doc, "profiles", &sample_profiles());
        }
        if Self::get_str(doc, "progress").is_none() {
            Self::write_json(doc, "progress", &Progress::default());
        }
        if !matches!(doc.get(&ROOT, "selected"), Ok(Some((Value::Scalar(_), _)))) {
            // Start with NO drive selected: the wizard's Drive step must gate
            // on a deliberate user choice before it will advance to Customize.
            let _ = doc.put(&ROOT, "selected", -1i64);
        }
        if Self::get_str(doc, "action").is_none() {
            let _ = doc.put(&ROOT, "action", "flash");
        }
        if Self::get_str(doc, "step").is_none() {
            let _ = doc.put(&ROOT, "step", Step::Source.key());
        }
        if Self::get_str(doc, "confirm").is_none() {
            let _ = doc.put(&ROOT, "confirm", "");
        }
        if Self::get_str(doc, "status").is_none() {
            let _ = doc.put(&ROOT, "status", "");
        }
    }

    // ---- targets ----------------------------------------------------------
    pub fn write_targets(doc: &mut AutoCommit, targets: &[TargetDrive]) {
        Self::write_json(doc, "targets", &targets.to_vec());
    }
    pub fn targets(doc: &AutoCommit) -> Vec<TargetDrive> {
        Self::read_json(doc, "targets").unwrap_or_else(sample_targets)
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

    // ---- saved profiles ---------------------------------------------------
    pub fn profiles(doc: &AutoCommit) -> Vec<(String, InstallProfile)> {
        Self::read_json(doc, "profiles").unwrap_or_else(sample_profiles)
    }
    pub fn set_profiles(doc: &mut AutoCommit, profiles: &[(String, InstallProfile)]) {
        Self::write_json(doc, "profiles", &profiles.to_vec());
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
                if i < 0 {
                    None
                } else {
                    Some(i as usize)
                }
            }
            _ => None,
        }
    }
    pub fn set_selected(doc: &mut AutoCommit, idx: Option<usize>) {
        let v = idx.map(|i| i as i64).unwrap_or(-1);
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

    pub fn confirm(doc: &AutoCommit) -> String {
        Self::get_str(doc, "confirm").unwrap_or_default()
    }
    pub fn set_confirm(doc: &mut AutoCommit, s: &str) {
        let _ = doc.put(&ROOT, "confirm", s);
    }

    pub fn status(doc: &AutoCommit) -> String {
        Self::get_str(doc, "status").unwrap_or_default()
    }
    pub fn set_status(doc: &mut AutoCommit, s: &str) {
        let _ = doc.put(&ROOT, "status", s);
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
    fn seed_populates_all_panels() {
        let doc = seeded();
        assert!(!ImagerDoc::is_empty(&doc));
        assert_eq!(ImagerDoc::targets(&doc).len(), 2);
        assert!(ImagerDoc::source(&doc).is_some());
        assert_eq!(ImagerDoc::recent(&doc).len(), 3);
        assert_eq!(ImagerDoc::profiles(&doc).len(), 2);
        assert_eq!(ImagerDoc::action(&doc), Action::Flash);
        // A fresh document starts with no drive chosen — the Drive step gates on
        // a deliberate selection.
        assert_eq!(ImagerDoc::selected(&doc), None);
    }

    #[test]
    fn source_round_trips() {
        let mut doc = seeded();
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
    fn confirm_and_status_round_trip() {
        let mut doc = seeded();
        ImagerDoc::set_confirm(&mut doc, "sdb");
        ImagerDoc::set_status(&mut doc, "ready");
        assert_eq!(ImagerDoc::confirm(&doc), "sdb");
        assert_eq!(ImagerDoc::status(&doc), "ready");
    }

    #[test]
    fn step_defaults_to_source_and_round_trips() {
        let mut doc = seeded();
        assert_eq!(ImagerDoc::step(&doc), Step::Source);
        ImagerDoc::set_step(&mut doc, Step::Write);
        assert_eq!(ImagerDoc::step(&doc), Step::Write);
    }
}

//! Automerge document layer for the Install Media app.
//!
//! Only reviewed, user-authored configuration lives in the CRDT. In
//! particular, a source and every non-secret profile field are ordinary maps,
//! not JSON snapshots: two collaborators changing the hostname and keyboard
//! must not win/lose a whole profile race. Device inventory, a selected drive,
//! destructive confirmation, progress and worker handles are machine/window
//! local facts.
//!
//! ```text
//! root
//! ├── "source_v2"    (Map) # stable reviewed source or tombstone
//! ├── "recent_v2"    (Map) # keyed reviewed sources, rank + tombstone
//! └── "profile_v2"   (Map) # non-secret, field-granular installer profile
//! ```

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::doc::{read_bool, read_i64_coerced, read_str};

use crate::model::{
    InstallMode, InstallProfile, Source, TargetDrive, sample_recent_sources, sample_source,
};

pub struct ImagerDoc;

impl ImagerDoc {
    /// True when the document has never been seeded.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        !matches!(
            doc.get(&ROOT, "imager_schema"),
            Ok(Some((Value::Scalar(_), _)))
        )
    }

    /// Seed the document with empty collaborative records — no fabricated
    /// image or history. The local app owns device discovery and job state.
    /// Idempotent: only writes fields that are missing.
    pub fn seed(doc: &mut AutoCommit, _targets: &[TargetDrive]) {
        let _ = doc.put(&ROOT, "imager_schema", 2i64);
        if Self::source_map_read(doc).is_none() {
            Self::set_source(doc, None);
        }
        if Self::map_read(doc, &ROOT, "recent_v2").is_none() {
            Self::set_recent(doc, &[]);
        }
        if Self::profile_map_read(doc).is_none() {
            Self::set_profile(doc, &InstallProfile::default());
        }
    }

    /// Seed a fully-populated **demo** document: a pre-selected Tilo image and
    /// short recent-sources history. Device/release fixtures live in the local
    /// app, never in this collaborative document. This is the single home of
    /// the imager's sample content — used
    /// only by `--shots`, the `--demo` flag, and tests, never by the live app
    /// (which stays blank per [`Self::seed`]).
    pub fn seed_demo(doc: &mut AutoCommit) {
        Self::seed(doc, &[]);
        Self::set_source(doc, Some(&sample_source()));
        Self::set_recent(doc, &sample_recent_sources());
    }

    // ---- source -----------------------------------------------------------
    pub fn set_source(doc: &mut AutoCommit, source: Option<&Source>) {
        let Some(map) = Self::source_map(doc) else {
            return;
        };
        match source {
            Some(source) => {
                ui::doc::put_if_changed(doc, &map, "deleted", false);
                ui::doc::put_if_changed(doc, &map, "id", Self::source_id(source));
                Self::write_source_fields(doc, &map, source);
            }
            None => {
                ui::doc::put_if_changed(doc, &map, "deleted", true);
            }
        }
    }
    pub fn source(doc: &AutoCommit) -> Option<Source> {
        Self::source_map_read(doc)
            .and_then(|map| (!read_bool(doc, &map, "deleted").unwrap_or(false)).then_some(map))
            .and_then(|map| Self::read_source_fields(doc, &map))
    }

    /// Stable resource identity for a reviewed source. Prefer immutable digest,
    /// then platform identity, then the canonical path/name fallback.
    pub fn source_id(source: &Source) -> String {
        let material = source
            .expected_sha256
            .as_deref()
            .filter(|s| !s.is_empty())
            .map_or_else(
                || {
                    source.identity.map_or_else(
                        || {
                            format!(
                                "{}\u{1f}{}\u{1f}{}",
                                source.kind.tag(),
                                source.path,
                                source.name
                            )
                        },
                        |identity| {
                            format!(
                                "{}\u{1f}{}:{}",
                                source.kind.tag(),
                                identity.volume,
                                identity.file
                            )
                        },
                    )
                },
                |digest| format!("{}\u{1f}{digest}", source.kind.tag()),
            );
        // FNV-1a is a stable identifier, not a security boundary. The writer
        // independently rechecks identity/digest at the privileged boundary.
        let hash = material
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325u64, |acc, byte| {
                (acc ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            });
        format!("imager.source:{hash:016x}")
    }

    // ---- recent sources ---------------------------------------------------
    pub fn recent(doc: &AutoCommit) -> Vec<Source> {
        let Some(recent) = Self::map_read(doc, &ROOT, "recent_v2") else {
            return Vec::new();
        };
        let mut rows: Vec<(i64, String, Source)> = doc
            .keys(&recent)
            .filter_map(|id| {
                let (value, row) = doc.get(&recent, &id).ok().flatten()?;
                let Value::Object(ObjType::Map) = value else {
                    return None;
                };
                (!read_bool(doc, &row, "deleted").unwrap_or(false))
                    .then(|| {
                        Self::read_source_fields(doc, &row).map(|source| {
                            (
                                read_i64_coerced(doc, &row, "rank").unwrap_or_default(),
                                id,
                                source,
                            )
                        })
                    })
                    .flatten()
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        rows.into_iter().map(|(_, _, source)| source).collect()
    }
    pub fn set_recent(doc: &mut AutoCommit, recent: &[Source]) {
        let Some(records) = Self::map(doc, &ROOT, "recent_v2") else {
            return;
        };
        #[allow(
            clippy::needless_collect,
            reason = "the immutable Automerge key iterator must end before tombstoning rows"
        )]
        let keys = doc.keys(&records).collect::<Vec<_>>();
        for key in keys {
            if let Some((Value::Object(ObjType::Map), row)) = doc.get(&records, &key).ok().flatten()
            {
                ui::doc::put_if_changed(doc, &row, "deleted", true);
            }
        }
        for (rank, source) in recent.iter().enumerate() {
            let id = Self::source_id(source);
            let row = Self::map(doc, &records, &id);
            let Some(row) = row else { continue };
            ui::doc::put_if_changed(doc, &row, "deleted", false);
            ui::doc::put_if_changed(doc, &row, "rank", rank as i64);
            Self::write_source_fields(doc, &row, source);
        }
    }

    /// Add one reviewed source to history without rewriting or tombstoning the
    /// rest of the keyed collection. Concurrent selections therefore retain
    /// both records; the deterministic id breaks an equal-rank tie.
    pub fn remember_recent_source(doc: &mut AutoCommit, source: &Source, rank: i64) {
        let Some(records) = Self::map(doc, &ROOT, "recent_v2") else {
            return;
        };
        let id = Self::source_id(source);
        let Some(row) = Self::map(doc, &records, &id) else {
            return;
        };
        ui::doc::put_if_changed(doc, &row, "deleted", false);
        ui::doc::put_if_changed(doc, &row, "rank", rank);
        Self::write_source_fields(doc, &row, source);
    }

    /// Capture the next MRU order at action-authoring time. Equal concurrent
    /// ranks are intentionally resolved by stable source id on read.
    pub fn next_recent_rank(doc: &AutoCommit) -> i64 {
        Self::map_read(doc, &ROOT, "recent_v2").map_or(0, |records| {
            doc.keys(&records)
                .filter_map(|key| doc.get(&records, &key).ok().flatten())
                .filter_map(|(value, row)| {
                    matches!(value, Value::Object(ObjType::Map)).then_some(row)
                })
                .filter(|row| !read_bool(doc, row, "deleted").unwrap_or(false))
                .map(|row| read_i64_coerced(doc, &row, "rank").unwrap_or_default())
                .min()
                .unwrap_or(0)
                .saturating_sub(1)
        })
    }

    // ---- live install profile --------------------------------------------
    pub fn profile(doc: &AutoCommit) -> InstallProfile {
        Self::profile_map_read(doc)
            .map(|map| Self::read_profile_fields(doc, &map))
            .unwrap_or_default()
    }
    pub fn set_profile(doc: &mut AutoCommit, profile: &InstallProfile) {
        let Some(map) = Self::profile_map(doc) else {
            return;
        };
        Self::write_profile_fields(doc, &map, profile);
    }

    /// Write exactly one non-secret profile field. These are the action
    /// application primitives: they never re-store a stale compound profile.
    pub fn set_profile_mode(doc: &mut AutoCommit, mode: InstallMode) {
        if let Some(map) = Self::profile_map(doc) {
            ui::doc::put_if_changed(doc, &map, "mode", mode.key());
        }
    }
    pub fn set_profile_filesystem(doc: &mut AutoCommit, filesystem: crate::model::Filesystem) {
        if let Some(map) = Self::profile_map(doc) {
            ui::doc::put_if_changed(doc, &map, "filesystem", filesystem.key());
        }
    }
    pub fn set_profile_enroll_local_identity(doc: &mut AutoCommit, on: bool) {
        if let Some(map) = Self::profile_map(doc) {
            ui::doc::put_if_changed(doc, &map, "enroll_local_identity", on);
        }
    }
    pub fn set_profile_encrypt(doc: &mut AutoCommit, on: bool) {
        if let Some(map) = Self::profile_map(doc) {
            ui::doc::put_if_changed(doc, &map, "encrypt", on);
        }
    }
    pub fn set_profile_autologin(doc: &mut AutoCommit, on: bool) {
        if let Some(map) = Self::profile_map(doc)
            && let Some(user) = Self::map(doc, &map, "user")
        {
            ui::doc::put_if_changed(doc, &user, "autologin", on);
        }
    }
    pub fn set_profile_text(doc: &mut AutoCommit, key: &str, value: &str) {
        let Some(map) = Self::profile_map(doc) else {
            return;
        };
        match key {
            "hostname" | "locale" | "timezone" | "keyboard" | "wifi_ssid" | "device_label" => {
                ui::doc::put_if_changed(doc, &map, key, value);
            }
            "full_name" | "username" => {
                if let Some(user) = Self::map(doc, &map, "user") {
                    ui::doc::put_if_changed(doc, &user, key, value);
                }
            }
            _ => {}
        }
    }

    // ---- helpers ----------------------------------------------------------
    fn map_read(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
        let (value, id) = doc.get(parent, key).ok().flatten()?;
        matches!(value, Value::Object(ObjType::Map)).then_some(id)
    }

    fn map(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
        Self::map_read(doc, parent, key).or_else(|| doc.put_object(parent, key, ObjType::Map).ok())
    }

    fn source_map_read(doc: &AutoCommit) -> Option<ObjId> {
        Self::map_read(doc, &ROOT, "source_v2")
    }

    fn source_map(doc: &mut AutoCommit) -> Option<ObjId> {
        Self::map(doc, &ROOT, "source_v2")
    }

    fn profile_map_read(doc: &AutoCommit) -> Option<ObjId> {
        Self::map_read(doc, &ROOT, "profile_v2")
    }

    fn profile_map(doc: &mut AutoCommit) -> Option<ObjId> {
        Self::map(doc, &ROOT, "profile_v2")
    }

    fn write_source_fields(doc: &mut AutoCommit, map: &ObjId, source: &Source) {
        ui::doc::put_if_changed(doc, map, "kind", source.kind.tag());
        ui::doc::put_if_changed(doc, map, "path", source.path.as_str());
        ui::doc::put_if_changed(doc, map, "name", source.name.as_str());
        ui::doc::put_if_changed(doc, map, "size", source.size as i64);
        ui::doc::put_if_changed(doc, map, "label", source.label.as_str());
        ui::doc::put_if_changed(doc, map, "installable", source.installable);
        ui::doc::put_if_changed(
            doc,
            map,
            "identity_volume",
            source
                .identity
                .map_or(-1, |identity| identity.volume as i64),
        );
        ui::doc::put_if_changed(
            doc,
            map,
            "identity_file",
            source.identity.map_or(-1, |identity| identity.file as i64),
        );
        ui::doc::put_if_changed(
            doc,
            map,
            "expected_sha256",
            source.expected_sha256.as_deref().unwrap_or_default(),
        );
    }

    fn read_source_fields(doc: &AutoCommit, map: &ObjId) -> Option<Source> {
        let name = read_str(doc, map, "name").unwrap_or_default();
        // A tombstoned or half-created row is not a usable write source.
        if name.is_empty() {
            return None;
        }
        let volume = read_i64_coerced(doc, map, "identity_volume").unwrap_or_default();
        let file = read_i64_coerced(doc, map, "identity_file").unwrap_or_default();
        let digest = read_str(doc, map, "expected_sha256").unwrap_or_default();
        Some(Source {
            kind: match read_str(doc, map, "kind").unwrap_or_default().as_str() {
                "Tilo" => crate::model::SourceKind::TiloImage,
                _ => crate::model::SourceKind::IsoFile,
            },
            path: read_str(doc, map, "path").unwrap_or_default(),
            name,
            size: read_i64_coerced(doc, map, "size")
                .unwrap_or_default()
                .max(0) as u64,
            label: read_str(doc, map, "label").unwrap_or_default(),
            installable: read_bool(doc, map, "installable").unwrap_or(false),
            identity: (volume >= 0 && file >= 0).then_some(crate::model::SourceIdentity {
                volume: volume as u64,
                file: file as u64,
            }),
            expected_sha256: (!digest.is_empty()).then_some(digest),
        })
    }

    fn write_profile_fields(doc: &mut AutoCommit, map: &ObjId, profile: &InstallProfile) {
        ui::doc::put_if_changed(doc, map, "mode", profile.mode.key());
        ui::doc::put_if_changed(doc, map, "hostname", profile.hostname.as_str());
        ui::doc::put_if_changed(doc, map, "locale", profile.locale.as_str());
        ui::doc::put_if_changed(doc, map, "keyboard", profile.keyboard.as_str());
        ui::doc::put_if_changed(doc, map, "timezone", profile.timezone.as_str());
        ui::doc::put_if_changed(doc, map, "filesystem", profile.filesystem.key());
        ui::doc::put_if_changed(doc, map, "encrypt", profile.encrypt);
        ui::doc::put_if_changed(doc, map, "persist_mb", profile.persist_mb as i64);
        ui::doc::put_if_changed(doc, map, "wifi_ssid", profile.wifi_ssid.as_str());
        ui::doc::put_if_changed(doc, map, "device_label", profile.device_label.as_str());
        ui::doc::put_if_changed(
            doc,
            map,
            "enroll_local_identity",
            profile.enroll_local_identity,
        );
        let Some(user) = Self::map(doc, map, "user") else {
            return;
        };
        ui::doc::put_if_changed(doc, &user, "full_name", profile.user.full_name.as_str());
        ui::doc::put_if_changed(doc, &user, "username", profile.user.username.as_str());
        ui::doc::put_if_changed(doc, &user, "is_admin", profile.user.is_admin);
        ui::doc::put_if_changed(doc, &user, "autologin", profile.user.autologin);
        // Passwords and encryption passphrases deliberately never enter a
        // collaborative document. The installer asks locally on the target.
    }

    fn read_profile_fields(doc: &AutoCommit, map: &ObjId) -> InstallProfile {
        let mut profile = InstallProfile {
            mode: InstallMode::from_key(&read_str(doc, map, "mode").unwrap_or_default()),
            ..InstallProfile::default()
        };
        if let Some(hostname) = read_str(doc, map, "hostname") {
            profile.hostname = hostname;
        }
        if let Some(locale) = read_str(doc, map, "locale") {
            profile.locale = locale;
        }
        if let Some(keyboard) = read_str(doc, map, "keyboard") {
            profile.keyboard = keyboard;
        }
        if let Some(timezone) = read_str(doc, map, "timezone") {
            profile.timezone = timezone;
        }
        if let Some(filesystem) = read_str(doc, map, "filesystem")
            .and_then(|value| crate::model::Filesystem::from_key(&value))
        {
            profile.filesystem = filesystem;
        }
        profile.encrypt = read_bool(doc, map, "encrypt").unwrap_or(false);
        if let Some(ssid) = read_str(doc, map, "wifi_ssid") {
            profile.wifi_ssid = ssid;
        }
        if let Some(label) = read_str(doc, map, "device_label") {
            profile.device_label = label;
        }
        profile.enroll_local_identity =
            read_bool(doc, map, "enroll_local_identity").unwrap_or(false);
        profile.persist_mb = read_i64_coerced(doc, map, "persist_mb")
            .unwrap_or_default()
            .max(0) as u64;
        if let Some(user) = Self::map_read(doc, map, "user") {
            if let Some(full_name) = read_str(doc, &user, "full_name") {
                profile.user.full_name = full_name;
            }
            if let Some(username) = read_str(doc, &user, "username") {
                profile.user.username = username;
            }
            profile.user.is_admin = read_bool(doc, &user, "is_admin").unwrap_or(false);
            profile.user.autologin = read_bool(doc, &user, "autologin").unwrap_or(false);
        }
        profile
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstallMode, SourceKind, sample_targets};

    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        assert!(ImagerDoc::is_empty(&doc));
        ImagerDoc::seed(&mut doc, &sample_targets());
        doc
    }

    /// Blank, demo and idempotent seeding.
    #[test]
    fn seeds_start_blank_or_demo_and_are_idempotent() {
        // Phase `seed_starts_blank_no_fake_content`.
        {
            // The default seed is EMPTY: real drives fill in from a live probe, and
            // there is no fabricated image or recent history.
            let doc = seeded();
            assert!(!ImagerDoc::is_empty(&doc));
            assert!(doc.get(&ROOT, "targets").unwrap().is_none());
            assert!(ImagerDoc::source(&doc).is_none(), "no fabricated image");
            assert!(
                ImagerDoc::recent(&doc).is_empty(),
                "no fabricated recent history"
            );
        }

        // Phase `seed_demo_populates_sample_content`.
        {
            // The opt-in demo seed is the only place sample content comes from.
            let mut doc = AutoCommit::new();
            ImagerDoc::seed_demo(&mut doc);
            assert!(ImagerDoc::source(&doc).is_some());
            assert!(!ImagerDoc::recent(&doc).is_empty());
        }

        // Phase `an_unseeded_document_names_no_drive_and_no_release`.
        // The collaborative document contains no device/release inventory.
        {
            let doc = AutoCommit::new();
            assert!(ImagerDoc::is_empty(&doc));
            assert!(doc.get(&ROOT, "targets").unwrap().is_none());
            assert!(doc.get(&ROOT, "releases").unwrap().is_none());
        }

        // Phase `plain_seed_offers_no_machine_catalogue`.
        {
            let doc = seeded();
            assert!(doc.get(&ROOT, "releases").unwrap().is_none());
        }

        // Phase `seed_is_idempotent_for_collaborative_state`.
        {
            let mut doc = seeded();
            let source = sample_source();
            ImagerDoc::set_source(&mut doc, Some(&source));
            ImagerDoc::seed(&mut doc, &sample_targets()); // re-seed must not clobber
            assert_eq!(ImagerDoc::source(&doc).unwrap().name, source.name);
        }
    }

    /// Source and profile persistence.
    #[test]
    fn document_state_round_trips() {
        // Phase `source_round_trips`.
        {
            let mut doc = seeded();
            assert!(ImagerDoc::source(&doc).is_none());
            let sample = sample_source();
            ImagerDoc::set_source(&mut doc, Some(&sample));
            let src = ImagerDoc::source(&doc).unwrap();
            assert_eq!(src.kind, SourceKind::TiloImage);
            ImagerDoc::set_source(&mut doc, None);
            assert!(ImagerDoc::source(&doc).is_none());
        }

        // Phase `profile_edits_persist`.
        {
            let mut doc = seeded();
            let mut prof = ImagerDoc::profile(&doc);
            prof.hostname = "edited-host".into();
            prof.mode = InstallMode::Persistent;
            prof.filesystem = crate::model::Filesystem::Xfs;
            prof.encrypt = true;
            ImagerDoc::set_profile(&mut doc, &prof);
            let back = ImagerDoc::profile(&doc);
            assert_eq!(back.hostname, "edited-host");
            assert_eq!(back.mode, InstallMode::Persistent);
            assert_eq!(back.filesystem, crate::model::Filesystem::Xfs);
            assert!(back.encrypt);
            assert!(matches!(
                doc.get(&ROOT, "profile_v2").unwrap(),
                Some((Value::Object(ObjType::Map), _))
            ));
            assert!(
                doc.get(&ROOT, "profile").unwrap().is_none(),
                "fresh profile writes never emit an opaque JSON scalar"
            );
        }
    }
}

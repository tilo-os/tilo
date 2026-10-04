//! Import and export: the host's file chooser, the bounded vCard parser
//! and the review of an imported card whose identity collides with one in
//! the book.

use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};

use automerge::AutoCommit;
use ui::portal::{
    FileFilter, MultiOutcome as ImportOutcome, PickRequest, PortalOutcome as ExportOutcome,
};

use super::{ContactsApp, ImportConflict, ParsedImport};
use crate::doc::ContactsDoc;
use crate::model::{Contact, ContactId};
use crate::vcard::{self, VCardVersion};

/// Suggested file name for the export Save dialog.
const EXPORT_FILE_NAME: &str = "contacts-export.vcf";
/// File reads happen on the UI-owned import completion path, so keep them
/// bounded even when an automation supplies a path rather than the picker.
const MAX_IMPORT_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_IMPORT_FILES: usize = 16;
const MAX_IMPORT_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_IMPORT_TOTAL_CARDS: usize = 2_048;
const MAX_IMPORT_CONFLICTS: usize = 64;

fn contacts_import_request() -> PickRequest {
    PickRequest::open_multiple("Import contacts", "Import")
        .thread_name("contacts-portal-import")
        .filter(
            FileFilter::new("vCard")
                .mimetype("text/vcard")
                .mimetype("text/x-vcard")
                .mimetype("text/directory")
                .glob("*.vcf")
                .glob("*.vcard"),
        )
        .all_files()
}

fn contacts_export_request(current_name: &str) -> PickRequest {
    PickRequest::save("Export contacts", "Export", current_name)
        .thread_name("contacts-portal-export")
        .filter(
            FileFilter::new("vCard")
                .mimetype("text/vcard")
                .glob("*.vcf")
                .glob("*.vcard"),
        )
}

#[derive(Debug, Default)]
pub(super) struct ImportSummary {
    pub(super) created: usize,
    pub(super) unchanged: usize,
    pub(super) conflicts: usize,
    pub(super) errors: usize,
}

impl ImportSummary {
    pub(super) fn accepted(&self) -> usize {
        self.created + self.unchanged
    }

    pub(super) fn completed(&self) -> bool {
        self.accepted() > 0
    }

    pub(super) fn failure_message(&self) -> String {
        if self.conflicts > 0 {
            return format!(
                "{} contact{} need{} review before merging",
                self.conflicts,
                if self.conflicts == 1 { "" } else { "s" },
                if self.conflicts == 1 { "s" } else { "" }
            );
        }
        if self.errors > 0 {
            return format!(
                "{} contact{} couldn't be imported",
                self.errors,
                if self.errors == 1 { "" } else { "s" }
            );
        }
        "no contacts were imported".into()
    }
}

/// An imported UID is only an identity hint, never proof that a file is
/// authorised to alter an existing record.  Exact re-imports of our own
/// export are harmless no-ops; a differing payload is surfaced as a conflict
/// for review instead of silently overwriting personal data.
fn same_import_payload(existing: &Contact, incoming: &Contact) -> bool {
    existing.first == incoming.first
        && existing.last == incoming.last
        && existing.org == incoming.org
        && existing.title == incoming.title
        && existing.website == incoming.website
        && existing.birthday == incoming.birthday
        && existing.notes == incoming.notes
        // Favourite is an app-local presentation flag, deliberately absent
        // from interoperable vCard; it must not turn a faithful re-import
        // into a false conflict or be cleared by external data.
        && existing
            .groups
            .iter()
            .map(|entry| &entry.value)
            .eq(incoming.groups.iter().map(|entry| &entry.value))
        && existing
            .phones
            .iter()
            .map(|entry| (&entry.label, &entry.value))
            .eq(incoming.phones.iter().map(|entry| (&entry.label, &entry.value)))
        && existing
            .emails
            .iter()
            .map(|entry| (&entry.label, &entry.value))
            .eq(incoming.emails.iter().map(|entry| (&entry.label, &entry.value)))
        && existing
            .addresses
            .iter()
            .map(|entry| {
                (
                    &entry.label,
                    &entry.street,
                    &entry.city,
                    &entry.region,
                    &entry.postal,
                    &entry.country,
                )
            })
            .eq(incoming.addresses.iter().map(|entry| {
                (
                    &entry.label,
                    &entry.street,
                    &entry.city,
                    &entry.region,
                    &entry.postal,
                    &entry.country,
                )
            }))
}

impl ContactsApp {
    /// Live Import action: open the host's native multi-select file chooser
    /// (XDG portal) filtered to vCard files. Non-blocking — the D-Bus
    /// round-trip and the host dialog run on a worker thread; the chosen paths
    /// arrive over a channel drained by [`drain_portal`]. An in-flight request
    /// is not restarted.
    pub(super) fn request_import(&mut self) {
        if self.import_rx.is_some() || self.import_parse_rx.is_some() {
            return;
        }
        self.import_rx = Some(contacts_import_request().send_multiple());
    }

    /// Read and parse every vCard file in `paths` into the address
    /// book, persisting once if anything was added. An external `UID` is only
    /// used to identify an existing contact: a changed collision is kept out
    /// of the shared document and returned as a review conflict. This is the
    /// hermetic core shared by the live portal
    /// drain and the tests — it never touches the picker, so multi-file import
    /// can be driven directly without a desktop portal.
    #[cfg(test)]
    pub(super) fn import_paths(
        &mut self,
        doc: &mut AutoCommit,
        paths: &[PathBuf],
    ) -> ImportSummary {
        if paths.is_empty() {
            // An empty selection (or a cancelled picker) fabricates nothing.
            return ImportSummary::default();
        }
        let mut summary = ImportSummary::default();
        let mut first_new = None;
        for path in paths {
            let text = match Self::read_import_file(path) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "contacts: import read failed");
                    summary.errors += 1;
                    continue;
                }
            };
            // Each file may hold many, differently-shaped cards; parsing is
            // intentionally bounded so one large or malformed import cannot
            // monopolise the UI completion path.
            let report = vcard::parse_vcards_report(&text);
            summary.errors += report.skipped_cards;
            summary.errors += usize::from(report.limit_reached);
            for c in report.contacts {
                self.absorb_import_contact(doc, c, &mut summary, &mut first_new);
            }
        }
        if let Some(id) = first_new {
            self.selected = Some(id);
        }
        if summary.created > 0 {
            self.show_import_summary(&summary);
            self.list_stale = true;
        } else if summary.unchanged > 0 || summary.conflicts > 0 || summary.errors > 0 {
            self.show_import_summary(&summary);
        } else {
            // A valid but empty/unparseable selection: say so rather than
            // silently doing nothing.
            self.show_toast("No contacts found in the selected file(s)");
        }
        summary
    }

    pub(super) fn absorb_import_contact(
        &mut self,
        doc: &mut AutoCommit,
        c: Contact,
        summary: &mut ImportSummary,
        first_new: &mut Option<ContactId>,
    ) {
        if let Some(existing) = self.store.get(c.id) {
            if same_import_payload(existing, &c) {
                summary.unchanged += 1;
            } else {
                let contact_id = c.id;
                if self.import_conflicts.len() < MAX_IMPORT_CONFLICTS {
                    self.import_conflicts.push(ImportConflict {
                        token: uuid::Uuid::new_v4(),
                        existing: existing.id,
                        incoming: c,
                    });
                    summary.conflicts += 1;
                } else {
                    // Do not claim this collision is reviewable when the
                    // bounded review queue deliberately declined it.
                    summary.errors += 1;
                }
                tracing::warn!(%contact_id, "contacts: import UID collision needs review");
            }
            return;
        }
        let action = crate::actions::create_contact_action(&c);
        match self.try_perform_action(doc, &action) {
            Ok(()) => {
                if first_new.is_none() {
                    *first_new = Some(c.id);
                }
                self.store.upsert(c);
                summary.created += 1;
            }
            Err(error) => {
                summary.errors += 1;
                tracing::warn!(error = %error, "contacts: import action refused");
                self.store.contacts = ContactsDoc::list_contacts(doc);
            }
        }
    }

    pub(super) fn apply_parsed_import(
        &mut self,
        doc: &mut AutoCommit,
        parsed: ParsedImport,
    ) -> ImportSummary {
        let mut summary = ImportSummary {
            errors: parsed.errors,
            ..ImportSummary::default()
        };
        let mut first_new = None;
        for contact in parsed.contacts {
            self.absorb_import_contact(doc, contact, &mut summary, &mut first_new);
        }
        if let Some(id) = first_new {
            self.selected = Some(id);
        }
        if summary.created > 0 {
            self.list_stale = true;
        }
        if summary.created > 0
            || summary.unchanged > 0
            || summary.conflicts > 0
            || summary.errors > 0
        {
            self.show_import_summary(&summary);
        } else {
            self.show_toast("No contacts found in the selected file(s)");
        }
        summary
    }

    pub(super) fn start_import_parse(&mut self, paths: Vec<PathBuf>) -> Result<(), String> {
        if paths.len() > MAX_IMPORT_FILES {
            return Err(format!(
                "select at most {MAX_IMPORT_FILES} vCard files at once"
            ));
        }
        let mut total = 0_u64;
        for path in &paths {
            let metadata = file_metadata(path)?;
            if metadata.kind != tilo_files::Kind::File {
                return Err("selected path is not a regular file".into());
            }
            total = total.saturating_add(metadata.len);
            if total > MAX_IMPORT_TOTAL_BYTES {
                return Err(format!(
                    "selected files exceed the {MAX_IMPORT_TOTAL_BYTES}-byte import limit"
                ));
            }
        }
        let generation = self.advance_import_generation()?;
        let (sender, receiver) = mpsc::sync_channel(1);
        ui::rt::spawn_thread("contacts-vcard-parser", move || {
            let mut contacts = Vec::new();
            let mut errors = 0;
            for path in paths {
                match Self::read_import_file(&path) {
                    Ok(text) => {
                        let report = vcard::parse_vcards_report(&text);
                        errors += report.skipped_cards + usize::from(report.limit_reached);
                        for c in report.contacts {
                            if contacts.len() < MAX_IMPORT_TOTAL_CARDS {
                                contacts.push(c);
                            } else {
                                errors += 1;
                                break;
                            }
                        }
                    }
                    Err(_) => errors += 1,
                }
            }
            let _ = sender.send(ParsedImport {
                generation,
                contacts,
                errors,
            });
        })
        .map_err(|error| error.to_string())?;
        self.import_parse_rx = Some(receiver);
        Ok(())
    }

    /// Allocate a parser generation without ever reusing the initial or a
    /// previously-issued value.  A wrap would let a delayed worker from an
    /// ancient import look current, so exhaustion permanently closes imports
    /// for this app instance instead of silently returning to generation 0.
    pub(super) fn advance_import_generation(&mut self) -> Result<u64, String> {
        let generation = self
            .import_generation
            .checked_add(1)
            .ok_or_else(|| "restart Contacts to import again".to_string())?;
        self.import_generation = generation;
        Ok(generation)
    }

    pub(super) fn read_import_file(path: &std::path::Path) -> Result<String, String> {
        let metadata = file_metadata(path)?;
        if metadata.kind != tilo_files::Kind::File {
            return Err("selected path is not a regular file".into());
        }
        if metadata.len > MAX_IMPORT_FILE_BYTES {
            return Err(format!(
                "file exceeds the {MAX_IMPORT_FILE_BYTES}-byte import limit"
            ));
        }
        let bytes = tilo_files::file_at(path)
            .and_then(|(dir, name)| tilo_files::blocking::read(&dir, &name))
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_IMPORT_FILE_BYTES {
            return Err(format!(
                "file exceeds the {MAX_IMPORT_FILE_BYTES}-byte import limit"
            ));
        }
        String::from_utf8(bytes).map_err(|_| "vCard file is not valid UTF-8 text".into())
    }

    pub(super) fn show_import_summary(&mut self, summary: &ImportSummary) {
        let mut parts = Vec::new();
        if summary.created > 0 {
            parts.push(format!(
                "Imported {} contact{}",
                summary.created,
                if summary.created == 1 { "" } else { "s" }
            ));
        }
        if summary.unchanged > 0 {
            parts.push(format!("{} already current", summary.unchanged));
        }
        if summary.conflicts > 0 {
            parts.push(format!(
                "{} need{} review before merging",
                summary.conflicts,
                if summary.conflicts == 1 { "s" } else { "" }
            ));
        }
        if summary.errors > 0 {
            parts.push(format!("{} couldn't be imported", summary.errors));
        }
        self.show_toast(parts.join("; "));
    }

    /// Whether a file-chooser portal (import or export) is currently awaiting
    /// the user's choice. Keeps the host loop pumping frames so the outcome is
    /// drained promptly once the dialog closes.
    pub(super) fn portal_in_flight(&self) -> bool {
        self.import_rx.is_some() || self.import_parse_rx.is_some() || self.export_rx.is_some()
    }

    pub(super) fn finish_resource_file(&mut self, success: bool, error: Option<String>) {
        let Some(invocation) = self.resource_invocation.take() else {
            return;
        };
        self.completed_resource_invocation = Some(if success {
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            }
        } else if let Some(message) = error {
            tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "contacts.file_failed".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            }
        } else {
            tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("file selection cancelled".into()),
            }
        });
    }

    /// Drain any in-flight import/export portal request. Called once per frame
    /// from [`Widget::sync`]; a `try_recv` never blocks the UI thread. When an
    /// outcome arrives the receiver is dropped (idle again).
    pub(super) fn drain_portal(&mut self, doc: &mut AutoCommit) {
        if let Some(rx) = &self.import_rx {
            match rx.try_recv() {
                Ok(ImportOutcome::Selected(paths)) => {
                    self.import_rx = None;
                    if let Err(error) = self.start_import_parse(paths) {
                        self.show_toast(format!("Couldn't start import: {error}"));
                        self.finish_resource_file(false, Some(error));
                    }
                }
                Ok(ImportOutcome::Cancelled) => {
                    // A dismissed picker is a silent no-op — nothing to say.
                    self.import_rx = None;
                    self.finish_resource_file(false, None);
                }
                Ok(ImportOutcome::Failed(reason)) => {
                    self.import_rx = None;
                    self.show_toast(format!("Import unavailable: {reason}"));
                    self.finish_resource_file(false, Some(reason));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.import_rx = None;
                    self.finish_resource_file(
                        false,
                        Some("import picker stopped unexpectedly".into()),
                    );
                }
            }
        }
        if let Some(rx) = &self.import_parse_rx {
            match rx.try_recv() {
                Ok(parsed) => {
                    self.import_parse_rx = None;
                    if parsed.generation == self.import_generation {
                        let imported = self.apply_parsed_import(doc, parsed);
                        let success = imported.completed();
                        self.finish_resource_file(
                            success,
                            (!success).then(|| imported.failure_message()),
                        );
                    } else {
                        // A superseded parser is never allowed to apply its
                        // contacts.  It must still settle a previously
                        // accepted broker invocation rather than leaving that
                        // job pending forever.
                        self.finish_resource_file(
                            false,
                            Some("import was superseded before parsing finished".into()),
                        );
                    }
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.import_parse_rx = None;
                    self.finish_resource_file(
                        false,
                        Some("import parser stopped unexpectedly".into()),
                    );
                }
            }
        }
        if let Some(rx) = &self.export_rx {
            match rx.try_recv() {
                Ok(ExportOutcome::Opened(path)) => {
                    self.export_rx = None;
                    if let Some((ids, version)) = self.pending_export.take() {
                        self.write_export(&ids, version, &path);
                        self.finish_resource_file(
                            path.is_file(),
                            (!path.is_file()).then(|| "vCard export was not written".into()),
                        );
                    }
                }
                Ok(ExportOutcome::Cancelled) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.export_rx = None;
                    self.pending_export = None;
                    self.finish_resource_file(false, None);
                }
                Ok(ExportOutcome::Failed(reason)) => {
                    self.export_rx = None;
                    self.pending_export = None;
                    self.show_toast(format!("Export unavailable: {reason}"));
                    self.finish_resource_file(false, Some(reason));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    /// Ask how to export the people the list shows now: every contact,
    /// the favourites, or one group.
    pub(super) fn open_export_dialog(&mut self) {
        let ids = self.visible_ids();
        let summary = match ids.len() {
            0 => return self.show_toast("There are no contacts to export"),
            1 => "1 contact is saved as a vCard file.".to_owned(),
            n => match self.destination_label() {
                Some(group) => format!("{n} contacts in {group} are saved as one vCard file."),
                None => format!("{n} contacts are saved as one vCard file."),
            },
        };
        self.open_export(ids, summary);
    }

    /// Live Export action: park the chosen `ids` + vCard `version`, then open
    /// the host's native Save dialog (XDG portal) with a suggested file name.
    /// Non-blocking — when the portal returns a destination, [`drain_portal`]
    /// writes the file via [`write_export`]. An in-flight request is not
    /// restarted. Returns `false` (nothing parked) when the selection is empty.
    pub(super) fn request_export(&mut self, ids: Vec<ContactId>, version: VCardVersion) -> bool {
        if ids.is_empty() {
            self.show_toast("Select at least one contact to export");
            return false;
        }
        if self.export_rx.is_some() {
            return false;
        }
        self.pending_export = Some((ids, version));
        self.export_rx = Some(contacts_export_request(EXPORT_FILE_NAME).send());
        true
    }

    /// Start the version-selection workflow for one canonical contact
    /// resource.  This is intentionally the only path behind both the
    /// contact resource's `export-contact` operation and the row context
    /// menu: a stale/busy check therefore has identical behavior for agents,
    /// keyboard, assistive technology, and pointer input.
    pub(super) fn open_contact_export(&mut self, id: ContactId) -> Result<(), String> {
        if self.store.get(id).is_none() {
            return Err("contact no longer exists".into());
        }
        if self.export.is_some() || self.export_rx.is_some() || self.resource_invocation.is_some() {
            return Err("a contact export is already in progress".into());
        }
        let name = self
            .store
            .get(id)
            .map(Contact::display_name)
            .unwrap_or_default();
        self.open_export(vec![id], format!("{name} is saved as a vCard file."));
        Ok(())
    }

    /// Serialize the same canonical operation published by the contact
    /// resource, then route it through the normal decoder/validator.  The
    /// context-menu affordance intentionally does not call
    /// [`Self::open_contact_export`] directly: this keeps pointer, keyboard,
    /// AT-SPI and agent activation on one typed operation boundary.
    pub(super) fn invoke_contact_export_operation(
        &mut self,
        state: &AutoCommit,
        id: ContactId,
    ) -> Result<bool, String> {
        let operation = crate::actions::export_contact_operation(id);
        let payload = serde_json::to_vec(&operation)
            .map_err(|error| format!("couldn't encode contact export: {error}"))?;
        self.answer_resource_operation(
            tilo_actions::OperationKind::Effect,
            crate::actions::CONTACTS_OPERATION_TYPE,
            &payload,
            state,
            &automerge::ROOT,
        )
    }

    /// Open the locally surfaced candidate through the exact typed operation
    /// owned by its canonical duplicate resource. The toolbar is therefore a
    /// discoverability affordance, not a second private merge path: global
    /// search, an agent, keyboard semantics, and pointer activation all land
    /// in the same field-by-field review surface.
    pub(super) fn invoke_duplicate_review_operation(
        &mut self,
        state: &AutoCommit,
    ) -> Result<bool, String> {
        let candidate = self
            .duplicate_candidate
            .as_ref()
            .ok_or_else(|| "there are no duplicate contacts to review".to_string())?;
        let operation = crate::actions::open_duplicate_review_operation(candidate);
        let payload = serde_json::to_vec(&operation)
            .map_err(|error| format!("couldn't encode duplicate review: {error}"))?;
        self.answer_resource_operation(
            tilo_actions::OperationKind::View,
            crate::actions::CONTACTS_OPERATION_TYPE,
            &payload,
            state,
            &automerge::ROOT,
        )
    }

    /// Write the chosen `ids` out as a `.vcf` of `version` to `path`, reporting
    /// the real destination. Hermetic core shared by the live portal drain and
    /// the tests — it takes an explicit path and never consults the picker or
    /// an env var.
    pub(super) fn write_export(
        &mut self,
        ids: &[ContactId],
        version: VCardVersion,
        path: &std::path::Path,
    ) {
        let contacts: Vec<Contact> = ids
            .iter()
            .filter_map(|id| self.store.get(*id).cloned())
            .collect();
        if contacts.is_empty() {
            self.show_toast("Select at least one contact to export");
            return;
        }
        let body = vcard::to_vcards(&contacts, version);
        let written = tilo_files::file_at(path).and_then(|(dir, name)| {
            tilo_files::blocking::create_dir_all(&dir, &tilo_files::VPath::root())?;
            tilo_files::blocking::save_document(&dir, &name, body.as_bytes())
        });
        match written {
            Ok(()) => self.show_toast(format!(
                "Exported {} ({}) → {}",
                contacts.len(),
                version.label(),
                path.display()
            )),
            Err(e) => self.show_toast(format!("Export failed: {e}")),
        }
    }
}

/// Size and kind of a file a chooser named.
fn file_metadata(path: &std::path::Path) -> Result<tilo_files::Meta, String> {
    tilo_files::file_at(path)
        .and_then(|(dir, name)| tilo_files::blocking::metadata(&dir, &name))
        .map_err(|error| error.to_string())
}

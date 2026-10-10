//! Import and export: the host's file chooser, the bounded vCard parser
//! and the review of an imported card whose identity collides with one in
//! the book.

#[cfg(test)]
use super::file_metadata;
use crate::resource::Contacts;
#[cfg(test)]
use std::path::PathBuf;
use std::rc::Rc;
use ui::framework::ChosenFile;

use ui::portal::{FileFilter, PickRequest};

use super::{ContactsApp, FileWork, FileWorkGuard, ImportConflict, ParsedImport};

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

pub(super) fn contacts_import_request() -> PickRequest {
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
    pub(super) fn request_import(&self) {
        if self.file_work.get().is_some() {
            return;
        }
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(&crate::actions::ContactsEffect::ImportVcard);
        }
    }
    #[cfg(test)]
    pub(super) fn import_paths(&mut self, paths: &[PathBuf]) -> ImportSummary {
        if paths.is_empty() {
            // An empty selection (or a cancelled picker) fabricates nothing.
            return ImportSummary::default();
        }
        let mut summary = ImportSummary::default();
        let mut first_new = None;
        let mut cards = Vec::new();
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
            cards.extend(report.contacts);
        }
        self.absorb_import_contacts(cards, &mut summary, &mut first_new);
        if let Some(id) = first_new {
            self.select_awaited(id);
        }
        if summary.created > 0 {
            self.show_import_summary(&summary);
            self.layout_dirty = true;
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
    /// Add the imported `cards` the book does not hold yet, as one change;
    /// a card whose id names a contact already is unchanged or waits for
    /// review, and one whose contact was deleted is not brought back.
    pub(super) fn absorb_import_contacts(
        &mut self,
        cards: impl IntoIterator<Item = Contact>,
        summary: &mut ImportSummary,
        first_new: &mut Option<ContactId>,
    ) {
        let mut added = Vec::new();
        for c in cards {
            // A card earlier in this import counts as held already.
            let held = self
                .store
                .contact(c.id)
                .or_else(|| added.iter().find(|card: &&Contact| card.id == c.id));
            if let Some(existing) = held {
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
                continue;
            }
            if self.store.removed.contains(&c.id) {
                summary.errors += 1;
                tracing::warn!(contact_id = %c.id, "contacts: import of a deleted contact");
                continue;
            }
            added.push(c);
        }
        if added.is_empty() {
            return;
        }
        let count = added.len();
        let first = added.first().map(|contact| contact.id);
        match self.submit(crate::resource::Contacts::add_all(added)) {
            Ok(()) => {
                if first_new.is_none() {
                    *first_new = first;
                }
                summary.created += count;
            }
            Err(error) => {
                summary.errors += count;
                tracing::warn!(error = %error, "contacts: import action refused");
            }
        }
    }
    pub(crate) fn open_cards(&mut self, text: String) {
        self.opened_cards = Some(text);
    }
    pub(super) fn import_opened_cards(&mut self) {
        let Some(text) = self.opened_cards.take() else {
            return;
        };
        let report = vcard::parse_vcards_report(&text);
        let mut contacts = report.contacts;
        let overflow = contacts.len().saturating_sub(MAX_IMPORT_TOTAL_CARDS);
        contacts.truncate(MAX_IMPORT_TOTAL_CARDS);
        self.apply_parsed_import(ParsedImport {
            contacts,
            errors: report.skipped_cards + usize::from(report.limit_reached) + overflow,
        });
    }
    pub(super) fn apply_parsed_import(&mut self, parsed: ParsedImport) -> ImportSummary {
        let mut summary = ImportSummary {
            errors: parsed.errors,
            ..ImportSummary::default()
        };
        let mut first_new = None;
        self.absorb_import_contacts(parsed.contacts, &mut summary, &mut first_new);
        if let Some(id) = first_new {
            self.select_awaited(id);
        }
        if summary.created > 0 {
            self.layout_dirty = true;
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
    pub(super) fn import_chosen(
        files: Vec<ChosenFile>,
        work: FileWorkGuard,
        cx: &mut ui::framework::EffectCx<Contacts, Self>,
    ) -> ui::framework::EffectAnswer {
        if files.is_empty() {
            return Ok(ui::framework::Effected::done());
        }
        if files.len() > MAX_IMPORT_FILES {
            return Err(format!(
                "select at most {MAX_IMPORT_FILES} vCard files at once"
            ));
        }
        let total = files
            .iter()
            .map(|file| file.bytes.len() as u64)
            .sum::<u64>();
        if total > MAX_IMPORT_TOTAL_BYTES {
            return Err(format!(
                "selected files exceed the {MAX_IMPORT_TOTAL_BYTES}-byte import limit"
            ));
        }
        Ok(cx.thread(
            move |_| parse_chosen(files),
            move |app: &mut Self, parsed, _| {
                drop(work);
                let summary = app.apply_parsed_import(parsed);
                if summary.completed() {
                    Ok(ui::framework::Effected::done())
                } else {
                    Err(summary.failure_message())
                }
            },
        ))
    }
    pub(super) fn begin_file_work(&self, work: FileWork) -> Result<FileWorkGuard, String> {
        if self.file_work.get().is_some() {
            return Err("another contact file operation is running".into());
        }
        self.file_work.set(Some(work));
        Ok(FileWorkGuard(Rc::clone(&self.file_work)))
    }
    pub(super) fn export_bytes(
        &self,
        ids: &[ContactId],
        version: VCardVersion,
    ) -> Result<(Vec<u8>, usize), String> {
        let contacts: Vec<Contact> = ids
            .iter()
            .filter_map(|id| self.store.contact(*id).cloned())
            .collect();
        if contacts.is_empty() {
            return Err("there are no contacts to export".into());
        }
        Ok((
            vcard::to_vcards(&contacts, version).into_bytes(),
            contacts.len(),
        ))
    }
    pub(super) fn export_to_chosen_file(
        &self,
        ids: &[ContactId],
        version: VCardVersion,
        cx: &mut ui::framework::EffectCx<Contacts, Self>,
    ) -> ui::framework::EffectAnswer {
        let (bytes, count) = self.export_bytes(ids, version)?;
        let work = self.begin_file_work(FileWork::Export)?;
        Ok(Self::save_export(bytes, count, version, work, cx))
    }
    pub(super) fn save_export(
        bytes: Vec<u8>,
        count: usize,
        version: VCardVersion,
        work: FileWorkGuard,
        cx: &mut ui::framework::EffectCx<Contacts, Self>,
    ) -> ui::framework::Effected {
        cx.save_file(
            contacts_export_request(EXPORT_FILE_NAME),
            bytes,
            move |app, saved, _| {
                drop(work);
                if saved {
                    app.show_toast(format!(
                        "Exported {count} contact{} ({})",
                        if count == 1 { "" } else { "s" },
                        version.label()
                    ));
                }
                Ok(ui::framework::Effected::done())
            },
        )
    }
    #[cfg(test)]
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
    pub(super) fn portal_in_flight(&self) -> bool {
        self.file_work.get().is_some()
    }
    pub(super) fn request_export(&mut self, ids: &[ContactId], version: VCardVersion) -> bool {
        if ids.is_empty() {
            self.show_toast("Select at least one contact to export");
            return false;
        }
        if self.file_work.get().is_some() {
            return false;
        }
        let Some(jobs) = self.jobs.clone() else {
            return false;
        };
        let (bytes, count) = match self.export_bytes(ids, version) {
            Ok(export) => export,
            Err(error) => {
                self.show_toast(format!("Export failed: {error}"));
                return false;
            }
        };
        let Ok(work) = self.begin_file_work(FileWork::Export) else {
            return false;
        };
        jobs.start::<Self>("export", move |cx| {
            Ok(Self::save_export(bytes, count, version, work, cx))
        });
        true
    }
}
fn parse_chosen(files: Vec<ChosenFile>) -> ParsedImport {
    let mut contacts = Vec::new();
    let mut errors = 0;
    for file in files {
        let Ok(text) = String::from_utf8(file.bytes) else {
            errors += 1;
            continue;
        };
        if text.len() as u64 > MAX_IMPORT_FILE_BYTES {
            errors += 1;
            continue;
        }
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
    ParsedImport { contacts, errors }
}

impl ContactsApp {
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
}

impl ContactsApp {
    pub(super) fn open_contact_export(&mut self, id: ContactId) -> Result<(), String> {
        if self.store.contact(id).is_none() {
            return Err("contact no longer exists".into());
        }
        if self.export.is_some() || self.portal_in_flight() {
            return Err("a contact export is already in progress".into());
        }
        let name = self
            .store
            .contact(id)
            .map(Contact::display_name)
            .unwrap_or_default();
        self.open_export(vec![id], format!("{name} is saved as a vCard file."));
        Ok(())
    }
}

impl ContactsApp {
    pub(super) fn invoke_contact_export_operation(
        &self,
        _state: &ui::widget::Edits<'_>,
        id: ContactId,
    ) -> Result<bool, String> {
        if self.store.contact(id).is_none() {
            return Err("contact no longer exists".into());
        }
        let dispatcher = self
            .dispatcher
            .as_ref()
            .ok_or("the address book is not open")?;
        dispatcher.effect(&crate::actions::ContactsEffect::ExportContact { contact: id });
        Ok(true)
    }
}

impl ContactsApp {
    pub(super) fn invoke_duplicate_review_operation(
        &self,
        _state: &ui::widget::Edits<'_>,
    ) -> Result<bool, String> {
        let candidate = self
            .duplicate_candidate
            .as_ref()
            .ok_or("there are no duplicate contacts to review")?;
        let dispatcher = self
            .dispatcher
            .as_ref()
            .ok_or("the address book is not open")?;
        dispatcher.effect_on(
            &crate::resource::place_resource_id(&crate::resource::duplicate_place(
                candidate.survivor_id,
                candidate.source_id,
            )),
            &crate::actions::ContactsEffect::OpenReview {
                survivor: candidate.survivor_id,
                source: candidate.source_id,
            },
        );
        Ok(true)
    }
}

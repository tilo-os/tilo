//! The window's decisions: deleting a contact, leaving unsaved edits, an
//! imported card whose identity collides with one in the book, merging a
//! duplicate, and the format of an export. Each is a [`ModalDialog`] in the
//! window's top layer; its buttons queue a [`DialogChoice`] that the app
//! applies to whichever decision is open.

use std::collections::HashSet;

use automerge::AutoCommit;
use ui::widget::{
    Column, ControlSize, Dropdown, Element, Field, LabelPlacement, ModalDialog, Text,
};
use ui::{LocalState, TextRole};

use super::{ContactsApp, DuplicateReview};
use crate::actions::MergeField;
use crate::model::{Contact, ContactId};
use crate::vcard::VCardVersion;
use crate::view::{DialogChoice, Request, Shared, queue};

/// Which decision a dialog asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DialogKey {
    Delete(ContactId),
    Unsaved(ContactId),
    ImportConflict(uuid::Uuid),
    Duplicate(ContactId, ContactId),
    Export,
}

/// The export formats, newest first: the stored value and the label.
const FORMATS: &[(&str, &str)] = &[("4.0", "vCard 4.0"), ("3.0", "vCard 3.0, for older apps")];

/// The export the person is choosing a format for.
pub(super) struct ExportDraft {
    pub(super) ids: Vec<ContactId>,
    /// One sentence: what is exported.
    pub(super) summary: String,
    pub(super) format: LocalState<String>,
}

impl ExportDraft {
    pub(super) fn version(&self) -> VCardVersion {
        if self.format.with(|format| format == "3.0") {
            VCardVersion::V3
        } else {
            VCardVersion::V4
        }
    }
}

fn body(text: &str) -> Text {
    Text::new(text).with_role(TextRole::Body)
}

fn dialog<C: ui::widget::Widget + 'static>(
    title: &str,
    content: C,
    shared: &Shared,
    primary: &str,
    secondary: &str,
) -> ModalDialog<C> {
    ModalDialog::new(title, content)
        .open(true)
        .primary_action(
            primary,
            queue(shared, Request::Dialog(DialogChoice::Primary)),
        )
        .secondary_action(
            secondary,
            queue(shared, Request::Dialog(DialogChoice::Secondary)),
        )
        .on_dismiss(queue(shared, Request::Dialog(DialogChoice::Dismiss)))
}

pub(super) fn duplicate_field_label(field: MergeField) -> &'static str {
    match field {
        MergeField::First => "First name",
        MergeField::Last => "Last name",
        MergeField::Organization => "Organization",
        MergeField::Title => "Job title",
        MergeField::Website => "Website",
        MergeField::Birthday => "Birthday",
        MergeField::Notes => "Notes",
    }
}

pub(super) fn duplicate_field_value(contact: &Contact, field: MergeField) -> String {
    match field {
        MergeField::First => contact.first.clone(),
        MergeField::Last => contact.last.clone(),
        MergeField::Organization => contact.org.clone(),
        MergeField::Title => contact.title.clone(),
        MergeField::Website => contact.website.clone(),
        MergeField::Birthday => contact
            .birthday
            .map(|birthday| birthday.pretty())
            .unwrap_or_default(),
        MergeField::Notes => contact.notes.clone(),
    }
}

/// What two duplicate cards have in common, as a phrase: "an email, a
/// phone number and a name".
pub(super) fn shared_facts(reasons: &[crate::actions::DuplicateReason]) -> String {
    use crate::actions::DuplicateReason;
    let facts: Vec<&str> = reasons
        .iter()
        .map(|reason| match reason {
            DuplicateReason::Email => "an email",
            DuplicateReason::Phone => "a phone number",
            DuplicateReason::Name => "a name",
        })
        .collect();
    match facts.split_last() {
        None => "their details".to_owned(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// The single-value fields two duplicates disagree on.
pub(super) fn duplicate_conflicts(survivor: &Contact, source: &Contact) -> Vec<MergeField> {
    let differs = |left: &str, right: &str| {
        !left.trim().is_empty() && !right.trim().is_empty() && left != right
    };
    let mut fields = Vec::new();
    for (field, left, right) in [
        (MergeField::First, &survivor.first, &source.first),
        (MergeField::Last, &survivor.last, &source.last),
        (MergeField::Organization, &survivor.org, &source.org),
        (MergeField::Title, &survivor.title, &source.title),
        (MergeField::Website, &survivor.website, &source.website),
        (MergeField::Notes, &survivor.notes, &source.notes),
    ] {
        if differs(left, right) {
            fields.push(field);
        }
    }
    if survivor.birthday.is_some()
        && source.birthday.is_some()
        && survivor.birthday != source.birthday
    {
        fields.push(MergeField::Birthday);
    }
    fields
}

impl DuplicateReview {
    /// Open a review of `source` merging into `survivor`, keeping the
    /// survivor's value of every field until the person chooses otherwise.
    pub(super) fn new(
        survivor: Contact,
        source: Contact,
        reasons: Vec<crate::actions::DuplicateReason>,
    ) -> Self {
        let choices = duplicate_conflicts(&survivor, &source)
            .into_iter()
            .map(|field| (field, LocalState::new("current".to_owned())))
            .collect();
        Self {
            survivor,
            source,
            reasons,
            choices,
        }
    }

    /// The fields whose value comes from the contact that is merged away.
    pub(super) fn take_from_source(&self) -> HashSet<MergeField> {
        self.choices
            .iter()
            .filter(|(_, choice)| choice.with(|value| value == "other"))
            .map(|(field, _)| *field)
            .collect()
    }

    /// Keep the merged-away contact's value of `field` (or the survivor's).
    #[cfg(test)]
    pub(super) fn choose(&self, field: MergeField, take_source: bool) {
        if let Some((_, choice)) = self.choices.iter().find(|(f, _)| *f == field) {
            choice.set(if take_source { "other" } else { "current" }.to_owned());
        }
    }
}

impl ContactsApp {
    /// The decision the window asks for now, topmost first.
    pub(super) fn wanted_dialog(&self) -> Option<DialogKey> {
        if let Some(review) = &self.duplicate_review {
            return Some(DialogKey::Duplicate(review.survivor.id, review.source.id));
        }
        if let Some(conflict) = self.import_conflicts.first() {
            return Some(DialogKey::ImportConflict(conflict.token));
        }
        if self.export.is_some() {
            return Some(DialogKey::Export);
        }
        if let Some(target) = self.pending_switch {
            return Some(DialogKey::Unsaved(target));
        }
        self.confirm_delete.map(DialogKey::Delete)
    }

    fn name_of(&self, id: ContactId) -> String {
        self.store
            .get(id)
            .map_or_else(|| "this contact".to_owned(), Contact::display_name)
    }

    /// The dialog that asks for `key`.
    pub(super) fn build_dialog(&self, key: &DialogKey) -> Option<Element> {
        let shared = &self.shared;
        let element: Element = match key {
            DialogKey::Delete(id) => Box::new(
                dialog(
                    &format!("Delete {}?", self.name_of(*id)),
                    body("The contact is removed from your address book."),
                    shared,
                    "Delete contact",
                    "Cancel",
                )
                .with_destructive(true)
                .with_test_id("contacts.delete-dialog"),
            ),
            DialogKey::Unsaved(_) => {
                let name = self
                    .form
                    .as_ref()
                    .map(|form| form.draft.display_name())
                    .unwrap_or_default();
                Box::new(
                    dialog(
                        &format!("Save changes to {name}?"),
                        body("Your changes are lost if you don't save them."),
                        shared,
                        "Save",
                        "Don't save",
                    )
                    .with_test_id("contacts.unsaved-dialog"),
                )
            }
            DialogKey::ImportConflict(token) => {
                let conflict = self
                    .import_conflicts
                    .iter()
                    .find(|conflict| conflict.token == *token)?;
                let existing = self.name_of(conflict.existing);
                Box::new(
                    dialog(
                        &format!("Import {} as a new contact?", conflict.incoming.display_name()),
                        body(&format!(
                            "The imported card has the same identity as {existing} in your book, but different details."
                        )),
                        shared,
                        "Import as new",
                        "Keep existing",
                    )
                    .with_test_id("contacts.import-dialog"),
                )
            }
            DialogKey::Duplicate(..) => self.duplicate_dialog()?,
            DialogKey::Export => self.export_dialog()?,
        };
        Some(element)
    }

    fn duplicate_dialog(&self) -> Option<Element> {
        let review = self.duplicate_review.as_ref()?;
        let mut content = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(body(&format!(
                "These cards share {}. Every phone number, email, address and group is kept.",
                shared_facts(&review.reasons)
            ))));
        for (field, choice) in &review.choices {
            let current = duplicate_field_value(&review.survivor, *field);
            let other = duplicate_field_value(&review.source, *field);
            let mut menu = Dropdown::from_state(vec![current.clone(), other.clone()], choice)
                .with_id(&format!("contacts.duplicate.{field:?}"))
                .control_size(ControlSize::Large);
            menu.set_value_options(vec![
                ("current".to_owned(), current),
                ("other".to_owned(), other),
            ]);
            content = content.push(Box::new(
                Field::new(duplicate_field_label(*field), menu)
                    .with_label_placement(Some(LabelPlacement::Above)),
            ));
        }
        let (source, survivor) = (review.source.display_name(), review.survivor.display_name());
        let title = if source == survivor {
            format!("Merge the two cards for {survivor}?")
        } else {
            format!("Merge {source} into {survivor}?")
        };
        Some(Box::new(
            dialog(&title, content, &self.shared, "Merge contacts", "Cancel")
                .with_destructive(true)
                .with_test_id("contacts.duplicate-dialog"),
        ))
    }

    fn export_dialog(&self) -> Option<Element> {
        let export = self.export.as_ref()?;
        let mut format = Dropdown::from_state(
            FORMATS
                .iter()
                .map(|(_, label)| (*label).to_owned())
                .collect(),
            &export.format,
        )
        .with_id("contacts.export.format")
        .control_size(ControlSize::Large);
        format.set_value_options(
            FORMATS
                .iter()
                .map(|(value, label)| ((*value).to_owned(), (*label).to_owned()))
                .collect(),
        );
        let content = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(body(&export.summary)))
            .push(Box::new(Field::new("Format", format)));
        Some(Box::new(
            dialog("Export contacts", content, &self.shared, "Export", "Cancel")
                .with_test_id("contacts.export-dialog"),
        ))
    }

    /// Apply the person's answer to the open decision.
    pub(super) fn answer_dialog(&mut self, doc: &mut AutoCommit, choice: DialogChoice) {
        let Some(key) = self.wanted_dialog() else {
            return;
        };
        match (key, choice) {
            (DialogKey::Delete(id), DialogChoice::Primary) => self.delete_selected(doc, id),
            (DialogKey::Delete(_), _) => self.confirm_delete = None,
            (DialogKey::Unsaved(target), DialogChoice::Primary) => {
                self.pending_switch = None;
                self.save_edit(doc);
                if self.mode == super::Mode::View {
                    self.commit_select(target);
                }
            }
            (DialogKey::Unsaved(target), DialogChoice::Secondary) => {
                self.pending_switch = None;
                self.commit_select(target);
            }
            (DialogKey::Unsaved(_), DialogChoice::Dismiss) => self.pending_switch = None,
            (DialogKey::ImportConflict(token), DialogChoice::Primary | DialogChoice::Secondary) => {
                let import_as_new = choice == DialogChoice::Primary;
                if let Err(error) =
                    self.resolve_import_conflict(doc, &token.to_string(), import_as_new)
                {
                    self.show_toast(format!("Couldn't resolve the import: {error}"));
                }
            }
            // Escape decides nothing: the collision waits for an answer.
            (DialogKey::ImportConflict(_), DialogChoice::Dismiss) => {}
            (DialogKey::Duplicate(..), DialogChoice::Primary) => self.merge_duplicate_review(doc),
            (DialogKey::Duplicate(..), _) => self.duplicate_review = None,
            (DialogKey::Export, DialogChoice::Primary) => {
                if let Some(export) = self.export.take() {
                    let version = export.version();
                    self.request_export(export.ids, version);
                }
            }
            (DialogKey::Export, _) => self.export = None,
        }
        self.dirty = true;
    }

    pub(super) fn open_export(&mut self, ids: Vec<ContactId>, summary: String) {
        self.export = Some(ExportDraft {
            ids,
            summary,
            format: LocalState::new("4.0".to_owned()),
        });
        self.dirty = true;
    }

    pub(super) fn open_duplicate_review(
        &mut self,
        survivor_id: ContactId,
        source_id: ContactId,
    ) -> Result<(), String> {
        let survivor = self
            .store
            .get(survivor_id)
            .cloned()
            .ok_or_else(|| "surviving contact no longer exists".to_string())?;
        let source = self
            .store
            .get(source_id)
            .cloned()
            .ok_or_else(|| "source contact no longer exists".to_string())?;
        let candidate = crate::actions::duplicate_candidate(&survivor, &source)
            .ok_or_else(|| "contacts are no longer a likely duplicate".to_string())?;
        self.selected = Some(survivor_id);
        self.duplicate_review = Some(DuplicateReview::new(survivor, source, candidate.reasons));
        self.menu.close();
        self.dirty = true;
        Ok(())
    }

    pub(super) fn merge_duplicate_review(&mut self, state: &mut AutoCommit) {
        let Some(review) = self.duplicate_review.take() else {
            return;
        };
        let action = crate::actions::merge_contacts_action(
            &review.survivor,
            &review.source,
            &review.take_from_source(),
        );
        match self.try_perform_action(state, &action) {
            Ok(()) => {
                self.selected = Some(review.survivor.id);
                self.show_toast(format!(
                    "Merged {} into {}",
                    review.source.display_name(),
                    review.survivor.display_name()
                ));
            }
            Err(error) => self.show_toast(format!("Couldn't merge contacts: {error}")),
        }
        self.list_stale = true;
        self.dirty = true;
    }
}

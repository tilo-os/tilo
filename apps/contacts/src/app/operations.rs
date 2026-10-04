//! The operations the book publishes (open a contact or a group, import,
//! export, review a duplicate, resolve an import), answered for pointer,
//! keyboard, assistive technology and agents alike.

use automerge::{AutoCommit, ObjId};
use ui::tilo_actions;

use super::{ContactsApp, FAVORITES_FILTER};

impl ContactsApp {
    #[allow(
        clippy::too_many_lines,
        reason = "all admitted Contacts view operations are centralized so resource routes cannot drift"
    )]
    pub(super) fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if type_id != crate::actions::CONTACTS_OPERATION_TYPE {
            return Ok(false);
        }
        let operation: crate::actions::ContactsOperation =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        match (kind, operation) {
            (tilo_actions::OperationKind::View, crate::actions::ContactsOperation::Open { id }) => {
                let id =
                    uuid::Uuid::parse_str(&id).map_err(|_| "invalid contact id".to_string())?;
                if self.store.get(id).is_none() {
                    return Err("contact no longer exists".into());
                }
                self.reveal_contact(id);
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::ContactsOperation::OpenGroup { name },
            ) => self.reveal_group(&name)?,
            (
                tilo_actions::OperationKind::View,
                crate::actions::ContactsOperation::OpenFavorites,
            ) => {
                self.set_group_filter(Some(FAVORITES_FILTER.into()));
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::ContactsOperation::OpenDuplicateReview {
                    survivor_id,
                    source_id,
                },
            ) => {
                let survivor = uuid::Uuid::parse_str(&survivor_id)
                    .map_err(|_| "invalid surviving contact id".to_string())?;
                let source = uuid::Uuid::parse_str(&source_id)
                    .map_err(|_| "invalid source contact id".to_string())?;
                self.open_duplicate_review(survivor, source)?;
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::ContactsOperation::ImportVcard { path },
            ) => {
                if path.is_some() {
                    return Err(
                        "direct filesystem paths are not accepted; use the system file picker"
                            .into(),
                    );
                }
                if self.import_rx.is_some() || self.import_parse_rx.is_some() {
                    return Err("a contact import is already in progress".into());
                }
                self.request_import();
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::ContactsOperation::ExportAllVcard { path },
            ) => {
                if path.is_some() {
                    return Err(
                        "direct filesystem paths are not accepted; use the system file picker"
                            .into(),
                    );
                }
                if self.export_rx.is_some() {
                    return Err("a contact export is already in progress".into());
                }
                let ids = self
                    .store
                    .contacts
                    .iter()
                    .map(|contact| contact.id)
                    .collect();
                if !self.request_export(ids, crate::vcard::VCardVersion::V4) {
                    return Err("there are no contacts to export".into());
                }
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::ContactsOperation::ExportContactVcard { id, path },
            ) => {
                if path.is_some() {
                    return Err(
                        "direct filesystem paths are not accepted; use the system file picker"
                            .into(),
                    );
                }
                let id =
                    uuid::Uuid::parse_str(&id).map_err(|_| "invalid contact id".to_string())?;
                self.open_contact_export(id)?;
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::ContactsOperation::KeepImportConflict { token },
            ) => {
                if !self
                    .import_conflicts
                    .iter()
                    .any(|conflict| conflict.token.to_string() == token)
                {
                    return Err("import conflict no longer exists".into());
                }
                self.pending_conflict_resolution = Some((token, false));
            }
            (
                tilo_actions::OperationKind::Effect,
                crate::actions::ContactsOperation::ImportConflictAsNew { token },
            ) => {
                if !self
                    .import_conflicts
                    .iter()
                    .any(|conflict| conflict.token.to_string() == token)
                {
                    return Err("import conflict no longer exists".into());
                }
                self.pending_conflict_resolution = Some((token, true));
            }
            _ => return Err("operation kind does not match Contacts operation".into()),
        }
        Ok(true)
    }

    pub(super) fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if type_id != crate::actions::CONTACTS_OPERATION_TYPE {
            return Ok(None);
        }
        let operation: crate::actions::ContactsOperation =
            serde_json::from_slice(payload).map_err(|e| e.to_string())?;
        match operation {
            crate::actions::ContactsOperation::ImportVcard { path: file } => {
                if self.resource_invocation.is_some()
                    || self.import_rx.is_some()
                    || self.import_parse_rx.is_some()
                {
                    return Err("another contact file operation is running".into());
                }
                if file.is_some() {
                    return Err(
                        "direct filesystem paths are not accepted; use the system file picker"
                            .into(),
                    );
                }
                self.resource_invocation = Some(invocation);
                self.request_import();
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            }
            crate::actions::ContactsOperation::ExportAllVcard { path: file } => {
                if self.store.contacts.is_empty() {
                    return Err("there are no contacts to export".into());
                }
                if self.resource_invocation.is_some() {
                    return Err("another contact file operation is running".into());
                }
                if file.is_some() {
                    return Err(
                        "direct filesystem paths are not accepted; use the system file picker"
                            .into(),
                    );
                }
                self.resource_invocation = Some(invocation);
                let ids = self.store.contacts.iter().map(|c| c.id).collect();
                if !self.request_export(ids, crate::vcard::VCardVersion::V4) {
                    self.resource_invocation = None;
                    return Err("contact export could not be started".into());
                }
                Ok(Some(ui::widget::ResourceOperationStart::Accepted))
            }
            crate::actions::ContactsOperation::ExportContactVcard { .. } => {
                // Decode/validate and open the exact same export dialog as a
                // context-menu activation. This operation completes once the
                // flow is shown; a portal job begins only after the person
                // chooses a vCard version and destination in that flow.
                self.answer_resource_operation(kind, type_id, payload, doc, path)
                    .map(|handled| {
                        handled.then(|| {
                            ui::widget::ResourceOperationStart::Completed(
                                tilo_actions::ActionOutput::default(),
                            )
                        })
                    })
            }
            crate::actions::ContactsOperation::Open { .. }
            | crate::actions::ContactsOperation::OpenGroup { .. }
            | crate::actions::ContactsOperation::OpenFavorites
            | crate::actions::ContactsOperation::OpenDuplicateReview { .. }
            | crate::actions::ContactsOperation::KeepImportConflict { .. }
            | crate::actions::ContactsOperation::ImportConflictAsNew { .. } => self
                .answer_resource_operation(kind, type_id, payload, doc, path)
                .map(|handled| {
                    handled.then(|| {
                        ui::widget::ResourceOperationStart::Completed(
                            tilo_actions::ActionOutput::default(),
                        )
                    })
                }),
        }
    }
}

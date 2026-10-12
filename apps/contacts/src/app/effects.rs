use super::files::contacts_import_request;
use super::{ContactId, ContactsApp, FAVORITES_FILTER, FileWork, VCardVersion};
impl ContactsApp {
    pub(crate) fn answer_effect(
        &mut self,
        effect: crate::actions::ContactsEffect,
        cx: &mut ui::framework::EffectCx<crate::resource::Contacts, Self>,
    ) -> ui::framework::EffectAnswer {
        use crate::actions::ContactsEffect;
        match effect {
            ContactsEffect::ImportVcard => {
                let work = self.begin_file_work(FileWork::Import)?;
                Ok(
                    cx.choose_files(contacts_import_request(), move |_, files, cx| {
                        Self::import_chosen(files, work, cx)
                    }),
                )
            }
            ContactsEffect::ExportAllVcard => {
                let ids: Vec<ContactId> = self.store.contacts.iter().map(|c| c.id).collect();
                self.export_to_chosen_file(&ids, VCardVersion::V4, cx)
            }
            ContactsEffect::ListContacts { query, group } => {
                let state = cx.live().get();
                Ok(ui::framework::Effected::value(
                    crate::actions::list_contacts_json(
                        &state.contacts,
                        query.as_deref(),
                        group.as_deref(),
                    ),
                ))
            }
            ContactsEffect::ReadContact { id } => {
                let id: ContactId = id
                    .trim()
                    .parse()
                    .map_err(|_| "id is a contact id, as list-contacts answers it")?;
                let state = cx.live().get();
                let contact = state
                    .contacts
                    .iter()
                    .find(|contact| contact.id == id)
                    .ok_or("no contact has this id")?;
                Ok(ui::framework::Effected::value(
                    crate::actions::contact_detail_json(contact),
                ))
            }
            ContactsEffect::ExportVcardContent { query, group } => {
                let state = cx.live().get();
                let chosen: Vec<crate::model::Contact> = crate::actions::matching_contacts(
                    &state.contacts,
                    query.as_deref(),
                    group.as_deref(),
                )
                .into_iter()
                .cloned()
                .collect();
                if chosen.is_empty() {
                    return Err("no contact matches".into());
                }
                let cards = crate::vcard::to_vcards(&chosen, VCardVersion::V4);
                let content = ui::tilo_resource::content::current()
                    .ok_or("this window keeps no content to export into")?;
                let hash = content
                    .put(cards.as_bytes())
                    .map_err(|error| format!("the vCard could not be kept: {error}"))?;
                ui::framework::Effected::content(&hash)
            }
            other => {
                self.perform_effect(other)?;
                Ok(ui::framework::Effected::done())
            }
        }
    }
    fn queue_import_resolution(
        &mut self,
        token: String,
        import_as_new: bool,
    ) -> Result<(), String> {
        if !self
            .import_conflicts
            .iter()
            .any(|conflict| conflict.token.to_string() == token)
        {
            return Err("import conflict no longer exists".into());
        }
        self.pending_conflict_resolution = Some((token, import_as_new));
        Ok(())
    }
    fn perform_effect(&mut self, effect: crate::actions::ContactsEffect) -> Result<(), String> {
        use crate::actions::ContactsEffect;
        match effect {
            ContactsEffect::Open { contact } => {
                if self.store.get(contact).is_none() {
                    return Err("contact no longer exists".into());
                }
                self.reveal_contact(contact);
            }
            ContactsEffect::OpenGroup { name } => self.reveal_group(&name)?,
            ContactsEffect::OpenFavorites => {
                self.set_group_filter(Some(FAVORITES_FILTER.into()));
            }
            ContactsEffect::OpenReview { survivor, source } => {
                self.open_duplicate_review(survivor, source)?;
            }
            ContactsEffect::ExportContact { contact } => self.open_contact_export(contact)?,
            ContactsEffect::KeepExisting { token } => self.queue_import_resolution(token, false)?,
            ContactsEffect::ImportAsNew { token } => self.queue_import_resolution(token, true)?,
            ContactsEffect::ImportVcard
            | ContactsEffect::ExportAllVcard
            | ContactsEffect::ExportVcardContent { .. }
            | ContactsEffect::ListContacts { .. }
            | ContactsEffect::ReadContact { .. } => {
                return Err("this effect answers through its own handler".into());
            }
        }
        Ok(())
    }
}

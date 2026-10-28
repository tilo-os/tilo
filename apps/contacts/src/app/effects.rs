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
            ContactsEffect::ExportVcardContent { contact, name } => {
                crate::content_export::vcards(&cx.live().get(), contact, name.as_deref())
                    .map(ui::framework::Effected::value)
            }
            ContactsEffect::ExportAllVcard => {
                let ids: Vec<ContactId> = self.store.contacts.iter().map(|c| c.id).collect();
                self.export_to_chosen_file(&ids, VCardVersion::V4, cx)
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
            | ContactsEffect::ExportVcardContent { .. } => {
                return Err("a file operation answers through its effect".into());
            }
        }
        Ok(())
    }
}

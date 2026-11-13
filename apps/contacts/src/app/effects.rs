//! The address book as a bound view: [`window`] takes the window into the view,
//! which answers the book's effects and publishes what the window holds beside
//! the book.
//!
//! An effect answers from the book the document holds; what it asks of the window
//! itself waits in [`Store::asks`] until the window next looks.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, Effected, LiveRow, Publishes, ViewCx};
use ui::widget::Element;

use super::files::{
    ImportSummary, begin_file_work, contacts_import_request, export_bytes, import_chosen,
    save_export,
};
use super::{ContactId, ContactsApp, FAVORITES_FILTER, FileWork, ImportConflict, VCardVersion};
use crate::actions::ContactsEffect;
use crate::model::group_names_equal;
use crate::resource::Contacts;

pub(crate) enum Ask {
    OpenGroup(String),
    OpenFavorites,
    Sort(String),
    EditField(ContactId, crate::form::Slot),
    OpenWebsite(String),
    Review(ContactId, ContactId),
    Export(ContactId),
    Resolve(String, bool),
    Imported(ImportSummary, Option<ContactId>),
    Say(String),
    Stage(Option<String>),
}

#[derive(Default)]
pub(crate) struct Store {
    pub(super) asks: Vec<Ask>,
    pub(crate) import_conflicts: Vec<ImportConflict>,
    pub(super) export_open: bool,
}

pub(crate) type Asked = Rc<RefCell<Store>>;

impl Store {
    pub(crate) fn conflict_rows(&self) -> Vec<LiveRow> {
        self.import_conflicts
            .iter()
            .map(|conflict| {
                crate::actions::import_conflict_row(
                    &conflict.token.to_string(),
                    &conflict.incoming.display_name(),
                )
            })
            .collect()
    }
}

/// Answer one of the book's effects, from a control, the palette, an agent or a
/// script.
fn answer(
    asked: &Asked,
    file_work: &Rc<Cell<Option<FileWork>>>,
    effect: ContactsEffect,
    cx: &mut EffectCx<Contacts>,
) -> EffectAnswer {
    let book = cx.live().get();
    let ask = match effect {
        ContactsEffect::ImportVcard => {
            let work = begin_file_work(file_work, FileWork::Import)?;
            let asked = Rc::clone(asked);
            return Ok(
                cx.choose_files(contacts_import_request(), move |(), files, cx| {
                    import_chosen(files, work, asked, cx)
                }),
            );
        }
        ContactsEffect::ExportVcardContent { contact, name } => {
            return crate::content_export::vcards(&book, contact, name.as_deref())
                .map(Effected::value);
        }
        ContactsEffect::ExportAllVcard => {
            let ids: Vec<ContactId> = book.contacts.iter().map(|c| c.id).collect();
            let (bytes, count) = export_bytes(&book, &ids, VCardVersion::V4)?;
            let work = begin_file_work(file_work, FileWork::Export)?;
            return Ok(save_export(
                bytes,
                count,
                VCardVersion::V4,
                work,
                Rc::clone(asked),
                cx,
            ));
        }
        ContactsEffect::OpenGroup { name } => {
            let group = book
                .all_groups()
                .into_iter()
                .find(|group| group_names_equal(group, &name))
                .ok_or_else(|| "group no longer exists".to_string())?;
            Ask::OpenGroup(group)
        }
        ContactsEffect::OpenFavorites => Ask::OpenFavorites,
        ContactsEffect::SortContacts { order } => Ask::Sort(order),
        ContactsEffect::EditField { contact, field } => {
            if book.contact(contact).is_none() {
                return Err("contact no longer exists".into());
            }
            let slot = crate::form::Slot::of_detail_key(&field)
                .ok_or_else(|| format!("no detail `{field}` to edit"))?;
            Ask::EditField(contact, slot)
        }
        ContactsEffect::OpenWebsite { contact } => {
            let website = book
                .contact(contact)
                .ok_or_else(|| "contact no longer exists".to_string())?
                .website
                .trim()
                .to_owned();
            if website.is_empty() {
                return Err("the contact has no website".into());
            }
            Ask::OpenWebsite(website)
        }
        ContactsEffect::ReadDuplicate { survivor, source } => {
            return Ok(Effected::value(read_duplicate(&book, survivor, source)?));
        }
        ContactsEffect::OpenReview { survivor, source } => {
            let kept = book
                .contact(survivor)
                .ok_or_else(|| "surviving contact no longer exists".to_string())?;
            let other = book
                .contact(source)
                .ok_or_else(|| "source contact no longer exists".to_string())?;
            crate::actions::duplicate_candidate(kept, other)
                .ok_or_else(|| "contacts are no longer a likely duplicate".to_string())?;
            Ask::Review(survivor, source)
        }
        ContactsEffect::ExportContact { contact } => {
            if book.contact(contact).is_none() {
                return Err("contact no longer exists".into());
            }
            if asked.borrow().export_open || file_work.get().is_some() {
                return Err("a contact export is already in progress".into());
            }
            Ask::Export(contact)
        }
        ContactsEffect::KeepExisting { token } => resolve(asked, token, false)?,
        ContactsEffect::ImportAsNew { token } => resolve(asked, token, true)?,
    };
    asked.borrow_mut().asks.push(ask);
    Ok(Effected::done())
}

/// The possible duplicate `source` of `survivor`: both contacts, by item and
/// name, and what they share.
fn read_duplicate(
    book: &Contacts,
    survivor: ContactId,
    source: ContactId,
) -> Result<serde_json::Value, String> {
    let kept = book
        .contact(survivor)
        .ok_or_else(|| "surviving contact no longer exists".to_string())?;
    let other = book
        .contact(source)
        .ok_or_else(|| "source contact no longer exists".to_string())?;
    let candidate = crate::actions::duplicate_candidate(kept, other)
        .ok_or_else(|| "contacts are no longer a likely duplicate".to_string())?;
    let person = |contact: &crate::model::Contact| {
        serde_json::json!({
            "item": crate::resource::contact_resource_id(contact.id),
            "name": contact.display_name(),
        })
    };
    Ok(serde_json::json!({
        "kind": "contacts.duplicate",
        "survivor": person(kept),
        "source": person(other),
        "shared": candidate.reasons.iter().map(|reason| match reason {
            crate::actions::DuplicateReason::Email => "email",
            crate::actions::DuplicateReason::Phone => "phone",
            crate::actions::DuplicateReason::Name => "name",
        }).collect::<Vec<_>>(),
    }))
}

fn resolve(asked: &Asked, token: String, import_as_new: bool) -> Result<Ask, String> {
    if !asked
        .borrow()
        .import_conflicts
        .iter()
        .any(|conflict| conflict.token.to_string() == token)
    {
        return Err("import conflict no longer exists".into());
    }
    Ok(Ask::Resolve(token, import_as_new))
}

impl ContactsApp {
    pub(super) fn do_asks(&mut self) {
        let asks = std::mem::take(&mut self.asked.borrow_mut().asks);
        for ask in asks {
            let refused = match ask {
                Ask::OpenGroup(group) => self.reveal_group(&group),
                Ask::OpenFavorites => {
                    self.set_group_filter(Some(FAVORITES_FILTER.into()));
                    Ok(())
                }
                Ask::Sort(order) => self.sort_by(&order),
                Ask::EditField(contact, slot) => {
                    self.edit_field(contact, slot);
                    Ok(())
                }
                Ask::OpenWebsite(url) => {
                    self.open_website(url);
                    Ok(())
                }
                Ask::Review(survivor, source) => self.open_duplicate_review(survivor, source),
                Ask::Export(contact) => self.open_contact_export(contact),
                Ask::Resolve(token, import_as_new) => {
                    self.pending_conflict_resolution = Some((token, import_as_new));
                    Ok(())
                }
                Ask::Imported(summary, first_new) => {
                    if summary.created > 0 {
                        self.lapse_delete_undo();
                    }
                    self.show_imported(&summary, first_new);
                    Ok(())
                }
                Ask::Say(text) => {
                    self.show_toast(text);
                    Ok(())
                }
                Ask::Stage(scenario) => {
                    self.stage_capture(scenario.as_deref());
                    Ok(())
                }
            };
            if let Err(reason) = refused {
                self.show_toast(reason);
            }
        }
    }
}

/// The view of the address book: `app` follows the document, and the view
/// answers the book's effects and publishes the pending import and account
/// reviews.
pub(crate) fn window(cx: &ViewCx<Contacts>, mut app: ContactsApp) -> Element {
    app.document = Some(cx.live().clone());
    app.actions = Some(cx.actions());
    app.attach_dispatcher(cx.dispatcher());
    app.attach_actions(&cx.actions());
    app.jobs = Some(cx.jobs());
    if cx.mode().is_live() {
        app.identity_directory = cx.shared_person::<tilo_contact_directory::ContactDirectory>();
    }
    // A vCard the app was opened with ("open with", a dropped file) is imported.
    if let Some(file) = cx.launch().file() {
        app.open_cards(String::from_utf8_lossy(file.bytes()).into_owned());
    }
    // A picked search hit (or an agent's `open`) shows its contact.
    cx.on_open_item(app.opener());
    let asked = Rc::clone(&app.asked);
    {
        let asked = Rc::clone(&asked);
        // The one file operation running, which the window shows.
        let file_work = Rc::clone(&app.file_work);
        cx.effects(move |effect: ContactsEffect, cx| answer(&asked, &file_work, effect, cx));
    }
    {
        let asked = Rc::clone(&asked);
        cx.on_stage(move |scenario| {
            asked
                .borrow_mut()
                .asks
                .push(Ask::Stage(scenario.map(str::to_owned)));
        });
    }
    let editor = app.account_editor.clone();
    let rows = move || {
        let mut rows = asked.borrow().conflict_rows();
        if let Some(editor) = &editor {
            rows.extend(crate::account_editor::review_rows(editor));
        }
        rows
    };
    let rows = Rc::new(rows);
    let row = Rc::clone(&rows);
    cx.publishing(
        Publishes::new()
            .live_rows(move || rows())
            .live_row(move |id| row().into_iter().find(|row| row.id() == id)),
        Box::new(app),
    )
}

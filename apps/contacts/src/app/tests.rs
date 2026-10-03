use super::*;
use automerge::ROOT;
use automerge::ReadDoc;
use std::path::PathBuf;
use std::sync::mpsc;
use tilo_automerge::ApplyIncoming;
use ui::headless::FrameHarness;
use ui::portal::{MultiOutcome as ImportOutcome, PortalOutcome as ExportOutcome};
use ui::tilo_actions;

use super::files::ImportSummary;
use crate::form::{RowKind, Slot, TypeTarget};
use crate::model::{Address, Birthday, GroupEntry, sample_contacts};
use crate::vcard;
use crate::view::DialogChoice;

/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// The sample book synced into a fresh document.
fn synced() -> (ContactsApp, AutoCommit) {
    let mut app = ContactsApp::sample();
    let mut doc = AutoCommit::new();
    app.sync(&mut doc, &ROOT);
    (app, doc)
}

/// Queue `request` as a pane or button would, and let the app apply it.
fn ask(app: &mut ContactsApp, doc: &mut AutoCommit, request: Request) {
    app.shared.borrow_mut().push(request);
    app.apply_requests(doc);
    app.publish();
}

fn selected_contact(app: &ContactsApp) -> Contact {
    app.selected
        .and_then(|id| app.store.get(id))
        .cloned()
        .expect("a contact is selected")
}

fn id_named(app: &ContactsApp, name: &str) -> ContactId {
    app.store
        .contacts
        .iter()
        .find(|contact| contact.display_name() == name)
        .unwrap_or_else(|| panic!("{name} is in the book"))
        .id
}

fn list_ids(app: &ContactsApp) -> Vec<ContactId> {
    app.shared.borrow().rows.iter().map(|row| row.id).collect()
}

fn harness(viewport: (u32, u32)) -> FrameHarness {
    let mut harness = FrameHarness::for_root(
        ui::State::new(),
        ContactsApp::sample(),
        viewport,
        Theme::dark(),
    );
    harness.send(&WindowEvent::RedrawRequested);
    harness
}

fn named(harness: &FrameHarness, name: &str) -> bool {
    harness
        .a11y_nodes()
        .iter()
        .any(|(_, node)| node.label() == Some(name))
}

fn live_message<'a>(
    node: &'a ui::a11y::AccessNode,
    message: &str,
) -> Option<&'a ui::a11y::AccessNode> {
    if node.name.as_deref() == Some(message) && node.live.is_some() {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| live_message(child, message))
}

fn toast(app: &ContactsApp) -> Option<&str> {
    app.toast.as_ref().map(|(message, _)| message.as_str())
}

/// The window is the shell: groups beside the people beside the person,
/// every header command named, the sample book listed and its first
/// contact shown.
#[test]
fn the_window_lists_groups_people_and_the_selected_person() {
    let harness = harness((1280, 800));
    let app = harness.root::<ContactsApp>();
    let geometry = app.shell.geometry();
    assert_eq!(geometry.sidebar, ui::widget::SidebarPlacement::Docked);
    assert!(geometry.side_by_side, "list and person stand side by side");
    assert!(geometry.stack.is_empty());

    assert_eq!(
        app.shared.borrow().rows.len(),
        7,
        "every sample contact is listed"
    );
    assert_eq!(
        app.shared.borrow().selected,
        app.selected,
        "the list shows the app's selection"
    );
    let shown = selected_contact(app);
    assert!(
        matches!(&app.content_shown, Some(ContentKey::Contact(contact)) if contact.id == shown.id),
        "the content column shows the selected person"
    );
    for name in [
        "Groups",
        "Contacts",
        "More actions",
        "New contact",
        "Favorite",
        "Edit",
        "Delete contact",
        "All contacts",
        "Favorites",
        "Work",
        shown.display_name().as_str(),
    ] {
        assert!(named(&harness, name), "{name} is exposed");
    }
    assert!(
        app.shared.borrow().painted.len() == 7,
        "the list painted one row per person"
    );
}

/// A phone-sized window is a push stack: the list first, the person pushed
/// over it when one is chosen.
#[test]
fn a_phone_window_pushes_the_person_over_the_list() {
    let mut harness = harness((390, 844));
    {
        let app = harness.root::<ContactsApp>();
        assert!(
            !app.shell.geometry().stack.is_empty(),
            "the window is a stack"
        );
        assert_eq!(app.shell.front_pane(), NavPane::Overview);
    }
    let grace = id_named(harness.root::<ContactsApp>(), "Grace Hopper");
    harness
        .root_mut::<ContactsApp>()
        .shared
        .borrow_mut()
        .push(Request::Select(grace));
    harness.send(&WindowEvent::RedrawRequested);
    let app = harness.root::<ContactsApp>();
    assert_eq!(app.selected, Some(grace));
    assert_eq!(
        app.shell.front_pane(),
        NavPane::Content,
        "choosing a person pushes them in"
    );
    assert!(named(&harness, "Back"), "the shell offers the way back");
}

/// The sidebar's destinations narrow the list: favourites, one group, and
/// back to everyone; a destination that empties out falls back to everyone.
#[test]
fn sidebar_destinations_narrow_the_list() {
    let (mut app, mut doc) = synced();
    assert_eq!(list_ids(&app).len(), 7);

    app.destination.set(Some(FAVORITES_DESTINATION.into()));
    app.poll_destination();
    app.publish();
    let favorites = list_ids(&app);
    assert_eq!(favorites.len(), 3);
    assert!(
        favorites
            .iter()
            .all(|id| app.store.get(*id).unwrap().favorite)
    );
    assert_eq!(app.destination_label().as_deref(), Some("Favorites"));

    app.destination
        .set(Some(format!("{GROUP_DESTINATION}Friends")));
    app.poll_destination();
    app.publish();
    let friends = list_ids(&app);
    assert_eq!(friends.len(), 2);
    for id in &friends {
        assert!(
            app.store
                .get(*id)
                .unwrap()
                .groups
                .iter()
                .any(|g| g.value == "Friends"),
            "every row is in the group"
        );
    }
    assert!(
        app.selected.is_some_and(|id| friends.contains(&id)),
        "the selection follows into the group"
    );
    assert!(
        app.sidebar_shown
            .iter()
            .any(|(id, label, count)| id == "group:Friends" && label == "Friends" && *count == 2),
        "the sidebar counts the group's members"
    );

    // Taking everyone out of the group removes it as a destination.
    for id in friends {
        let mut contact = app.store.get(id).cloned().unwrap();
        let before = contact.clone();
        contact.groups.clear();
        app.try_perform_action(
            &mut doc,
            &crate::actions::patch_contact_action(&before, &contact),
        )
        .unwrap();
        app.store.upsert(contact);
    }
    app.list_stale = true;
    app.publish();
    assert!(
        app.group_filter.is_none(),
        "an emptied group is no destination"
    );
    assert_eq!(list_ids(&app).len(), 7);
    assert_eq!(
        app.destination.with(Clone::clone).as_deref(),
        Some(ALL_DESTINATION),
        "the sidebar follows"
    );
}

/// Sort and group views of the book order and filter the published rows.
#[test]
fn sort_orders_the_list_and_the_more_menu_offers_it() {
    let (mut app, mut doc) = synced();
    let by_name = list_ids(&app);
    let names: Vec<String> = by_name
        .iter()
        .map(|id| app.store.get(*id).unwrap().last.clone())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "the list is sorted by last name");

    app.open_menu_at(PendingMenu::More, Rect::new(500.0, 40.0, 532.0, 72.0));
    let Some(OpenMenu::More(rows)) = &app.open_menu else {
        panic!("the More menu is open");
    };
    let sort_org = rows
        .iter()
        .position(|row| *row == Some(MoreAction::Sort(SortKey::Org)))
        .expect("the menu offers sorting by organization");
    assert!(
        !rows.contains(&Some(MoreAction::ReviewDuplicate)),
        "no review is offered without a likely duplicate"
    );
    app.run_menu_choice(&mut doc, sort_org);
    app.publish();
    let by_org = list_ids(&app);
    assert_ne!(by_org, by_name);
    let orgs: Vec<String> = by_org
        .iter()
        .map(|id| normalize_text(&app.store.get(*id).unwrap().org))
        .collect();
    let filled: Vec<&String> = orgs.iter().filter(|org| !org.is_empty()).collect();
    let mut sorted = filled.clone();
    sorted.sort();
    assert_eq!(filled, sorted, "the list is sorted by organization");
    assert!(
        orgs.iter().skip(filled.len()).all(String::is_empty),
        "people without one come last"
    );
}

/// Pointer, keyboard and assistive technology all choose people through
/// the list: a press selects, a double press edits, the arrows move.
#[test]
fn the_list_selects_by_pointer_keyboard_and_semantics() {
    let mut harness = harness((1280, 800));
    let (target, center) = {
        let app = harness.root::<ContactsApp>();
        let bridge = app.shared.borrow();
        let (id, rect) = bridge.painted[1];
        (id, rect.center())
    };
    harness.click_at(center.x, center.y);
    assert_eq!(harness.root::<ContactsApp>().selected, Some(target));
    assert_eq!(harness.root::<ContactsApp>().mode, Mode::View);

    harness.click_at(center.x, center.y);
    let app = harness.root::<ContactsApp>();
    assert_eq!(
        app.mode,
        Mode::Edit,
        "a double press opens the person for editing"
    );
    assert_eq!(app.form.as_ref().map(|form| form.draft.id), Some(target));

    // Escape leaves the form; the arrows then move through the list.
    harness.key("Escape");
    assert_eq!(harness.root::<ContactsApp>().mode, Mode::View);
    let rows = list_ids(harness.root::<ContactsApp>());
    harness.key("Down");
    assert_eq!(harness.root::<ContactsApp>().selected, rows.get(2).copied());
    harness.key("Up");
    harness.key("Up");
    assert_eq!(
        harness.root::<ContactsApp>().selected,
        rows.first().copied()
    );

    // The list's semantic rows select too.
    let last = *rows.last().unwrap();
    let (app, state) = harness.root_and_state_mut::<ContactsApp>();
    let mut pane = ContactList::new(std::rc::Rc::clone(&app.shared));
    assert!(pane.perform_semantic_action(
        &format!("contacts.select:{last}"),
        &ui::a11y::Action::Activate,
        &mut state.doc,
        &ROOT,
    ));
    app.apply_requests(&mut state.doc);
    assert_eq!(app.selected, Some(last));
}

/// The list's keys: Delete asks before deleting, `e` edits, `n` starts a
/// new contact, and an open form ignores bare keys.
#[test]
fn list_keys_delete_edit_and_create() {
    let mut harness = harness((1280, 800));
    let selected = harness.root::<ContactsApp>().selected;
    harness.key("Delete");
    assert_eq!(harness.root::<ContactsApp>().confirm_delete, selected);
    assert!(
        matches!(
            harness.root::<ContactsApp>().dialog,
            Some((DialogKey::Delete(_), _))
        ),
        "the delete dialog is up"
    );
    harness.key("Escape");
    assert!(
        harness.root::<ContactsApp>().confirm_delete.is_none(),
        "Escape cancels"
    );

    harness.key("e");
    assert_eq!(harness.root::<ContactsApp>().mode, Mode::Edit);
    // Bare letters belong to the form now, not to list shortcuts.
    harness.key("n");
    assert!(
        harness
            .root::<ContactsApp>()
            .form
            .as_ref()
            .is_some_and(|form| !form.is_new),
        "an open form ignores the new-contact key"
    );
    harness.key("Escape");
    harness.key("n");
    let app = harness.root::<ContactsApp>();
    assert!(app.form.as_ref().is_some_and(|form| form.is_new));
    assert!(
        app.shared.borrow().selected.is_none(),
        "the list shows no one selected while a new contact is drafted"
    );
}

/// A contact's action menu reaches the canonical edit, favourite, export
/// and delete paths.
#[test]
fn the_contact_menu_runs_the_canonical_actions() {
    let (mut app, mut doc) = synced();
    let grace = id_named(&app, "Grace Hopper");
    let was_favorite = app.store.get(grace).unwrap().favorite;
    ask(
        &mut app,
        &mut doc,
        Request::ContextMenu(grace, Point::new(400.0, 200.0)),
    );
    assert_eq!(app.selected, Some(grace));
    let rows = match &app.open_menu {
        Some(OpenMenu::Contact(id, rows)) if *id == grace => rows.clone(),
        _ => panic!("Grace's menu is open"),
    };
    assert!(app.menu.is_open());
    let index = |action| rows.iter().position(|row| *row == Some(action)).unwrap();

    app.run_menu_choice(&mut doc, index(ContextAction::Favorite));
    assert_eq!(app.store.get(grace).unwrap().favorite, !was_favorite);
    assert_eq!(
        ContactsDoc::list_contacts(&doc)
            .iter()
            .find(|c| c.id == grace)
            .unwrap()
            .favorite,
        !was_favorite,
        "the favourite went through the document"
    );

    ask(
        &mut app,
        &mut doc,
        Request::ContextMenu(grace, Point::new(400.0, 200.0)),
    );
    app.run_menu_choice(&mut doc, index(ContextAction::Export));
    app.publish();
    assert!(
        app.export
            .as_ref()
            .is_some_and(|export| export.ids == vec![grace]),
        "export asks for the format of just this contact"
    );
    assert!(matches!(app.dialog, Some((DialogKey::Export, _))));
    ask(&mut app, &mut doc, Request::Dialog(DialogChoice::Secondary));
    assert!(app.export.is_none());

    ask(
        &mut app,
        &mut doc,
        Request::ContextMenu(grace, Point::new(400.0, 200.0)),
    );
    app.run_menu_choice(&mut doc, index(ContextAction::Delete));
    assert_eq!(app.confirm_delete, Some(grace));
    ask(&mut app, &mut doc, Request::Dialog(DialogChoice::Primary));
    assert!(app.store.get(grace).is_none());
    assert!(
        !ContactsDoc::list_contacts(&doc)
            .iter()
            .any(|c| c.id == grace)
    );
    assert_eq!(toast(&app), Some("Contact deleted"));
    assert!(
        app.last_dispatched().is_some(),
        "the deletion is the palette's action"
    );

    let next = app.selected.unwrap();
    ask(
        &mut app,
        &mut doc,
        Request::ContextMenu(next, Point::ORIGIN),
    );
    app.run_menu_choice(&mut doc, index(ContextAction::Edit));
    assert_eq!(app.mode, Mode::Edit);
}

/// The person's header commands: favourite toggles through the document,
/// Delete asks first, and the website opens.
#[test]
fn header_commands_favorite_delete_and_open_the_website() {
    let (mut app, mut doc) = synced();
    let ada = selected_contact(&app);
    ask(&mut app, &mut doc, Request::ToggleFavorite);
    assert_eq!(app.store.get(ada.id).unwrap().favorite, !ada.favorite);
    assert_eq!(
        app.header_shown,
        Some(HeaderKey::View {
            favorite: !ada.favorite
        }),
        "the toggle follows the person"
    );

    ask(&mut app, &mut doc, Request::DeleteSelected);
    assert_eq!(app.confirm_delete, Some(ada.id));
    ask(&mut app, &mut doc, Request::Dialog(DialogChoice::Dismiss));
    assert!(app.confirm_delete.is_none());
    assert!(
        app.store.get(ada.id).is_some(),
        "dismissing deletes nothing"
    );

    ask(
        &mut app,
        &mut doc,
        Request::OpenWebsite(ada.website.clone()),
    );
    assert_eq!(app.opened_url.as_deref(), Some(ada.website.as_str()));
}

/// Saves round-trip through the document; refused saves, favourites and
/// deletes reconcile and announce errors without claiming success.
#[test]
fn saves_persist_and_refusals_never_claim_success() {
    // --- phase: save_round_trips_through_doc ---
    {
        let (mut app, mut doc) = synced();
        let c = selected_contact(&app);
        let id = c.id;
        ask(&mut app, &mut doc, Request::EditSelected);
        app.form
            .as_ref()
            .unwrap()
            .set_value(Slot::Title, "Edited Title");
        ask(&mut app, &mut doc, Request::Save);
        assert_eq!(app.mode, Mode::View);
        let stored = ContactsDoc::list_contacts(&doc);
        assert_eq!(
            stored.iter().find(|x| x.id == id).unwrap().title,
            "Edited Title"
        );
        assert_eq!(toast(&app), Some("Saved"));
    }

    // --- phase: refused_save_reconciles_and_announces_an_error_without_claiming_success ---
    {
        let (mut app, mut doc) = synced();
        let contact = selected_contact(&app);
        ContactsDoc::delete_contact(&mut doc, contact.id);
        app.begin_edit(contact.clone(), false);
        app.save_edit(&mut doc);

        assert_eq!(app.mode, Mode::Edit, "the rejected draft remains editable");
        assert!(app.form.is_some());
        assert!(
            app.store.get(contact.id).is_none(),
            "store reconciles from the document"
        );
        let message = toast(&app).unwrap().to_owned();
        assert!(message.starts_with("Couldn't save contact:"));
        assert!(
            live_message(&app.accessibility().unwrap(), &message).is_some(),
            "the refusal is announced through the live region"
        );
    }

    // --- phase: refused_favorite_and_delete_reconcile_without_false_success ---
    {
        let (mut app, mut doc) = synced();
        let contact = selected_contact(&app);
        ContactsDoc::delete_contact(&mut doc, contact.id);

        assert!(
            !app.toggle_favorite(&mut doc, contact.id),
            "a tombstoned toggle is rejected"
        );
        assert!(
            app.store.get(contact.id).is_none(),
            "favorite refusal reconciles"
        );
        assert!(
            toast(&app)
                .unwrap()
                .starts_with("Couldn't update favorite:")
        );

        app.store.upsert(contact.clone());
        app.selected = Some(contact.id);
        app.delete_selected(&mut doc, contact.id);
        assert!(
            app.store.get(contact.id).is_none(),
            "delete refusal reconciles"
        );
        let delete_error = toast(&app).unwrap().to_owned();
        assert!(delete_error.starts_with("Couldn't delete contact:"));
        assert!(live_message(&app.accessibility().unwrap(), &delete_error).is_some());
    }
}

/// Edits stay in the form until Save; Cancel discards them; a new contact
/// is created only by Save.
#[test]
fn edits_commit_only_on_save_and_cancel_discards() {
    let (mut app, mut doc) = synced();
    let ada = selected_contact(&app);
    ask(&mut app, &mut doc, Request::EditSelected);
    app.form.as_ref().unwrap().set_value(Slot::First, "Augusta");
    app.sync(&mut doc, &ROOT);
    assert_eq!(
        ContactsDoc::list_contacts(&doc)
            .iter()
            .find(|c| c.id == ada.id)
            .unwrap()
            .first,
        ada.first,
        "typing never reaches the document"
    );
    ask(&mut app, &mut doc, Request::Cancel);
    assert_eq!(app.mode, Mode::View);
    assert_eq!(
        app.store.get(ada.id).unwrap().first,
        ada.first,
        "Cancel discards"
    );

    let before = app.store.len();
    ask(&mut app, &mut doc, Request::NewContact);
    let form = app.form.as_ref().unwrap();
    assert!(form.is_new);
    assert_eq!(
        form.draft.phones.len(),
        1,
        "a new contact starts with a phone row"
    );
    assert_eq!(form.draft.emails.len(), 1, "and an email row");
    form.set_value(Slot::First, "Hedy");
    form.set_value(Slot::Last, "Lamarr");
    form.set_value(Slot::Phone(0), "+1 310 555 0142");
    ask(&mut app, &mut doc, Request::Save);
    assert_eq!(app.store.len(), before + 1);
    let hedy = id_named(&app, "Hedy Lamarr");
    assert_eq!(app.selected, Some(hedy), "the new contact is selected");
    assert_eq!(
        app.store.get(hedy).unwrap().phones[0].value,
        "+1 310 555 0142"
    );
    assert_eq!(toast(&app), Some("Contact created"));
    assert!(list_ids(&app).contains(&hedy), "and listed");
}

/// Repeated rows grow and shrink in the draft, keep their identity and
/// text, and kinds and birthdays change only the draft until Save.
#[test]
fn repeated_rows_kinds_and_birthdays_edit_the_draft() {
    let (mut app, mut doc) = synced();
    let ada = selected_contact(&app);
    ask(&mut app, &mut doc, Request::EditSelected);
    let epoch = app.form_epoch;
    app.form
        .as_ref()
        .unwrap()
        .set_value(Slot::Phone(1), "+44 20 0000 0000");
    ask(&mut app, &mut doc, Request::Add(RowKind::Phone));
    assert!(
        app.form_epoch != epoch,
        "the form is rebuilt over the new row"
    );
    {
        let form = app.form.as_ref().unwrap();
        assert_eq!(form.draft.phones.len(), ada.phones.len() + 1);
        assert_eq!(
            form.value(Slot::Phone(1)),
            "+44 20 0000 0000",
            "typed text survives"
        );
        assert_eq!(
            form.draft.phones[0].id, ada.phones[0].id,
            "rows keep their identity"
        );
        assert_eq!(form.kind(TypeTarget::Phone(2)), "mobile");
    }
    ask(&mut app, &mut doc, Request::Remove(TypeTarget::Phone(0)));
    {
        let form = app.form.as_ref().unwrap();
        assert_eq!(form.draft.phones.len(), ada.phones.len());
        assert_eq!(form.draft.phones[0].id, ada.phones[1].id);
        assert_eq!(form.value(Slot::Phone(0)), "+44 20 0000 0000");
    }
    ask(&mut app, &mut doc, Request::Add(RowKind::Address));
    assert_eq!(
        app.form.as_ref().unwrap().draft.addresses.len(),
        ada.addresses.len() + 1
    );

    app.form
        .as_ref()
        .unwrap()
        .set_kind(TypeTarget::Email(0), "other");
    ask(&mut app, &mut doc, Request::ToggleCalendar);
    assert!(app.form.as_ref().unwrap().calendar_open);
    ask(&mut app, &mut doc, Request::PickDate("1816-01-02".into()));
    {
        let form = app.form.as_ref().unwrap();
        assert!(!form.calendar_open, "choosing a day closes the calendar");
        assert_eq!(form.value(Slot::Birthday), "1816-01-02");
    }
    assert_eq!(
        app.store.get(ada.id).unwrap().birthday,
        ada.birthday,
        "nothing changed before Save"
    );
    ask(&mut app, &mut doc, Request::Save);
    let saved = app.store.get(ada.id).unwrap();
    assert_eq!(saved.emails[0].label, "other");
    assert_eq!(saved.birthday, Birthday::parse("1816-01-02"));
    assert_eq!(saved.phones.len(), ada.phones.len());
}

/// Switching people with unsaved edits asks first: Save keeps them, Don't
/// save drops them, and dismissing stays in the form.
#[test]
fn switching_while_editing_is_guarded() {
    for choice in [
        DialogChoice::Primary,
        DialogChoice::Secondary,
        DialogChoice::Dismiss,
    ] {
        let (mut app, mut doc) = synced();
        let ada = selected_contact(&app);
        let grace = id_named(&app, "Grace Hopper");
        ask(&mut app, &mut doc, Request::EditSelected);
        app.form.as_ref().unwrap().set_value(Slot::Title, "Poet");
        ask(&mut app, &mut doc, Request::Select(grace));
        assert_eq!(
            app.pending_switch,
            Some(grace),
            "{choice:?}: the switch waits"
        );
        assert!(matches!(app.dialog, Some((DialogKey::Unsaved(_), _))));
        ask(&mut app, &mut doc, Request::Dialog(choice));
        let title = app.store.get(ada.id).unwrap().title.clone();
        match choice {
            DialogChoice::Primary => {
                assert_eq!(title, "Poet", "Save keeps the edit");
                assert_eq!(app.selected, Some(grace));
            }
            DialogChoice::Secondary => {
                assert_eq!(title, ada.title, "Don't save drops it");
                assert_eq!(app.selected, Some(grace));
                assert_eq!(app.mode, Mode::View);
            }
            DialogChoice::Dismiss => {
                assert_eq!(app.mode, Mode::Edit, "dismissing stays in the form");
                assert_eq!(app.form.as_ref().unwrap().value(Slot::Title), "Poet");
            }
        }
        assert!(app.pending_switch.is_none());
    }

    // A clean form switches without asking.
    let (mut app, mut doc) = synced();
    let grace = id_named(&app, "Grace Hopper");
    ask(&mut app, &mut doc, Request::EditSelected);
    ask(&mut app, &mut doc, Request::Select(grace));
    assert!(app.pending_switch.is_none());
    assert_eq!(app.selected, Some(grace));
    assert_eq!(app.mode, Mode::View);
}

/// A collaborator's change reaches an open form: untouched fields follow
/// it, the person's own edits stay.
#[test]
fn an_open_form_rebases_onto_a_collaborators_change() {
    let (mut app, mut doc) = synced();
    let ada = selected_contact(&app);
    ask(&mut app, &mut doc, Request::EditSelected);
    app.form
        .as_ref()
        .unwrap()
        .set_value(Slot::Title, "Local title");
    let mut remote = ada.clone();
    remote.org = "Remote organization".into();
    remote.title = "Remote title".into();
    tilo_actions::Action::apply(
        &crate::actions::patch_contact_action(&ada, &remote),
        &mut doc,
    )
    .unwrap();
    app.sync(&mut doc, &ROOT);
    let form = app.form.as_ref().unwrap();
    assert_eq!(form.value(Slot::Org), "Remote organization");
    assert_eq!(form.value(Slot::Title), "Local title");
}

/// The first run is empty, never fabricated, and its one action starts a
/// contact.
#[test]
fn first_run_is_empty_and_actionable() {
    let mut app = ContactsApp::new();
    let mut doc = AutoCommit::new();
    app.sync(&mut doc, &ROOT);
    assert!(app.store.is_empty(), "the live app invents no one");
    assert!(app.selected.is_none());
    assert_eq!(app.content_shown, Some(ContentKey::FirstRun));
    assert_eq!(app.header_shown, Some(HeaderKey::Empty));

    let mut pane = EmptyPane::new(
        std::rc::Rc::clone(&app.shared),
        "No contacts yet",
        Some(("New contact", Request::NewContact)),
    );
    let node = pane.accessibility().unwrap();
    let action = node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some("contacts.empty.action"))
        .expect("the first-run action is exposed");
    assert_eq!(action.name.as_deref(), Some("New contact"));
    assert!(pane.perform_semantic_action(
        "contacts.empty.action",
        &ui::a11y::Action::Activate,
        &mut doc,
        &ROOT,
    ));
    app.apply_requests(&mut doc);
    assert_eq!(app.mode, Mode::Edit);
    assert!(app.form.as_ref().is_some_and(|form| form.is_new));

    let empty = FrameHarness::for_root(
        ui::State::new(),
        ContactsApp::new(),
        (390, 844),
        Theme::dark(),
    );
    assert!(named(&empty, "New contact"));
}

/// Resource and search providers stay bounded on large books, resolve
/// exactly, and their activations reveal the addressed contact or group.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one case"
)]
#[test]
fn resource_and_search_providers_bound_large_books() {
    // --- phase: real_resource_provider_bounds_large_book_snapshot_and_exact_lookup ---
    {
        const LARGE: usize = 5_000;
        let (mut app, doc) = synced();
        let template = app.store.contacts[0].clone();
        app.store.contacts = (0..LARGE)
            .map(|index| {
                let mut contact = template.clone();
                contact.id = uuid::Uuid::from_u128(7_000_000 + index as u128);
                contact.first = format!("Contact {index}");
                contact.last.clear();
                contact.groups.clear();
                contact
            })
            .collect();
        let late_id = app.store.contacts.last().expect("late contact").id;
        app.store
            .contacts
            .last_mut()
            .expect("late contact")
            .groups
            .push(GroupEntry::new("Late group"));
        app.rebuild_resource_index();

        let root = ContactsApp::publish_resources(&app, &doc);
        assert_eq!(root.len(), 1);
        assert!(
            app.resource_snapshot_materialized.get()
                <= crate::actions::MAX_CONTEXT_CONTACT_RESOURCES
        );
        let contact_id = crate::actions::contact_resource_id(late_id);
        let contact = ContactsApp::publish_resource(&app, &doc, &contact_id)
            .expect("late contact resolves through the real provider");
        assert_eq!(contact.kind, "contacts.contact");
        assert_eq!(app.resource_exact_materialized.get(), 1);
        let group_id = crate::actions::group_resource_id("Late group");
        let group = ContactsApp::publish_resource(&app, &doc, &group_id)
            .expect("late group resolves through the real provider");
        assert_eq!(group.kind, "contacts.group");
        app.store.contacts.pop();
        assert!(
            ContactsApp::publish_resource(&app, &doc, &contact_id).is_none(),
            "a stale exact id must not redirect to the preceding contact"
        );

        // The list of a large book publishes every row but paints a window.
        app.list_stale = true;
        app.publish();
        assert_eq!(app.shared.borrow().rows.len(), LARGE - 1);
    }

    // --- phase: ordinary_local_update_syncs_a_large_book_without_full_retained_rebuild ---
    {
        const LARGE: usize = 5_000;
        let (mut app, mut doc) = synced();
        let template = app.store.contacts[0].clone();
        let mut contacts = Vec::with_capacity(LARGE);
        contacts.extend((0..LARGE - 1).map(|index| Contact {
            id: uuid::Uuid::from_u128(9_000_000 + index as u128),
            first: format!("Directory {index}"),
            ..Contact::default()
        }));
        contacts.push(template.clone());
        app.store.contacts = contacts;
        app.rebuild_resource_index();
        app.refresh_search_provider();
        app.search_heads.clone_from(&app.model_heads);
        app.resource_index_rebuilds.set(0);
        app.local_change_index_lookups.set(0);
        let refreshes_before = app.search_provider.full_refreshes();

        let id = template.id;
        let action = crate::actions::set_favorite_action(id.to_string(), !template.favorite);
        app.try_perform_action(&mut doc, &action)
            .expect("current local row accepts an action");
        let position = *app.resource_index.contacts.get(&id).unwrap();
        assert_eq!(position, LARGE - 1);
        app.store.contacts[position].favorite = !template.favorite;
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.resource_index_rebuilds.get(), 0);
        assert_eq!(app.search_provider.full_refreshes(), refreshes_before);
        assert!(app.local_change_index_lookups.get() <= 2);
    }

    // --- phase: search_provider_row_invokes_the_exported_open_action ---
    {
        let mut app = ContactsApp::new();
        for c in sample_contacts() {
            app.store.upsert(c);
        }
        app.rebuild_resource_index();
        app.refresh_search_provider();
        let id = id_named(&app, "Grace Hopper");
        let mut doc = AutoCommit::new();
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&ui::resources::resources(&app, &doc));
        palette.set_providers(ui::search::capability::search_providers(&app));
        palette.open();
        palette.set_query("navy");
        assert_eq!(palette.result_labels(), vec!["Grace Hopper"]);
        palette.activate_selected();
        let operation = palette
            .take_operation()
            .expect("canonical search hit queued a broker operation");
        assert_eq!(operation.kind, tilo_actions::OperationKind::View);
        let encoded = operation.encoded().expect("prepared encoding");
        assert!(matches!(
            Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    operation.kind,
                    &encoded.type_id,
                    &encoded.payload
                ),
                &mut doc,
                &ROOT
            )
            .expect("open operation succeeds"),
            Some(ui::widget::ResourceOperationStart::Completed(_))
        ));
        assert_eq!(app.selected, Some(id), "the open action selected Grace");
    }

    // --- phase: search_activation_reveals_a_contact_hidden_by_the_group ---
    {
        let mut app = ContactsApp::new();
        for c in sample_contacts() {
            app.store.upsert(c);
        }
        app.rebuild_resource_index();
        app.set_group_filter(Some("Friends".to_string()));
        let id = id_named(&app, "Grace Hopper");
        assert!(!app.visible_ids().contains(&id));
        let mut doc = AutoCommit::new();
        let resources = ui::resources::resources(&app, &doc);
        let address = crate::actions::open_contact_address(id);
        let spec = tilo_actions::door::resolve(&resources, &address)
            .expect("the search address resolves in the agent-visible tree");
        let tilo_actions::ActionInput::Immediate(encoded) = &spec.input else {
            panic!("open takes no arguments")
        };
        Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                tilo_actions::InvocationId::new(),
                spec.kind,
                &encoded.type_id,
                &encoded.payload,
            ),
            &mut doc,
            &ROOT,
        )
        .expect("open operation succeeds");
        assert_eq!(app.selected, Some(id));
        assert!(app.group_filter.is_none(), "opening reveals everyone");
        assert!(app.visible_ids().contains(&id));
    }

    // --- phase: group_open_operation_reveals_its_destination ---
    {
        let mut app = ContactsApp::new();
        for contact in sample_contacts() {
            app.store.upsert(contact);
        }
        app.set_group_filter(Some("Friends".into()));
        let operation = crate::actions::ContactsOperation::OpenGroup {
            name: "work".into(),
        };
        let payload = serde_json::to_vec(&operation).unwrap();
        Widget::start_operation(
            &mut app,
            &ui::widget::ResourceOperation::local(
                tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::View,
                crate::actions::CONTACTS_OPERATION_TYPE,
                &payload,
            ),
            &mut AutoCommit::new(),
            &ROOT,
        )
        .expect("group open succeeds");
        assert_eq!(app.group_filter.as_deref(), Some("Work"));
        assert_eq!(
            app.destination.with(Clone::clone).as_deref(),
            Some("group:Work"),
            "the sidebar shows the group chosen"
        );
        assert!(app.visible_ids().iter().all(|id| {
            app.store
                .get(*id)
                .is_some_and(|c| c.groups.iter().any(|g| group_names_equal(&g.value, "Work")))
        }));
    }
}

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "contacts-app-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

/// Run the live Import action under a recording host whose multi-select
/// chooser answers `outcome`, and check the app asked for that chooser.
fn import_with(app: &mut ContactsApp, outcome: ImportOutcome) {
    let (services, recorder) = ui::host::HostServices::recording();
    recorder.respond_to_pick_multiple(outcome);
    ui::host::scoped(&services, || app.request_import());
    let picks = recorder.pick_requests();
    assert_eq!(picks.len(), 1, "one file chooser was requested");
    assert_eq!(picks[0].title(), "Import contacts");
    assert!(picks[0].is_multiple(), "Import picks several files at once");
}

/// Export asks for a format for the people the list shows, then writes the
/// file the Save dialog chose; nothing is written before.
#[test]
#[serial_test::serial]
fn export_asks_a_format_and_writes_the_chosen_file() {
    let dir = scratch_dir();
    let vcf = dir.join("out.vcf");
    let (mut app, mut doc) = synced();
    app.set_group_filter(Some("Friends".into()));
    app.publish();
    app.open_menu_at(PendingMenu::More, Rect::new(500.0, 40.0, 532.0, 72.0));
    let Some(OpenMenu::More(rows)) = &app.open_menu else {
        panic!("the More menu is open");
    };
    let export = rows
        .iter()
        .position(|row| *row == Some(MoreAction::Export))
        .unwrap();
    app.run_menu_choice(&mut doc, export);
    app.publish();
    let draft = app.export.as_ref().expect("export asks for a format");
    assert_eq!(draft.ids.len(), 2, "the group's people are exported");
    assert_eq!(
        draft.summary,
        "2 contacts in Friends are saved as one vCard file."
    );
    assert!(matches!(app.dialog, Some((DialogKey::Export, _))));
    draft.format.set("3.0".into());

    let (services, recorder) = ui::host::HostServices::recording();
    recorder.respond_to_pick(ExportOutcome::Opened(vcf.clone()));
    let _host = ui::host::enter(&services);
    ask(&mut app, &mut doc, Request::Dialog(DialogChoice::Primary));
    assert!(app.export.is_none(), "choosing Export closes the dialog");
    {
        let (ids, version) = app.pending_export.as_ref().expect("the export is parked");
        assert_eq!(ids.len(), 2);
        assert_eq!(*version, VCardVersion::V3);
    }
    assert!(app.portal_in_flight());
    assert!(
        !vcf.exists(),
        "nothing is written until a destination is chosen"
    );
    assert_eq!(recorder.pick_requests()[0].title(), "Export contacts");

    app.sync(&mut doc, &ROOT);
    assert!(app.pending_export.is_none());
    let body = std::fs::read_to_string(&vcf).expect("export wrote the chosen file");
    assert_eq!(body.matches("BEGIN:VCARD").count(), 2);
    assert!(body.contains("VERSION:3.0"));

    // Asking with nobody to export opens nothing.
    assert!(!app.request_export(Vec::new(), VCardVersion::V4));
    assert!(toast(&app).is_some_and(|message| message.to_lowercase().contains("select")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The on-disk book persists creations and deletions, merges with a
/// diverged replica, and imports real vCard files.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one case"
)]
#[test]
#[serial_test::serial]
fn saved_book_persists_across_restarts_and_imports() {
    // --- phase: created_contact_persists_across_a_restart ---
    {
        let home = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_DATA_HOME", home.path())
            .remove("TILO_PACKAGE_ID");
        let mut store = ui::persist::DocStore::open_for("so.tilo.contacts").unwrap();
        let mut doc = store.load_or_new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty());
        let mut c = Contact::new();
        c.first = "Persisted".into();
        c.last = "Person".into();
        c.phones.push(Labeled::new("mobile", "+1 555 0100"));
        app.begin_edit(c, true);
        app.save_edit(&mut doc);
        assert_eq!(app.store.len(), 1);
        store.save(&mut doc).unwrap();
        drop((store, doc, app));

        let mut store2 = ui::persist::DocStore::open_for("so.tilo.contacts").unwrap();
        let mut doc2 = store2.load().unwrap().expect("the store has a document");
        let mut app2 = ContactsApp::new();
        app2.sync(&mut doc2, &ROOT);
        assert_eq!(app2.store.len(), 1);
        assert_eq!(app2.store.contacts[0].first, "Persisted");
        assert_eq!(app2.store.contacts[0].phones[0].value, "+1 555 0100");
    }

    // --- phase: a_saved_book_still_merges_with_a_replica_that_diverged_before_the_save ---
    {
        let home = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_DATA_HOME", home.path())
            .remove("TILO_PACKAGE_ID");
        let mut store = ui::persist::DocStore::open_for("so.tilo.contacts").unwrap();
        let mut doc = store.new_document();
        let mut shared = Contact::new();
        shared.first = "Ada".into();
        let shared_id = shared.id;
        ContactsDoc::upsert_contact(&mut doc, &shared);
        store.save(&mut doc).unwrap();
        let mut peer = doc.fork();
        let mut theirs = Contact::new();
        theirs.first = "Grace".into();
        let their_id = theirs.id;
        ContactsDoc::upsert_contact(&mut peer, &theirs);
        let mut mine = Contact::new();
        mine.first = "Alan".into();
        let my_id = mine.id;
        ContactsDoc::upsert_contact(&mut doc, &mine);
        store.save(&mut doc).unwrap();
        drop((store, doc));

        let mut store2 = ui::persist::DocStore::open_for("so.tilo.contacts").unwrap();
        let mut restored = store2.load().unwrap().expect("the saved document");
        restored
            .merge_incoming(&mut peer)
            .expect("a stored book merges with a peer");
        let book = ContactsDoc::list_contacts(&restored);
        assert_eq!(book.len(), 3);
        for id in [shared_id, my_id, their_id] {
            assert_eq!(book.iter().filter(|c| c.id == id).count(), 1);
        }
        assert_eq!(restored.get_all(&ROOT, "contacts").unwrap().len(), 1);
    }

    // --- phase: import_reads_real_vcf_files ---
    {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let vcf_a = dir.join("work.vcf");
        std::fs::write(
            &vcf_a,
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Imported Person\r\nN:Person;Imported;;;\r\nEMAIL;TYPE=HOME:imported@example.com\r\nEND:VCARD\r\n",
        )
        .unwrap();
        let vcf_b = dir.join("family.vcard");
        std::fs::write(
            &vcf_b,
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ada Byron\r\nN:Byron;Ada;;;\r\nTEL:+1 555 0100\r\nEND:VCARD\r\n\
             BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nORG:US Navy\r\nEND:VCARD\r\n",
        )
        .unwrap();
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        let summary = app.import_paths(&mut doc, &[vcf_a, vcf_b]);
        assert_eq!(summary.created, 3);
        assert_eq!(app.store.len(), 3);
        assert_eq!(
            ContactsDoc::list_contacts(&doc).len(),
            3,
            "written to the doc"
        );
        app.publish();
        assert_eq!(list_ids(&app).len(), 3, "and listed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: deleting_a_contact_persists_the_removal ---
    {
        let home = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_DATA_HOME", home.path())
            .remove("TILO_PACKAGE_ID");
        let book = sample_contacts();
        let removed = book[0].id;
        let mut store = ui::persist::DocStore::open_for("so.tilo.contacts").unwrap();
        let mut doc = store.new_document();
        for c in &book {
            ContactsDoc::upsert_contact(&mut doc, c);
        }
        store.save(&mut doc).unwrap();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        let before = app.store.len();
        app.delete_selected(&mut doc, removed);
        store.save(&mut doc).unwrap();
        drop((store, doc, app));
        let mut store2 = ui::persist::DocStore::open_for("so.tilo.contacts").unwrap();
        let doc2 = store2.load().unwrap().expect("document after restart");
        let reloaded = ContactsDoc::list_contacts(&doc2);
        assert_eq!(reloaded.len(), before - 1);
        assert!(!reloaded.iter().any(|c| c.id == removed));
    }
}

/// Re-imports recognise identities, hostile UIDs go to review, and empty
/// imports are silent no-ops.
#[test]
fn imports_recognise_identities_without_duplicating() {
    // --- phase: reimporting_an_export_recognises_its_uid ---
    {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let card = sample_contacts().into_iter().next().unwrap();
        let vcf = dir.join("contacts-export.vcf");
        std::fs::write(&vcf, vcard::to_vcard(&card, VCardVersion::V4)).unwrap();
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        assert_eq!(
            app.import_paths(&mut doc, std::slice::from_ref(&vcf))
                .created,
            1
        );
        let second = app.import_paths(&mut doc, std::slice::from_ref(&vcf));
        assert_eq!(second.created, 0);
        assert_eq!(second.unchanged, 1);
        assert_eq!(app.store.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: hostile_known_uid_is_reported_for_review_without_overwriting_contact ---
    {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let original = sample_contacts().into_iter().next().unwrap();
        let malicious = vcard::to_vcard(&original, VCardVersion::V4)
            .replace(&original.emails[0].value, "attacker@example.invalid");
        let original_file = dir.join("original.vcf");
        let malicious_file = dir.join("malicious.vcf");
        std::fs::write(&original_file, vcard::to_vcard(&original, VCardVersion::V4)).unwrap();
        std::fs::write(&malicious_file, malicious).unwrap();
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.import_paths(&mut doc, &[original_file]).created, 1);
        let result = app.import_paths(&mut doc, &[malicious_file]);
        assert_eq!(result.conflicts, 1);
        let resources = ui::resources::resources(&app, &doc);
        let review = resources[0]
            .children
            .iter()
            .find(|resource| resource.kind == "contacts.import-conflict")
            .expect("the pending collision has an addressable review resource");
        assert!(
            review
                .actions
                .iter()
                .any(|action| action.id == "keep-existing")
        );
        assert!(
            review
                .actions
                .iter()
                .any(|action| action.id == "import-as-new")
        );
        assert_eq!(app.store.len(), 1);
        assert_eq!(
            app.store.get(original.id).unwrap().emails[0].value,
            original.emails[0].value
        );
        assert!(toast(&app).is_some_and(|message| message.contains("review")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: import_of_nothing_is_a_clean_noop ---
    {
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        assert!(!app.import_paths(&mut doc, &[]).completed());
        import_with(&mut app, ImportOutcome::Cancelled);
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty());
        assert!(ContactsDoc::list_contacts(&doc).is_empty());
        assert!(app.toast.is_none(), "a cancelled import stays silent");
    }
}

/// Import jobs are portal-brokered, merge through sync, and parser
/// generations fence stale or exhausted workers.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one case"
)]
#[test]
fn import_jobs_are_brokered_and_generation_fenced() {
    // --- phase: import_drain_merges_selected_files_through_sync ---
    {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let vcf = dir.join("incoming.vcf");
        std::fs::write(
            &vcf,
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Drained Person\r\nN:Person;Drained;;;\r\nEND:VCARD\r\n",
        )
        .unwrap();
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        import_with(&mut app, ImportOutcome::Selected(vec![vcf]));
        assert!(app.portal_in_flight());
        let deadline = ui::time::now() + HANG_BOUND;
        while ui::time::now() < deadline {
            app.sync(&mut doc, &ROOT);
            if !app.portal_in_flight() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!app.portal_in_flight());
        assert_eq!(app.store.len(), 1);
        assert_eq!(app.store.contacts[0].first, "Drained");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: stale_background_parser_cannot_apply ---
    {
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        let mut stale = Contact::new();
        stale.first = "Stale worker".into();
        let (sender, receiver) = mpsc::sync_channel(1);
        app.import_generation = 2;
        app.import_parse_rx = Some(receiver);
        let invocation = tilo_actions::InvocationId::new();
        app.resource_invocation = Some(invocation);
        sender
            .send(ParsedImport {
                generation: 1,
                contacts: vec![stale],
                errors: 0,
            })
            .unwrap();
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty());
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &doc, &ROOT),
            Some(tilo_actions::InvocationUpdate::Failed { .. })
        ));
    }

    // --- phase: brokered_file_operations_refuse_direct_paths ---
    {
        let (mut app, mut doc) = synced();
        for operation in [
            crate::actions::ContactsOperation::ImportVcard {
                path: Some("/etc/passwd".into()),
            },
            crate::actions::ContactsOperation::ExportAllVcard {
                path: Some("/tmp/contacts.vcf".into()),
            },
        ] {
            let payload = serde_json::to_vec(&operation).unwrap();
            let error = app
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        crate::actions::CONTACTS_OPERATION_TYPE,
                        &payload,
                    ),
                    &mut doc,
                    &ROOT,
                )
                .expect_err("a broker cannot bypass the system file picker");
            assert!(error.contains("system file picker"));
            assert!(app.resource_invocation.is_none());
        }
    }

    // --- phase: exhausted_parser_generation_refuses_a_new_import ---
    {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let vcf = dir.join("generation-limit.vcf");
        std::fs::write(&vcf, "BEGIN:VCARD\r\nFN:Never Parsed\r\nEND:VCARD\r\n").unwrap();
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        let invocation = tilo_actions::InvocationId::new();
        app.resource_invocation = Some(invocation);
        app.import_generation = u64::MAX;
        import_with(&mut app, ImportOutcome::Selected(vec![vcf]));
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty());
        assert_eq!(
            app.import_generation,
            u64::MAX,
            "the exhausted token is never reused"
        );
        assert!(toast(&app).is_some_and(|message| message.contains("restart Contacts")));
        assert!(matches!(
            app.poll_resource_operation(invocation, false, &doc, &ROOT),
            Some(tilo_actions::InvocationUpdate::Failed { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: cancelling_at_generation_max_invalidates_the_worker_without_wrapping ---
    {
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        let (sender, receiver) = mpsc::sync_channel(1);
        let invocation = tilo_actions::InvocationId::new();
        app.resource_invocation = Some(invocation);
        app.import_parse_rx = Some(receiver);
        app.import_generation = u64::MAX;
        let update = app
            .poll_resource_operation(invocation, true, &doc, &ROOT)
            .expect("accepted import is settled when cancelled");
        assert!(matches!(
            update,
            tilo_actions::InvocationUpdate::Cancelled { .. }
        ));
        assert_eq!(app.import_generation, u64::MAX);
        assert!(toast(&app).is_some_and(|message| message.contains("Restart Contacts")));
        let _ = sender.send(ParsedImport {
            generation: u64::MAX,
            contacts: vec![Contact::new()],
            errors: 0,
        });
        app.sync(&mut doc, &ROOT);
        assert!(app.store.is_empty());
    }
}

/// An imported card that collides with a known identity is a decision:
/// keep the existing contact, or import it as a new one.
#[test]
fn import_conflicts_are_decided_in_a_dialog() {
    fn conflicting_app() -> (ContactsApp, AutoCommit, Contact, Contact) {
        let original = sample_contacts().into_iter().next().unwrap();
        let mut incoming = original.clone();
        incoming.emails[0].value = "attacker@example.invalid".into();
        let mut doc = AutoCommit::new();
        let mut app = ContactsApp::new();
        app.sync(&mut doc, &ROOT);
        let mut summary = ImportSummary::default();
        let mut first_new = None;
        app.absorb_import_contact(&mut doc, original.clone(), &mut summary, &mut first_new);
        app.absorb_import_contact(&mut doc, incoming.clone(), &mut summary, &mut first_new);
        assert_eq!(summary.conflicts, 1);
        app.publish();
        (app, doc, original, incoming)
    }

    let (mut app, mut doc, original, _) = conflicting_app();
    assert!(matches!(
        app.dialog,
        Some((DialogKey::ImportConflict(_), _))
    ));
    ask(&mut app, &mut doc, Request::Dialog(DialogChoice::Dismiss));
    assert_eq!(app.import_conflicts.len(), 1, "Escape decides nothing");
    ask(&mut app, &mut doc, Request::Dialog(DialogChoice::Secondary));
    assert!(app.import_conflicts.is_empty());
    assert!(app.dialog.is_none());
    assert_eq!(app.store.len(), 1);
    assert_eq!(
        app.store.get(original.id).unwrap().emails[0].value,
        original.emails[0].value
    );

    let (mut app, mut doc, original, incoming) = conflicting_app();
    ask(&mut app, &mut doc, Request::Dialog(DialogChoice::Primary));
    assert!(app.import_conflicts.is_empty());
    assert_eq!(app.store.len(), 2);
    assert!(app.store.contacts.iter().any(|contact| {
        contact.id != original.id && contact.emails[0].value == incoming.emails[0].value
    }));
    assert!(
        app.last_dispatched().is_some(),
        "Import as new uses ContactsAction"
    );

    // The dialog is the window's top layer, with its decisions named.
    let (app, doc, _, _) = conflicting_app();
    let mut state = ui::State::new();
    state.adopt_doc(doc);
    let harness = FrameHarness::for_root(state, app, (1280, 800), Theme::dark());
    assert!(named(&harness, "Import as new"));
    assert!(named(&harness, "Keep existing"));
}

/// A likely duplicate is offered for review; the person picks each
/// conflicting value and the merge keeps their picks.
#[test]
fn duplicates_are_reviewed_field_by_field_before_merging() {
    let (mut app, mut doc) = synced();
    let ada = selected_contact(&app);
    let mut copy = ada.clone();
    copy.id = uuid::Uuid::from_u128(ada.id.as_u128() + 0x100);
    copy.org = "Babbage & Co.".into();
    tilo_actions::Action::apply(&crate::actions::create_contact_action(&copy), &mut doc).unwrap();
    app.sync(&mut doc, &ROOT);
    assert!(
        app.duplicate_candidate.is_some(),
        "the copy is a likely duplicate"
    );

    app.open_menu_at(PendingMenu::More, Rect::new(500.0, 40.0, 532.0, 72.0));
    let Some(OpenMenu::More(rows)) = &app.open_menu else {
        panic!("the More menu is open");
    };
    let review = rows
        .iter()
        .position(|row| *row == Some(MoreAction::ReviewDuplicate))
        .expect("the More menu offers the review");
    app.run_menu_choice(&mut doc, review);
    app.publish();
    let opened = app.duplicate_review.as_ref().expect("the review is open");
    assert_eq!(opened.survivor.id, ada.id);
    assert_eq!(opened.source.id, copy.id);
    assert_eq!(
        opened
            .choices
            .iter()
            .map(|(field, _)| *field)
            .collect::<Vec<_>>(),
        vec![crate::actions::MergeField::Organization],
        "only the fields that differ are asked about"
    );
    assert!(matches!(app.dialog, Some((DialogKey::Duplicate(..), _))));
    opened.choose(crate::actions::MergeField::Organization, true);

    ask(&mut app, &mut doc, Request::Dialog(DialogChoice::Primary));
    app.sync(&mut doc, &ROOT);
    let book = ContactsDoc::list_contacts(&doc);
    assert!(
        !book.iter().any(|c| c.id == copy.id),
        "the copy merged away"
    );
    assert_eq!(
        book.iter().find(|c| c.id == ada.id).unwrap().org,
        "Babbage & Co.",
        "the merge kept the chosen value"
    );
    assert!(toast(&app).is_some_and(|message| message.starts_with("Merged")));
}

/// Shared facts read as a phrase.
#[test]
fn shared_facts_read_as_a_phrase() {
    use crate::actions::DuplicateReason;
    assert_eq!(dialogs::shared_facts(&[DuplicateReason::Email]), "an email");
    assert_eq!(
        dialogs::shared_facts(&[
            DuplicateReason::Email,
            DuplicateReason::Phone,
            DuplicateReason::Name
        ]),
        "an email, a phone number and a name"
    );
}

/// Every capture scenario stages without panicking, in a wide and a phone
/// window, dark and light.
#[test]
fn every_capture_scenario_renders() {
    for scenario in [
        "new",
        "edit",
        "calendar",
        "export",
        "unsaved",
        "groups",
        "favorites",
        "context",
        "more",
        "confirm",
        "import-conflict",
        "duplicate-review",
        "empty",
    ] {
        for (viewport, theme) in [((1280, 800), Theme::dark()), ((390, 844), Theme::light())] {
            let mut app = ContactsApp::sample();
            app.stage_scenario(scenario);
            let mut harness = FrameHarness::for_root(ui::State::new(), app, viewport, theme);
            harness.send(&WindowEvent::RedrawRequested);
            let app = harness.root::<ContactsApp>();
            match scenario {
                "new" | "edit" | "calendar" => assert_eq!(app.mode, Mode::Edit, "{scenario}"),
                "export" => assert!(app.export.is_some()),
                "confirm" => assert!(app.confirm_delete.is_some()),
                "duplicate-review" => assert!(app.duplicate_review.is_some()),
                "empty" => assert!(app.store.is_empty()),
                _ => {}
            }
        }
    }
}

/// An address and its kind label read like an envelope.
#[test]
fn details_read_like_an_envelope() {
    let address = Address {
        street: "12 St James's Square".into(),
        city: "London".into(),
        postal: "SW1Y 4LE".into(),
        country: "United Kingdom".into(),
        ..Address::default()
    };
    assert_eq!(
        crate::detail::address_lines(&address),
        "12 St James's Square\nLondon SW1Y 4LE\nUnited Kingdom"
    );
    assert_eq!(crate::detail::kind_label("mobile"), "Mobile");
    assert_eq!(crate::detail::kind_label("WORK"), "Work");
    assert_eq!(crate::detail::kind_label("cell"), "Mobile");
    assert_eq!(crate::detail::kind_label("Ham radio"), "Ham radio");
    assert_eq!(crate::detail::kind_label(""), "Other");
}

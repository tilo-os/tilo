use super::*;
use crate::doc::ContactsDoc;
use crate::form::{Slot, TYPE_OPTIONS, TypeTarget};
use crate::model::sample_contacts;
use crate::model::{Address, Birthday};
use crate::vcard;
use crate::view::DialogChoice;
use automerge::AutoCommit;
use automerge::ROOT;
use std::path::PathBuf;
use taffy::{AvailableSpace, Dimension, Size, Style};
use tilo_automerge::ApplyIncoming;
use ui::a11y::{AccessNode, Role};
use ui::headless::FrameHarness;
use ui::permissions::PermissionContext;
use ui::tokens::colors::ON_FILL_MIN_CONTRAST;
use vello::kurbo::Point;

fn target(app: &ContactsApp, id: &str) -> Rect {
    fn find(widget: &dyn Widget, id: &str) -> Option<Rect> {
        if widget.test_id() == Some(id) {
            return Some(widget.get_rect());
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| find(child, id))
    }
    find(app, id).unwrap_or_else(|| panic!("production target {id} is absent"))
}
fn named_widget<'a>(widget: &'a dyn Widget, name: &str, role: Role) -> Option<&'a dyn Widget> {
    if widget
        .accessibility()
        .is_some_and(|node| node.name.as_deref() == Some(name) && node.role == role)
    {
        return Some(widget);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| named_widget(child, name, role))
}

fn current_dialog(widget: &dyn Widget) -> &ui::widget::ModalDialog<ui::widget::Text> {
    fn find(widget: &dyn Widget) -> Option<&ui::widget::ModalDialog<ui::widget::Text>> {
        if let Some(dialog) = widget
            .as_any()
            .and_then(|value| value.downcast_ref::<ui::widget::ModalDialog<ui::widget::Text>>())
        {
            return Some(dialog);
        }
        widget.children().into_iter().find_map(find)
    }
    find(widget).expect("the actual shared modal is hosted")
}
fn menu_rows(app: &ContactsApp) -> Vec<Rect> {
    let Some(OpenMenu::Contact(_, rows)) = &app.open_menu else {
        panic!("the actual contact menu is open");
    };
    (0..rows.len())
        .map(|index| app.menu.item_rect(index).expect("actual menu row geometry"))
        .collect()
}
fn menu_index(app: &ContactsApp, action: ContextAction) -> usize {
    let Some(OpenMenu::Contact(_, rows)) = &app.open_menu else {
        panic!("the actual contact menu is open");
    };
    rows.iter().position(|row| *row == Some(action)).unwrap()
}

fn list_bounds(app: &ContactsApp) -> Rect {
    fn find(widget: &dyn Widget) -> Option<Rect> {
        if widget.name() == "ContactList" {
            return Some(widget.get_rect());
        }
        widget.children().into_iter().find_map(find)
    }
    find(app).expect("the production contact list is hosted")
}

fn form_widget(app: &ContactsApp, slot: Slot) -> Option<&dyn Widget> {
    fn find<'a>(widget: &'a dyn Widget, name: &str) -> Option<&'a dyn Widget> {
        if widget.name() == "TextInput"
            && widget.accessibility().and_then(|node| node.name).as_deref() == Some(name)
        {
            return Some(widget);
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| find(child, name))
    }
    find(app, &crate::form::field_name(slot))
}
fn form_slots(form: &EditForm) -> Vec<Slot> {
    let mut slots = vec![
        Slot::First,
        Slot::Last,
        Slot::Org,
        Slot::Title,
        Slot::Website,
        Slot::Birthday,
        Slot::Notes,
        Slot::Groups,
    ];
    slots.extend((0..form.draft.phones.len()).map(Slot::Phone));
    slots.extend((0..form.draft.emails.len()).map(Slot::Email));
    slots.extend((0..form.draft.addresses.len()).flat_map(|i| {
        [
            Slot::AddrStreet(i),
            Slot::AddrCity(i),
            Slot::AddrRegion(i),
            Slot::AddrPostal(i),
            Slot::AddrCountry(i),
        ]
    }));
    slots
}

/// The surface presenting the sample book, as a capture shows it. Detached
/// from a door, it edits nothing: tests of what an edit writes run the app.
fn sampled() -> ContactsApp {
    let mut app = ContactsApp::new();
    app.present_book(&crate::resource::ContactsState {
        contacts: crate::resource::sample_book(),
    });
    app
}

/// The book `app` presents, as the framework publishes it.
fn published(app: &ContactsApp) -> Vec<tilo_actions::Resource> {
    vec![ui::tilo_resource::publish::<crate::resource::Contacts>(
        "contacts.book",
        "Contacts",
        &crate::resource::ContactsState {
            contacts: app.store.contacts.clone(),
        },
    )]
}

/// The sample book synced into a fresh document.
fn synced() -> Live {
    Live::sample(1280, 800)
}

/// Queue `request` as a pane or button would, and let the app apply it.
fn ask(app: &mut Live, request: Request) {
    app.request(request);
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
    let mut app = synced();
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
        app.invoke(&crate::actions::patch_contact_action(&before, &contact));
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

/// The list's order control sorts by name or by organization.
#[test]
fn the_sort_control_orders_the_list() {
    let mut app = synced();
    let by_name = list_ids(&app);
    let names: Vec<String> = by_name
        .iter()
        .map(|id| app.store.get(*id).unwrap().last.clone())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "the list is sorted by last name");

    assert!(
        !app.more_rows()
            .iter()
            .any(|(action, _)| *action == MoreAction::ReviewDuplicate),
        "no review is offered without a likely duplicate"
    );
    ask(&mut app, Request::Sort(SortKey::Org));
    assert!(
        app.sort_choice.with(|value| value == "organization"),
        "the control shows the order"
    );
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
        &mut ui::widget::Edits::lend(&mut state.doc),
        &ROOT,
    ));
    app.apply_requests(&mut ui::widget::Edits::lend(&mut state.doc));
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

/// Asked for from the keyboard or assistive technology, a contact's menu
/// opens under its row, leaving the row readable.
#[test]
fn a_contact_menu_without_a_pointer_opens_under_its_row() {
    let mut harness = harness((1280, 800));
    let (grace, row) = {
        let app = harness.root::<ContactsApp>();
        let grace = id_named(app, "Grace Hopper");
        let row = app
            .shared
            .borrow()
            .painted
            .iter()
            .find(|(id, _)| *id == grace)
            .map(|(_, rect)| *rect)
            .unwrap();
        (grace, row)
    };
    harness
        .root_mut::<ContactsApp>()
        .shared
        .borrow_mut()
        .push(Request::ContextMenu(grace, None));
    harness.send(&WindowEvent::RedrawRequested);
    harness.send(&WindowEvent::RedrawRequested);
    let app = harness.root::<ContactsApp>();
    assert!(
        matches!(&app.open_menu, Some(OpenMenu::Contact(id, _)) if *id == grace),
        "Grace's menu is open"
    );
    let first = app.menu.item_rect(0).expect("the menu has rows");
    assert!(
        first.y0 >= row.y1 - 0.5,
        "the menu's first row ({first:?}) is under the contact's row ({row:?})"
    );
}

/// A contact's action menu reaches the canonical edit, favourite, export
/// and delete paths.
#[test]
fn the_contact_menu_runs_the_canonical_actions() {
    let mut app = synced();
    let grace = id_named(&app, "Grace Hopper");
    let was_favorite = app.store.get(grace).unwrap().favorite;
    ask(
        &mut app,
        Request::ContextMenu(grace, Some(Point::new(400.0, 200.0))),
    );
    assert_eq!(app.selected, Some(grace));
    let rows = match &app.open_menu {
        Some(OpenMenu::Contact(id, rows)) if *id == grace => rows.clone(),
        _ => panic!("Grace's menu is open"),
    };
    assert!(app.menu.is_open());
    let index = |action| rows.iter().position(|row| *row == Some(action)).unwrap();

    app.choose_menu(index(ContextAction::Favorite));
    assert_eq!(app.store.get(grace).unwrap().favorite, !was_favorite);
    assert_eq!(
        app.book().iter().find(|c| c.id == grace).unwrap().favorite,
        !was_favorite,
        "the favourite went through the document"
    );

    ask(
        &mut app,
        Request::ContextMenu(grace, Some(Point::new(400.0, 200.0))),
    );
    app.choose_menu(index(ContextAction::Export));
    app.publish();
    assert!(
        app.export
            .as_ref()
            .is_some_and(|export| export.ids == vec![grace]),
        "export asks for the format of just this contact"
    );
    assert!(matches!(app.dialog, Some((DialogKey::Export, _))));
    ask(&mut app, Request::Dialog(DialogChoice::Secondary));
    assert!(app.export.is_none());

    ask(
        &mut app,
        Request::ContextMenu(grace, Some(Point::new(400.0, 200.0))),
    );
    app.choose_menu(index(ContextAction::Delete));
    assert_eq!(app.confirm_delete, Some(grace));
    ask(&mut app, Request::Dialog(DialogChoice::Primary));
    assert!(app.store.get(grace).is_none());
    assert!(!app.book().iter().any(|c| c.id == grace));
    assert_eq!(toast(&app), Some("Contact deleted"));
    assert!(
        app.app
            .state()
            .contacts
            .iter()
            .all(|contact| contact.id != grace),
        "the deletion is the palette's action"
    );

    let next = app.selected.unwrap();
    ask(&mut app, Request::ContextMenu(next, Some(Point::ORIGIN)));
    app.choose_menu(index(ContextAction::Edit));
    assert_eq!(app.mode, Mode::Edit);
}

/// The person's header commands: favourite toggles through the document,
/// Delete asks first, and the website opens.
#[test]
fn header_commands_favorite_delete_and_open_the_website() {
    let mut app = synced();
    let ada = selected_contact(&app);
    ask(&mut app, Request::ToggleFavorite);
    assert_eq!(app.store.get(ada.id).unwrap().favorite, !ada.favorite);
    assert_eq!(
        app.header_shown,
        Some(HeaderKey::View {
            favorite: !ada.favorite
        }),
        "the toggle follows the person"
    );

    ask(&mut app, Request::DeleteSelected);
    assert_eq!(app.confirm_delete, Some(ada.id));
    ask(&mut app, Request::Dialog(DialogChoice::Dismiss));
    assert!(app.confirm_delete.is_none());
    assert!(
        app.store.get(ada.id).is_some(),
        "dismissing deletes nothing"
    );

    ask(&mut app, Request::OpenWebsite(ada.website.clone()));
    assert_eq!(app.opened_url.as_deref(), Some(ada.website.as_str()));
}

/// Saves round-trip through the document; refused saves, favourites and
/// deletes reconcile and announce errors without claiming success.
#[test]
fn retained_forms_preserve_persistence_and_refusals() {
    // --- phase: save_round_trips_through_doc ---
    {
        let mut app = synced();
        let c = selected_contact(&app);
        let id = c.id;
        ask(&mut app, Request::EditSelected);
        app.form
            .as_ref()
            .unwrap()
            .set_value(Slot::Title, "Edited Title");
        ask(&mut app, Request::Save);
        assert_eq!(app.mode, Mode::View);
        let stored = app.book();
        assert_eq!(
            stored.iter().find(|x| x.id == id).unwrap().title,
            "Edited Title"
        );
        assert_eq!(toast(&app), Some("Saved"));
    }

    // --- phase: refused_save_reconciles_and_announces_an_error_without_claiming_success ---
    {
        let mut app = synced();
        let contact = selected_contact(&app);
        app.app
            .merge_peer_edit(|doc| ContactsDoc::delete_contact(doc, contact.id));
        app.settle();
        app.begin_edit(contact.clone(), false);
        app.save_edit();

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
        let mut app = synced();
        let contact = selected_contact(&app);
        app.app
            .merge_peer_edit(|doc| ContactsDoc::delete_contact(doc, contact.id));
        app.settle();

        assert!(
            !app.toggle_favorite(contact.id),
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

        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = theme.tokens.colors.surfaces.base;
            let selected = ui::widget::ChipStyle::Live.colors(&theme, ground, true, false);
            assert!(
                ui::a11y::contrast_ratio(selected.ink, selected.fill) >= ON_FILL_MIN_CONTRAST,
                "selected chip label must remain readable on its resolved fill"
            );
            assert!(
                ui::a11y::contrast_ratio(
                    selected.edge.expect("a selected contact chip has an edge"),
                    ground
                ) >= ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
                "selected chip edge must remain distinguishable from its pane"
            );
        }
    }

    // --- phase: seeds_sample_contacts_and_selects_one ---
    {
        let mut app = sampled();
        let doc = AutoCommit::new();
        app.sync(&doc, &ROOT);
        assert_eq!(app.store.len(), 7);
        assert!(app.selected.is_some(), "a contact must be auto-selected");
        // The seeded selection points at a real, richly-populated record.
        let sel = app.store.get(app.selected.unwrap()).unwrap();
        assert!(!sel.phones.is_empty());
    }

    // --- phase: view_mode_frame_does_not_panic ---
    {
        let mut app = sampled();
        drive_frame(&mut app, 1280, 800);
    }

    // --- phase: edit_mode_with_repeated_fields_frame_does_not_panic ---
    {
        let mut app = sampled();
        // Seed first.
        let doc = AutoCommit::new();
        app.sync(&doc, &ROOT);
        // Enter edit on the selected contact and add repeated fields.
        let c = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        app.begin_edit(c, false);
        app.form.as_mut().unwrap().add(crate::form::RowKind::Phone);
        app.form
            .as_mut()
            .unwrap()
            .add(crate::form::RowKind::Address);
        app.form_epoch = app.form_epoch.wrapping_add(1);
        drive_frame(&mut app, 1280, 800);
        assert_eq!(app.mode, Mode::Edit);
    }

    // --- phase: calendar_and_type_popups_frame_does_not_panic ---
    {
        let mut app = sampled();
        let doc = AutoCommit::new();
        app.sync(&doc, &ROOT);
        let c = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        app.begin_edit(c, false);
        // Calendar popup.
        app.popup = Some(Popup::Calendar {
            year: 1815,
            month: 12,
            anchor: Rect::new(600.0, 400.0, 700.0, 432.0),
        });
        drive_frame(&mut app, 1280, 800);
        // Type menu popup.
        app.popup = Some(Popup::TypeMenu {
            target: TypeTarget::Phone(0),
            anchor: Rect::new(600.0, 200.0, 678.0, 238.0),
        });
        drive_frame(&mut app, 1280, 800);
    }

    // --- phase: importing_state_is_visible_and_announced_as_busy ---
    {
        let mut app = sampled();
        drive_frame(&mut app, 1280, 800);
        app.stage_loading();
        let tree = app.accessibility().expect("contacts accessibility tree");
        let import = access_target(&tree, "contacts.import").expect("import action");
        assert_eq!(import.name.as_deref(), Some("Importing contacts"));
        assert!(import.state.busy);
        assert!(import.state.disabled);
        assert!(!import.state.focusable);
    }
}

/// The window offers importing and exporting the book beside its actions.
#[test]
fn the_window_offers_import_and_export_on_the_book() {
    let mut app = Live::live_sample(1280, 800);
    let resources = app.app.harness().resources();
    let book = resources
        .iter()
        .find(|resource| {
            resource.id.as_str() == <crate::resource::Contacts as ui::tilo_resource::Resource>::KIND
        })
        .expect("the address book");
    for id in ["import-vcard", "export-all-vcard"] {
        assert!(
            book.actions.iter().any(|action| action.id == id),
            "{id} is on the book"
        );
    }
}

/// Search hits and published open operations reveal the addressed contact or group.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one case"
)]
#[test]
fn search_and_open_operations_reveal_their_target() {
    // --- phase: search_hit_invokes_the_exported_open_action ---
    // Contacts folds its address book into the unified search through its
    // resource's entries. A field-only query finds the contact, and the hit
    // invokes the same published `open` view operation agents and
    // accessibility consume.
    {
        let mut app = Live::live_sample(1280, 800);
        let id = app
            .store
            .contacts
            .iter()
            .find(|contact| contact.display_name() == "Grace Hopper")
            .expect("Grace is seeded")
            .id;
        app.selected = None;
        let entries = <crate::resource::Contacts as ui::tilo_resource::Resource>::search(
            &crate::resource::ContactsState {
                contacts: app.book(),
            },
        );
        let hits: Vec<_> = entries
            .iter()
            .filter(|entry| entry.text.to_lowercase().contains("navy"))
            .collect();
        assert_eq!(hits.len(), 1, "the organisation finds one contact");
        assert_eq!(hits[0].title, "Grace Hopper");
        let address = hits[0].action.clone().expect("the hit invokes an action");
        assert_eq!(address, crate::actions::open_contact_address(id));
        app.app
            .call_on(&address.resource, &address.action, serde_json::json!({}))
            .expect("the open operation succeeds");
        app.settle();
        assert_eq!(app.selected, Some(id), "the open action selected Grace");
    }

    // --- phase: search_activation_reveals_a_contact_hidden_by_the_group ---
    {
        let mut app = sampled();
        // Grace Hopper is in "Work" only; narrowing the list to "Friends"
        // (Alan Turing's group) filters her out of the visible set.
        app.group_filter = Some("Friends".to_string());

        let id = app
            .store
            .contacts
            .iter()
            .find(|contact| contact.display_name() == "Grace Hopper")
            .expect("Grace is seeded")
            .id;

        // Precondition: under the Friends filter, Grace is not in the list.
        assert!(
            !app.visible_ids().contains(&id),
            "the Friends group filter hides Grace before activation"
        );

        let resources = published(&app);
        let address = crate::actions::open_contact_address(id);
        let spec = tilo_actions::door::resolve(&resources, &address)
            .expect("the search address resolves in the agent-visible tree");
        let encoded = ui::testing::action_contract::spec_encoding(spec, None);
        let effect = <crate::actions::ContactsEffect as ui::tilo_resource::Effect>::from_payload(
            &encoded.payload,
        )
        .expect("the address names a contacts effect");
        app.perform_effect(effect).expect("open succeeds");

        assert_eq!(app.selected, Some(id), "activation selected Grace");
        assert!(
            app.group_filter.is_none(),
            "opening from search dropped the hiding filter"
        );
        assert!(
            app.visible_ids().contains(&id),
            "Grace is now visible in the list — opened, not just selected behind a filter"
        );
    }

    // --- phase: group_open_operation_reveals_its_destination ---
    {
        let mut app = sampled();
        app.search.set_text("nothing will match this");
        app.group_filter = Some("Friends".into());
        let group = app
            .store
            .all_groups()
            .into_iter()
            .find(|group| group == "Work")
            .expect("samples expose Work");
        app.perform_effect(crate::actions::ContactsEffect::OpenGroup {
            name: group.clone(),
        })
        .expect("group open succeeds");
        assert_eq!(app.group_filter.as_deref(), Some(group.as_str()));
        assert!(
            app.search.value().is_empty(),
            "open reveals a complete group"
        );
        assert!(
            !app.visible_ids().is_empty()
                && app.visible_ids().iter().all(|id| app
                    .store
                    .get(*id)
                    .is_some_and(|c| c.groups.iter().any(|g| group_names_equal(&g.value, &group)))),
            "every revealed row belongs to the canonical group"
        );
    }
}

/// Drive one full five-pass frame against `app` at the given size,
/// encoding the scene on the CPU so any panic in sync/layout/draw
/// surfaces. (GPU rasterization is verified separately by `--shots`.)
fn drive_frame(app: &mut ContactsApp, w: u32, h: u32) {
    drive_frame_with_theme(app, w, h, &Theme::dark());
}

fn drive_frame_with_theme(app: &mut ContactsApp, w: u32, h: u32, theme: &Theme) {
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let doc = AutoCommit::new();

    app.sync(&doc, &ROOT);
    let child = app.build_layout(&mut taffy, theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    {
        let measure_map = node_map.clone();
        let trr = &mut tr;
        taffy
            .compute_layout_with_measure(
                root,
                Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
                |known, avail, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, avail, trr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
    }
    app.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    app.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        theme,
        &presence,
        &permission,
    );
    // Encoding the scene is enough to exercise every draw path; the
    // GPU blit is covered by the `--shots` harness.
    let _ = &scene;
}

/// Narrow windows push the detail over the list (by selection, row press or Enter) and back; wide windows stay docked; resizing reflows the same instance.
#[test]
fn single_pane_push_navigation_follows_width() {
    // --- phase: narrow_frame_resolves_single_pane_push ---
    {
        let mut app = sampled();
        // A narrow viewport drives the single-pane master-detail push.
        drive_frame(&mut app, 480, 860);
        assert!(
            (!app.shell.geometry().side_by_side),
            "a narrow viewport resolves to single-pane push"
        );
        assert_eq!(
            app.shell
                .geometry()
                .stack
                .last()
                .copied()
                .unwrap_or(NavPane::Overview),
            NavPane::Overview,
            "a fresh narrow frame opens to the list"
        );
    }

    // --- phase: narrow_selection_pushes_detail_then_back_returns_to_list ---
    {
        let mut app = sampled();
        let doc = AutoCommit::new();
        app.sync(&doc, &ROOT);
        drive_frame(&mut app, 480, 860);
        assert!((!app.shell.geometry().side_by_side));
        // Snap the push to the detail pane (as a row selection would) and
        // confirm the pushed-detail frame renders without panicking.
        app.shell.show(NavPane::Content);
        app.shell.snap();
        assert_eq!(
            app.shell
                .geometry()
                .stack
                .last()
                .copied()
                .unwrap_or(NavPane::Overview),
            NavPane::Content
        );
        drive_frame(&mut app, 480, 860);
        // The Back affordance must be laid out (hittable) in the pushed pane.
        assert!(
            target(&app, "nav-shell-back").width() > 0.0,
            "pushed detail shows a Back affordance"
        );
    }

    // --- phase: wide_frame_stays_docked_side_by_side ---
    {
        let mut app = sampled();
        drive_frame(&mut app, 1280, 800);
        assert!(
            app.shell.geometry().side_by_side,
            "a desktop-width viewport keeps the docked split"
        );
        // The list pane sits left of the detail pane (classic split).
        assert!(
            app.shell.geometry().overview_column.x1
                <= app.shell.geometry().content_column.x0 + 0.001
        );
    }

    // --- phase: narrow_row_press_pushes_detail_then_back_returns_to_list ---
    {
        fn narrow_row_press_pushes_detail_then_back_returns_to_list() {
            let mut h = Harness::new(460, 880);
            assert!((!h.app.shell.geometry().side_by_side));
            assert_eq!(
                h.app
                    .shell
                    .geometry()
                    .stack
                    .last()
                    .copied()
                    .unwrap_or(NavPane::Overview),
                NavPane::Overview
            );
            // Press a list row → the detail pane is pushed in.
            let row = h.row_center(1);
            assert!(h.press_left(row));
            assert_eq!(
                h.app
                    .shell
                    .geometry()
                    .stack
                    .last()
                    .copied()
                    .unwrap_or(NavPane::Overview),
                NavPane::Content,
                "selecting pushes the detail in"
            );
            h.relayout();
            assert!(
                target(&h.app, "nav-shell-back").width() > 0.0,
                "the Back affordance is laid out"
            );
            // Press Back → return to the list.
            let back = target(&h.app, "nav-shell-back").center();
            assert!(h.press_left(back));
            assert_eq!(
                h.app
                    .shell
                    .geometry()
                    .stack
                    .last()
                    .copied()
                    .unwrap_or(NavPane::Overview),
                NavPane::Overview,
                "Back returns to the full-width list"
            );
        }
        narrow_row_press_pushes_detail_then_back_returns_to_list();
    }

    // --- phase: enter_opens_the_selected_contact_in_narrow_push_navigation ---
    {
        let mut h = Harness::new(420, 800);
        assert!(
            (!h.app.shell.geometry().side_by_side),
            "phone width uses push navigation"
        );
        assert_eq!(
            h.app
                .shell
                .geometry()
                .stack
                .last()
                .copied()
                .unwrap_or(NavPane::Overview),
            NavPane::Overview
        );

        assert!(
            h.press_named(NamedKey::Enter),
            "Enter opens the selected contact"
        );
        assert_eq!(
            h.app
                .shell
                .geometry()
                .stack
                .last()
                .copied()
                .unwrap_or(NavPane::Overview),
            NavPane::Content
        );
    }

    // --- phase: resizing_wide_to_narrow_reflows_the_same_instance ---
    {
        // Guards against the "responsive on first launch, frozen after resize"
        // bug: the breakpoint must be re-derived from the CURRENT width on
        // every layout pass, not cached from the first frame.
        // Lay the SAME instance out wide, then narrow, then wide again.
        let mut app = Live::sample(1280, 800);
        assert!(
            app.shell.geometry().side_by_side,
            "wide viewport is the docked split"
        );
        assert!(
            app.shell.geometry().overview_column.x1
                <= app.shell.geometry().content_column.x0 + 0.001,
            "list sits left of detail"
        );

        rerender_at(&mut app, 560, 720);
        assert!(
            (!app.shell.geometry().side_by_side),
            "shrinking below the breakpoint reflows to single-pane push"
        );

        rerender_at(&mut app, 1280, 800);
        assert!(
            app.shell.geometry().side_by_side,
            "growing back above the breakpoint restores the docked split"
        );
    }
}

/// 200% text reflows and scales interaction geometry and full names fit the detail row.
#[test]
fn text_scale_reflows_geometry_and_names_fit() {
    let mut app = Live::sample(1440, 900);
    app.request(Request::EditSelected);
    let first_1x = form_widget(&app, Slot::First).unwrap().get_rect();
    let row_1x = app.shared.borrow().painted[0].1.height();
    let theme = Theme::dark().with_text_scale(2.0);
    app.harness_mut()
        .relayout_themed((1440, 900), theme.clone())
        .unwrap();
    assert!(
        !app.shell.geometry().side_by_side,
        "large text reflows the panes"
    );
    let first = form_widget(&app, Slot::First).unwrap().get_rect();
    let last = form_widget(&app, Slot::Last).unwrap().get_rect();
    let notes = form_widget(&app, Slot::Notes).unwrap();
    assert!(
        first.height() > first_1x.height(),
        "shared fields grow with text"
    );
    assert!(last.y0 > first.y1, "paired fields stack at large text");
    assert!(
        notes
            .as_any()
            .and_then(|value| value.downcast_ref::<ui::widget::TextInput>())
            .unwrap()
            .is_multiline()
    );
    assert!(
        notes.get_rect().height() >= first.height() * 3.0,
        "notes reserve multiple large text lines"
    );
    app.request(Request::Cancel);
    app.shell.show(NavPane::Overview);
    app.shell.snap();
    app.recompose();
    assert!(
        app.shared.borrow().painted[0].1.height() > row_1x,
        "shared list rows grow with text"
    );

    let mut h = Harness::new(800, 600);
    let mut tr = TextRenderer::new();
    let ids: Vec<_> = h
        .app
        .store
        .contacts
        .iter()
        .map(|contact| contact.id)
        .collect();
    assert!(h.app.shell.geometry().side_by_side);
    for id in ids {
        h.app.commit_select(id);
        h.relayout();
        let name = h.app.store.get(id).unwrap().display_name();
        let heading = named_widget(&*h.app, &name, Role::Heading { level: 1 })
            .expect("the real contact heading");
        let measured = f64::from(
            tr.measure_weighted(
                &name,
                Theme::dark().text(ui::TextRole::Title).size,
                None,
                ui::text_render::TextWeight::Semibold,
            )
            .0,
        );
        assert!(
            heading.get_rect().width() >= measured,
            "the full name {name} fits its actual heading"
        );
    }
}

/// Saves round-trip through the document; refused saves, favourites and deletes reconcile and announce errors without claiming success.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn saves_persist_and_refusals_never_claim_success() {
    fn set_title(app: &mut ContactsApp, title: &str) {
        app.form
            .as_ref()
            .expect("the editor exposes a title input")
            .set_value(Slot::Title, title);
    }

    // --- phase: save_round_trips_through_doc ---
    {
        let mut app = Live::sample(1280, 800);
        let c = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        let id = c.id;
        app.begin_edit(c, false);
        set_title(&mut app, "Edited Title");
        app.save_edit();
        app.settle();
        assert_eq!(app.mode, Mode::View);
        let stored = app.book();
        let r = stored.iter().find(|x| x.id == id).unwrap();
        assert_eq!(r.title, "Edited Title");
    }

    // --- phase: refused_save_reconciles_and_announces_an_error_without_claiming_success ---
    {
        let mut app = Live::sample(1280, 800);
        let contact = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        app.begin_edit(contact.clone(), false);
        set_title(&mut app, "Unsaved draft");
        // A collaborator deletes the record while it is being edited.
        app.app
            .merge_peer_edit(|doc| ContactsDoc::delete_contact(doc, contact.id));
        app.settle();
        app.save_edit();
        app.settle();

        assert_eq!(app.mode, Mode::Edit, "the rejected draft remains editable");
        assert_eq!(
            app.form.as_ref().map(|form| form.draft.title.as_str()),
            Some("Unsaved draft"),
            "the person's text is kept"
        );
        assert!(
            app.store.get(contact.id).is_none(),
            "the store follows the document"
        );
        let message = app
            .toast
            .as_ref()
            .map(|(message, _)| message.clone())
            .unwrap();
        assert!(message.starts_with("Couldn't save contact:"), "{message}");
        let tree = app.accessibility().unwrap();
        assert!(
            live_message(&tree, &message).is_some(),
            "the refusal is announced through the retained live region"
        );
    }

    // --- phase: refused_favorite_and_delete_reconcile_without_false_success ---
    {
        // A peer deletes the record after this replica rendered its detail
        // controls. Both mutations go to the door, which refuses them; the
        // stale projection is taken back and the refusal announced.
        let mut app = Live::sample(1280, 800);
        let contact = app
            .selected
            .and_then(|id| app.store.get(id))
            .cloned()
            .unwrap();
        app.app
            .merge_peer_edit(|doc| ContactsDoc::delete_contact(doc, contact.id));
        app.settle();
        app.store.upsert(contact.clone());
        app.selected = Some(contact.id);

        assert!(app.toggle_favorite(contact.id), "the toggle is submitted");
        app.settle();
        assert!(
            app.store.get(contact.id).is_none(),
            "favorite refusal reconciles"
        );
        let favorite_error = app.toast.as_ref().unwrap().0.clone();
        assert!(
            favorite_error.starts_with("Couldn't change the address book:"),
            "{favorite_error}"
        );
        assert!(
            live_message(&app.accessibility().unwrap(), &favorite_error).is_some(),
            "the favorite refusal is announced"
        );

        // Recreate the stale projection to exercise the independently visible
        // delete-confirmation path against the same tombstone.
        app.store.upsert(contact.clone());
        app.selected = Some(contact.id);
        app.delete_selected(contact.id);
        app.settle();
        assert!(
            app.store.get(contact.id).is_none(),
            "delete refusal reconciles"
        );
        let delete_error = app.toast.as_ref().unwrap().0.clone();
        assert!(delete_error.starts_with("Couldn't change the address book:"));
        assert_ne!(delete_error, "Contact deleted", "no false success toast");
        assert!(
            live_message(&app.accessibility().unwrap(), &delete_error).is_some(),
            "the delete refusal is announced"
        );
    }

    // --- phase: a_participant_who_may_only_view_cannot_save ---
    {
        let mut app = Live::mount(
            ui::framework::testing::TestApp::with_role(ui::Role::Viewer).unwrap(),
            (1280, 800),
        );
        let draft = Contact::new();
        app.begin_edit(draft, true);
        set_title(&mut app, "Not mine to add");
        app.save_edit();
        app.settle();
        assert_eq!(app.mode, Mode::Edit, "the draft stays open");
        assert!(app.book().is_empty(), "nothing reached the document");
        let message = app.toast.as_ref().unwrap().0.clone();
        assert!(message.starts_with("Couldn't save contact:"), "{message}");
    }
}

// =================================================================
// Interaction harness — drives real winit events through the
// production `handle_event` path (not test-only shortcuts), so the
// assertions prove the shipped behaviour.
// =================================================================

use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr};

/// Build a `KeyboardInput` winit event. winit 0.30's `KeyEvent` carries a
/// `pub(crate)` `platform_specific` field, so it can't be struct-literalled
/// — the whole struct is zero-initialised (the same trick `shell`'s and
/// `terminal`'s test harnesses use) then the public fields we care about are
/// overwritten. `text` is what a focused `TextInput` actually inserts.
fn key_input_event(logical_key: Key, text: Option<&str>) -> WindowEvent {
    // SAFETY: every public `KeyEvent` field tolerates the all-zero bit
    // pattern (`Option` → `None`; `Key`/`PhysicalKey`/`KeyLocation`/
    // `ElementState` first variant; `bool` → false) and we overwrite the
    // ones we read below; the private field is inert for our widgets.
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = logical_key;
    ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev.text = text.map(SmolStr::new);
    WindowEvent::KeyboardInput {
        device_id: device_id(),
        event: ev,
        is_synthetic: false,
    }
}

fn device_id() -> DeviceId {
    // winit gives no public DeviceId constructor; the production code
    // does exactly this for synthesised presses (see
    // `forward_press_to_focused_input`).
    unsafe { std::mem::zeroed() }
}

/// Mounts the app in the production frame host so tests can pump events
/// through the production `Widget::handle_event`.
struct Harness {
    app: Live,
}

impl Harness {
    fn new(w: u32, h: u32) -> Self {
        Harness {
            app: Live::sample(w, h),
        }
    }

    /// The sample book in a window someone uses, where effects and jobs run.
    fn live(w: u32, h: u32) -> Self {
        Harness {
            app: Live::live_sample(w, h),
        }
    }

    /// Rebuild, lay out and draw a whole frame (after the model changed). The
    /// draw populates draw-time hit rects (e.g. `detail_field_hits` for
    /// double-click-to-edit), exactly as the live renderer does each frame.
    fn relayout(&mut self) {
        self.app.recompose();
    }

    /// The address book as the document holds it.
    fn book(&mut self) -> Vec<Contact> {
        self.app.book()
    }

    fn dispatch(&mut self, event: &WindowEvent) -> bool {
        self.app.dispatch(event, ModifiersState::empty())
    }

    fn move_to(&mut self, p: Point) {
        self.dispatch(&WindowEvent::CursorMoved {
            device_id: device_id(),
            position: PhysicalPosition::new(p.x, p.y),
        });
    }

    fn press_left(&mut self, p: Point) -> bool {
        self.move_to(p);
        self.dispatch(&WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        })
    }

    fn press_right(&mut self, p: Point) -> bool {
        self.move_to(p);
        self.dispatch(&WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Right,
        })
    }

    /// A double click: two left presses at the same point in quick
    /// succession (the production double-click window is 400ms).
    fn double_click(&mut self, p: Point) {
        self.press_left(p);
        self.press_left(p);
    }

    /// First visible list row's center point.
    fn row_center(&self, index: usize) -> Point {
        self.app.shared.borrow().painted[index].1.center()
    }

    fn row_id(&self, index: usize) -> ContactId {
        self.app.shared.borrow().painted[index].0
    }

    /// Dispatch a key event with explicit modifier state through the real
    /// `handle_event` path.
    fn dispatch_key_mods(&mut self, event: &WindowEvent, mods: ModifiersState) -> bool {
        self.app.dispatch(event, mods)
    }

    /// Press a single `Named` key (no modifiers).
    fn press_named(&mut self, key: NamedKey) -> bool {
        self.dispatch_key_mods(
            &key_input_event(Key::Named(key), None),
            ModifiersState::empty(),
        )
    }

    /// Press a named key with Shift held (the standard Shift+F10 context-menu
    /// equivalent on keyboards without a dedicated Menu key).
    fn press_named_shift(&mut self, key: NamedKey) -> bool {
        self.dispatch_key_mods(
            &key_input_event(Key::Named(key), None),
            ModifiersState::SHIFT,
        )
    }

    /// Press a single character key (no modifiers) — a global shortcut like
    /// `n`, `e`, `/` when no field is focused.
    fn press_char(&mut self, c: &str) -> bool {
        self.dispatch_key_mods(
            &key_input_event(Key::Character(SmolStr::new(c)), Some(c)),
            ModifiersState::empty(),
        )
    }

    /// Press a character key with Ctrl held (e.g. Ctrl+S, Ctrl+K).
    fn press_char_ctrl(&mut self, c: &str) -> bool {
        self.dispatch_key_mods(
            &key_input_event(Key::Character(SmolStr::new(c)), Some(c)),
            ModifiersState::CONTROL,
        )
    }

    /// Press Shift+Tab (previous-field navigation) through the real path.
    fn press_shift_tab(&mut self) -> bool {
        self.dispatch_key_mods(
            &key_input_event(Key::Named(NamedKey::Tab), None),
            ModifiersState::SHIFT,
        )
    }

    /// The `Slot` of the currently-focused edit-form field, if any.
    fn focused_slot(&self) -> Option<Slot> {
        self.app.form.as_ref().and_then(|form| {
            form_slots(form)
                .into_iter()
                .find(|slot| form_widget(&self.app, *slot).is_some_and(Widget::is_focused))
        })
    }

    /// Type a string one keystroke at a time into whichever input is
    /// focused, driving the production `handle_event` → `TextInput` path
    /// (NOT `set_text`), so the live edit semantics are what's exercised.
    fn type_text(&mut self, s: &str) {
        for ch in s.chars() {
            let buf = ch.to_string();
            self.dispatch_key_mods(
                &key_input_event(Key::Character(SmolStr::new(&buf)), Some(&buf)),
                ModifiersState::empty(),
            );
        }
    }

    /// Value currently held in the focused/edit form field for `slot`.
    fn field_value(&self, slot: Slot) -> String {
        self.app
            .form
            .as_ref()
            .map_or_else(String::new, |form| form.value(slot))
    }

    /// The stored (committed) contact for `id`, read back from the CRDT doc.
    fn doc_contact(&mut self, id: ContactId) -> Option<Contact> {
        self.book().into_iter().find(|c| c.id == id)
    }

    /// Focus the edit-form field for `slot` by clicking its laid-out rect
    /// through the production press path. Returns whether it was found.
    fn focus_field(&mut self, slot: Slot) -> bool {
        let Some(rect) = form_widget(&self.app, slot).map(Widget::get_rect) else {
            return false;
        };
        if rect.width() <= 0.0 {
            return false;
        }
        self.press_left(rect.center());
        form_widget(&self.app, slot).is_some_and(Widget::is_focused)
    }
}

/// Detail fields enter edit on double click only, focus scrolls below-the-fold fields into view, and every slot has a placeholder.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn detail_fields_enter_edit_on_double_click_with_placeholders() {
    // The shared property's Edit action replaces the old private double-click
    // hit rectangle; it reaches the same field-focused, cancellable editor.
    let mut h = Harness::new(1280, 800);
    assert_eq!(h.app.mode, Mode::View);
    let birthday = named_widget(&*h.app, "Birthday", Role::Label).map(Widget::get_rect);
    let edit = h
        .app
        .harness_mut()
        .a11y_node_named("Edit birthday")
        .expect("shared birthday Edit action");
    h.app
        .harness_mut()
        .perform_a11y_action(edit, &ui::a11y::Action::Activate)
        .unwrap();
    h.app.settle();
    assert_eq!(h.app.mode, Mode::Edit);
    assert_eq!(
        h.focused_slot(),
        Some(Slot::Birthday),
        "the chosen property focuses its field"
    );
    h.app.request(Request::Cancel);
    assert_eq!(h.app.mode, Mode::View);
    if let Some(rect) = birthday {
        h.press_left(rect.center());
        assert_eq!(
            h.app.mode,
            Mode::View,
            "pressing a property label does not edit"
        );
    }

    let mut h = Harness::new(1000, 420);
    let mut contact = Contact::new();
    contact.first = "Long".into();
    contact.last = "Form".into();
    for _ in 0..4 {
        contact.phones.push(Labeled::new("mobile", ""));
        contact.emails.push(Labeled::new("home", ""));
    }
    h.app.begin_edit(contact, true);
    h.relayout();
    let groups_before = form_widget(&h.app, Slot::Groups).unwrap().get_rect();
    assert!(
        groups_before.y1 > h.app.shell.geometry().content_column.y1,
        "the long form extends below the fold"
    );
    // Focus the real retained last input via the exported accessibility door.
    let groups = h
        .app
        .harness_mut()
        .a11y_node_named("Groups")
        .expect("the real Groups input is addressable");
    h.app
        .harness_mut()
        .perform_a11y_action(groups, &ui::a11y::Action::Focus)
        .unwrap();
    h.app.settle();
    assert_eq!(h.focused_slot(), Some(Slot::Groups));
    assert!(
        form_widget(&h.app, Slot::Groups).unwrap().get_rect().y1
            <= h.app.shell.geometry().content_column.y1 + 0.5
    );
    assert!(
        h.app.form.as_ref().unwrap().scroll.get().1 > 0.0,
        "focus reveals the off-screen field"
    );
    let first = h.app.harness_mut().a11y_node_named("First name").unwrap();
    h.app
        .harness_mut()
        .perform_a11y_action(first, &ui::a11y::Action::Focus)
        .unwrap();
    h.app.settle();
    assert_eq!(h.focused_slot(), Some(Slot::First));
    assert!(
        form_widget(&h.app, Slot::First).unwrap().get_rect().y0
            >= h.app.shell.geometry().content_column.y0
    );

    for slot in form_slots(h.app.form.as_ref().unwrap()) {
        let input = form_widget(&h.app, slot).expect("every editable slot is a shared TextInput");
        assert_eq!(
            input.accessibility().unwrap().name.as_deref(),
            Some(crate::form::field_name(slot).as_str()),
            "every field has an explicit label"
        );
    }
}

/// Build an edit-mode harness on a known contact so the field order under
/// Tab is deterministic: First, Last, Org, Title, Phone(0), Email(0),
/// Birthday, Website, Notes, Groups.
fn edit_harness() -> Harness {
    let mut h = Harness::new(1280, 800);
    let mut c = Contact::new();
    c.first = "Ada".into();
    c.last = "Byron".into();
    c.phones.push(Labeled::new("mobile", "+1 555 0100"));
    c.emails.push(Labeled::new("home", "ada@example.com"));
    h.app.begin_edit(c, true);
    h.relayout();
    h
}

/// Tab and Shift-Tab move and wrap focus, Enter advances then submits, and tabbing places the caret at the end.
#[test]
fn form_focus_moves_by_tab_shift_tab_and_enter() {
    // --- phase: tab_moves_focus_forward_across_the_form_fields ---
    {
        let mut h = edit_harness();
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(h.focused_slot().is_none(), "no field is focused until Tab");
        // Tab lands on the first field, then walks the natural order.
        assert!(
            h.press_named(NamedKey::Tab),
            "Tab is handled in the edit form"
        );
        assert_eq!(h.focused_slot(), Some(Slot::First));
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Last));
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Org));
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Title));
        h.press_named(NamedKey::Tab);
        assert_eq!(
            h.focused_slot(),
            Some(Slot::Phone(0)),
            "repeated phone rows are in the order"
        );
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Email(0)));
    }

    // --- phase: shift_tab_moves_focus_backward_and_wraps ---
    {
        let mut h = edit_harness();
        // Forward twice → Last is focused.
        h.press_named(NamedKey::Tab);
        h.press_named(NamedKey::Tab);
        assert_eq!(h.focused_slot(), Some(Slot::Last));
        // Shift+Tab → back to First.
        assert!(h.press_shift_tab(), "Shift+Tab is handled");
        assert_eq!(h.focused_slot(), Some(Slot::First));
        // Shift+Tab from the first field wraps to the last (Groups).
        h.press_shift_tab();
        assert_eq!(
            h.focused_slot(),
            Some(Slot::Groups),
            "backward from the first wraps to the last"
        );
        // Tab from the last field wraps forward to the first.
        h.press_named(NamedKey::Tab);
        assert_eq!(
            h.focused_slot(),
            Some(Slot::First),
            "forward from the last wraps to the first"
        );
    }

    // --- phase: enter_advances_to_the_next_field_then_submits_on_the_last ---
    {
        let mut h = edit_harness();
        // Enter with nothing focused focuses the first field.
        assert!(
            h.press_named(NamedKey::Enter),
            "Enter is handled in the edit form"
        );
        assert_eq!(h.focused_slot(), Some(Slot::First));
        // Enter advances to the next field.
        h.press_named(NamedKey::Enter);
        assert_eq!(h.focused_slot(), Some(Slot::Last));
        // Walk back to the last field: Last → First → (wrap) → Groups. Then
        // Enter on the last field submits (saves) instead of wrapping forward.
        h.press_shift_tab(); // Last → First
        assert_eq!(h.focused_slot(), Some(Slot::First));
        h.press_shift_tab(); // First → (wrap) → Groups
        assert_eq!(h.focused_slot(), Some(Slot::Groups));
        h.press_named(NamedKey::Enter);
        assert_eq!(
            h.app.mode,
            Mode::View,
            "Enter on the last field submits the form"
        );
        assert!(h.app.form.is_none(), "the edit form closed on submit");
        assert!(
            h.app
                .store
                .contacts
                .iter()
                .any(|c| c.first == "Ada" && c.last == "Byron"),
            "the edited contact was saved to the store",
        );
    }

    // --- phase: tab_into_a_field_places_the_caret_at_the_end ---
    {
        // Tabbing into a pre-filled field puts the caret after its text so the
        // user can keep typing rather than overwriting.
        let mut h = edit_harness();
        h.press_named(NamedKey::Tab); // First ("Ada")
        let end = h.app.form.as_ref().unwrap().value(Slot::First).len();
        assert_eq!(end, "Ada".len());
        h.type_text("!");
        assert_eq!(
            h.field_value(Slot::First),
            "Ada!",
            "typing appends at the caret"
        );
    }
}

/// The website field renders and opens through the shared link widget.
#[test]
fn website_renders_and_opens_through_the_link_widget() {
    // --- phase: website_renders_via_the_link_widget ---
    {
        // The view-mode website is a real ui `Link` widget (not hand-drawn
        // text): the app holds a Link bound to the selected contact's URL, laid
        // out with a hit rect so it's clickable.
        let h = Harness::new(1280, 800);
        let id = h.app.selected.expect("a contact is selected");
        let website = h.app.store.get(id).unwrap().website.clone();
        assert!(!website.is_empty(), "the selected seed has a website");
        let link = h
            .app
            .website_link
            .as_ref()
            .expect("the website is rendered via a Link widget");
        assert_eq!(link.url(), website, "the link opens the contact's website");
        assert!(
            link.get_rect().width() > 0.0,
            "the link is laid out with a hit rect"
        );
    }

    // --- phase: clicking_the_website_opens_it_through_the_link_widget ---
    {
        // Drive the whole click path (press → hit-test → Link::activate) and
        // confirm the contact's URL is what gets opened. A recording host
        // captures the URL instead of spawning the OS browser, so this exercises
        // the production click path hermetically — no xdg-open side effect.
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        // The link opens through background work; pump it so the open is seen.
        let work = ui::background::Pumped::new();
        let _work = work.background().enter();
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let url = h.app.store.get(id).unwrap().website.clone();
        assert!(!url.is_empty(), "the selected seed has a website");

        h.relayout();
        let rect = h.app.website_link.as_ref().unwrap().get_rect();
        assert!(
            rect.width() > 0.0,
            "the website link has a hit rect after layout"
        );

        assert!(
            h.press_left(rect.center()),
            "clicking the website link is handled"
        );
        work.run_pending();
        assert_eq!(
            recorder.opened_uris(),
            [url],
            "the click opened the contact's website via the Link widget",
        );
        assert_eq!(
            h.app.mode,
            Mode::View,
            "opening a link does not enter edit mode"
        );
    }
}

/// Pointer and keyboard context menus open the selected contacts' canonical actions; favourite toggles, persists and relabels.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn context_menu_opens_canonical_contact_actions() {
    // --- phase: right_click_row_opens_context_menu_then_delete_opens_confirm ---
    {
        let mut h = Harness::new(1280, 800);
        let row = h.row_center(0);
        let id = h.row_id(0);
        assert!(h.press_right(row));
        match &h.app.open_menu {
            Some(OpenMenu::Contact(target, _)) => assert_eq!(*target, id),
            other => panic!(
                "right-click should open the context menu, got {:?}",
                other.is_some()
            ),
        }
        // Click the "Delete" row in the menu → the delete-confirm modal opens.
        let rows = menu_rows(&h.app);
        let delete_idx = menu_index(&h.app, ContextAction::Delete);
        h.press_left(rows[delete_idx].center());
        assert_eq!(
            h.app.confirm_delete,
            Some(id),
            "context-menu Delete routes through the confirmation modal"
        );
        assert!(
            !h.app.menu.is_open(),
            "the context menu closes when an item is chosen"
        );
    }

    // --- phase: keyboard_context_menu_keys_open_selected_contacts_canonical_actions ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.expect("a contact starts selected");

        assert!(
            h.press_named(NamedKey::ContextMenu),
            "the dedicated Menu key opens the selected contact's actions"
        );
        match &h.app.open_menu {
            Some(OpenMenu::Contact(target, _)) => assert_eq!(*target, id),
            _ => panic!("Menu key should open the selected contact's context menu"),
        }

        h.press_named(NamedKey::Escape);
        assert!(!h.app.menu.is_open(), "Escape dismisses the transient menu");
        assert!(
            h.press_named_shift(NamedKey::F10),
            "Shift+F10 opens the same context menu on keyboards without Menu"
        );
        match &h.app.open_menu {
            Some(OpenMenu::Contact(target, _)) => assert_eq!(*target, id),
            _ => panic!("Shift+F10 should open the selected contact's context menu"),
        }
    }

    // --- phase: context_menu_favorite_toggles_flag_persists_and_relabels ---
    // The context-menu Favorite item flips the contact's flag, persists it to
    // the CRDT, and its label reflects the (now-updated) state on reopen.
    {
        let mut h = Harness::new(1280, 800);
        let row = h.row_center(0);
        let id = h.row_id(0);

        // Read the CRDT-backed favorite state (not just the in-memory store).
        let fav_in_doc = |book: Vec<Contact>, id: ContactId| {
            book.into_iter()
                .find(|c| c.id == id)
                .map(|c| c.favorite)
                .expect("contact present in doc")
        };
        let before = h.app.store.get(id).unwrap().favorite;

        assert!(h.press_right(row));
        // The menu item index is stable; resolve it and confirm its label
        // matches the *current* (pre-toggle) favorite state.
        let fav_idx = menu_index(&h.app, ContextAction::Favorite);
        assert_eq!(
            h.app.menu.items()[fav_idx].label().unwrap(),
            if before {
                "Remove from favorites"
            } else {
                "Add to favorites"
            },
            "the Favorite item is labelled for the contact's current state",
        );

        // Click the Favorite item in the actual shared menu.
        let rows = menu_rows(&h.app);
        h.press_left(rows[fav_idx].center());

        // The flag flipped in the store AND in the CRDT, and the menu closed.
        assert_eq!(
            h.app.store.get(id).unwrap().favorite,
            !before,
            "store flag flipped"
        );
        assert_eq!(
            fav_in_doc(h.book(), id),
            !before,
            "CRDT favorite flag flipped"
        );
        assert!(
            !h.app.menu.is_open(),
            "the menu closes after toggling favorite"
        );

        // Reopening the menu now shows the inverted label.
        assert!(h.press_right(h.row_center(0)));
        assert_eq!(
            h.app.menu.items()[fav_idx].label().unwrap(),
            if before {
                "Add to favorites"
            } else {
                "Remove from favorites"
            },
            "the relabelled item reflects the toggled state",
        );

        // Toggling again restores the original state (round-trip).
        let rows = menu_rows(&h.app);
        h.press_left(rows[fav_idx].center());
        assert_eq!(
            fav_in_doc(h.book(), id),
            before,
            "second toggle restores the CRDT flag"
        );
    }
}

/// The delete key and confirm modal dispatch the palette's delete action; the shared modal's cancel is non-destructive.
#[test]
fn delete_confirmation_dispatches_the_palette_action() {
    // --- phase: confirm_modal_delete_removes_contact_from_doc ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.row_id(0);
        h.app.confirm_delete = Some(id);
        let before = h.book().len();
        h.relayout();
        let del = current_dialog(&*h.app).primary_rect();
        h.press_left(del.center());
        assert!(
            h.app.confirm_delete.is_none(),
            "the modal closes after confirming"
        );
        let after = h.book().len();
        assert_eq!(
            after,
            before - 1,
            "Delete removes the contact from the CRDT doc"
        );
        assert!(
            !h.book().iter().any(|c| c.id == id),
            "the deleted contact is gone from the document"
        );
    }

    // --- phase: confirm_delete_dispatches_the_action_the_palette_builds ---
    {
        // Single source of truth, proven on the real button path: confirming
        // a delete dispatches the byte-identical EncodedAction the Ctrl+K
        // "Delete contact" spec builds for the same contact.
        let mut h = Harness::new(1280, 800);
        let id = h.row_id(0);
        let expected = h
            .app
            .book_after(&crate::actions::delete_contact_action(id.to_string()));
        h.app.confirm_delete = Some(id);
        h.relayout();
        let del = current_dialog(&*h.app).primary_rect();
        h.press_left(del.center());
        assert_eq!(
            h.book(),
            expected,
            "the confirm-delete button does what the palette's Delete does",
        );
        assert!(
            !h.book().iter().any(|c| c.id == id),
            "and it actually deleted the contact",
        );
    }

    // --- phase: delete_key_opens_confirm_and_escape_dismisses_it ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        assert!(h.press_named(NamedKey::Delete));
        assert_eq!(
            h.app.confirm_delete,
            Some(id),
            "Delete raises the confirm modal for the selection"
        );
        h.press_named(NamedKey::Escape);
        assert!(
            h.app.confirm_delete.is_none(),
            "Escape dismisses the confirm modal"
        );
        assert!(h.app.store.get(id).is_some(), "nothing was deleted");
    }

    // --- phase: delete_confirmation_is_shared_modal_and_cancel_is_non_destructive ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.row_id(0);
        h.app.confirm_delete = Some(id);
        h.relayout();

        let dialog = current_dialog(&*h.app);
        let access = dialog.accessibility().expect("modal accessibility node");
        assert_eq!(access.role, ui::a11y::Role::Dialog);
        assert!(access.state.modal, "assistive tech sees a blocking dialog");
        assert!(h.app.traps_focus(), "keyboard focus cannot leave the modal");
        let cancel = dialog.secondary_rect();

        assert!(h.press_left(cancel.center()));
        assert!(h.app.confirm_delete.is_none(), "Cancel closes the dialog");
        assert!(h.app.store.get(id).is_some(), "Cancel never deletes");
        assert!(
            h.book().iter().any(|c| c.id == id),
            "Cancel leaves the document untouched"
        );
    }
}

/// Favourite toggles dispatch the palette action, palette actions reconcile on sync, the detail star toggles store and doc and a toast dismisses.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn favorite_toggles_dispatch_and_reconcile_with_feedback() {
    // --- phase: favorite_toggle_dispatches_the_action_the_palette_builds ---
    {
        // Same, on the favourite path: the context-menu Favorite item dispatches
        // the byte-identical EncodedAction the Ctrl+K "favorite" spec builds,
        // toggling to the negation of the current state.
        let mut h = Harness::new(1280, 800);
        let row = h.row_center(0);
        let id = h.row_id(0);
        let before = h.app.store.get(id).unwrap().favorite;
        let expected = h.app.book_after(&crate::actions::set_favorite_action(
            id.to_string(),
            !before,
        ));

        assert!(h.press_right(row));
        let rows = menu_rows(&h.app);
        let fav_idx = menu_index(&h.app, ContextAction::Favorite);
        h.press_left(rows[fav_idx].center());
        assert_eq!(
            h.book(),
            expected,
            "the favourite affordance does what the palette's favorite does",
        );
    }

    // --- phase: palette_action_reconciles_into_the_list_on_next_sync ---
    {
        // A Ctrl+K palette action mutates only the document; the next sync must
        // reconcile the in-memory store so the change shows in the list — the
        // same way an on-screen edit already does.
        let mut h = Harness::new(1280, 800);
        let id = h.row_id(0);
        let before = h.app.store.len();
        // Invoke the palette's delete through the door, bypassing the app's
        // own affordances (exactly as the shell's Ctrl+K palette would).
        h.app
            .app
            .call_on(
                &crate::actions::contact_resource_id(id),
                "delete",
                serde_json::json!({}),
            )
            .expect_err("deleting asks for confirmation");
        let action = crate::actions::delete_contact_action(id.to_string());
        h.app
            .app
            .invoke_confirmed(ui::tilo_resource::Invoke::apply(&action).unwrap())
            .unwrap();
        h.app.settle();
        assert!(
            h.app.store.get(id).is_none(),
            "the presented book dropped the deleted contact"
        );
        assert_eq!(h.app.store.len(), before - 1);
    }

    // --- phase: detail_favorite_star_click_toggles_store_and_doc ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let before = h.app.store.get(id).unwrap().favorite;
        assert!(
            target(&h.app, "contacts.favorite").width() > 0.0,
            "the detail favorite star is laid out"
        );
        h.press_left(target(&h.app, "contacts.favorite").center());
        assert_eq!(
            h.app.store.get(id).unwrap().favorite,
            !before,
            "the star flips the store flag"
        );
        assert_eq!(
            h.doc_contact(id).unwrap().favorite,
            !before,
            "and persists to the doc"
        );
    }

    // --- phase: toast_auto_dismisses_after_a_real_action ---
    {
        // After Save/Delete/Import/Export the confirmation toast used to stay
        // forever: it stored an Instant nothing ever read. Drive a real action
        // (delete) so the production `show_toast` fires, then prove the frame
        // loop keeps itself awake and clears the toast once its lifetime
        // elapses, and idles afterwards.
        let mut app = sampled();
        let id = app.selected.unwrap();

        app.delete_selected(id);
        assert!(app.toast.is_some(), "deleting a contact raises a toast");
        assert!(
            app.needs_animation_frame(),
            "the loop stays awake so the timed dismissal fires without further input",
        );

        // A frame drawn before the lifetime elapses keeps the toast.
        drive_frame(&mut app, 1280, 800);
        assert!(
            app.toast.is_some(),
            "a fresh toast survives an immediate frame"
        );

        // Back-date the toast past its lifetime and draw again → it clears,
        // and the loop is then free to idle.
        if let Some((_, shown)) = app.toast.as_mut() {
            *shown = ui::time::now()
                .checked_sub(TOAST_LIFETIME)
                .unwrap()
                .checked_sub(Duration::from_millis(50))
                .unwrap();
        }
        assert!(
            app.toast_expired(),
            "the back-dated toast is past its lifetime"
        );
        drive_frame(&mut app, 1280, 800);
        assert!(
            app.toast.is_none(),
            "the toast auto-dismisses once its lifetime elapses"
        );
        assert!(
            !app.needs_animation_frame(),
            "with the toast gone the loop can settle back to idle",
        );
    }
}

/// Switching away from a dirty edit pops the save modal (save commits, discard switches), clean edits switch directly, and keystroke dirtying reaches the guard.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn switching_while_editing_is_guarded_by_the_unsaved_modal() {
    // --- phase: switching_while_editing_dirty_pops_save_modal_then_save_commits ---
    {
        let mut h = Harness::new(1280, 800);
        // Enter edit on the selected contact and dirty a field.
        let cur = h.app.selected.unwrap();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        if let Some(form) = h.app.form.as_ref() {
            form.set_value(Slot::Title, "Brand New Title");
        }
        h.relayout();
        assert!(h.app.form_is_dirty(), "the edited form is dirty");
        // Click a different list row → the guard modal pops, no switch yet.
        let other_idx = (0..h.app.shared.borrow().painted.len())
            .find(|i| h.app.shared.borrow().painted[*i].0 != cur)
            .unwrap();
        let other_id = h.row_id(other_idx);
        let other_row = h.row_center(other_idx);
        h.press_left(other_row);
        assert_eq!(
            h.app.pending_switch,
            Some(other_id),
            "switch is deferred behind the modal"
        );
        assert_eq!(
            h.app.mode,
            Mode::Edit,
            "still editing while the modal is up"
        );
        // Press Save → commits the edit AND switches to the target.
        let save = current_dialog(&*h.app).primary_rect();
        h.press_left(save.center());
        assert!(h.app.pending_switch.is_none());
        assert_eq!(h.app.mode, Mode::View, "saved and left the form");
        assert_eq!(
            h.app.selected,
            Some(other_id),
            "switched to the clicked contact"
        );
        let stored = h.book();
        let edited = stored.iter().find(|x| x.id == cur).unwrap();
        assert_eq!(
            edited.title, "Brand New Title",
            "the edit was saved to the doc"
        );
    }

    // --- phase: switching_while_editing_discard_drops_edits_and_switches ---
    {
        let mut h = Harness::new(1280, 800);
        let cur = h.app.selected.unwrap();
        let original_title = h.app.store.get(cur).unwrap().title.clone();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        if let Some(form) = h.app.form.as_ref() {
            form.set_value(Slot::Title, "Throwaway Title");
        }
        h.relayout();
        let other_idx = (0..h.app.shared.borrow().painted.len())
            .find(|i| h.app.shared.borrow().painted[*i].0 != cur)
            .unwrap();
        let other_id = h.row_id(other_idx);
        let other_row = h.row_center(other_idx);
        h.press_left(other_row);
        assert_eq!(h.app.pending_switch, Some(other_id));
        // Press Discard → drop edits, switch.
        let discard = current_dialog(&*h.app).secondary_rect();
        h.press_left(discard.center());
        assert_eq!(h.app.selected, Some(other_id));
        let stored = h.book();
        let unchanged = stored.iter().find(|x| x.id == cur).unwrap();
        assert_eq!(
            unchanged.title, original_title,
            "discard left the doc unchanged"
        );
    }

    // --- phase: switching_with_no_changes_switches_directly_no_modal ---
    {
        let mut h = Harness::new(1280, 800);
        let cur = h.app.selected.unwrap();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(!h.app.form_is_dirty(), "no edits ⇒ not dirty");
        let other_idx = (0..h.app.shared.borrow().painted.len())
            .find(|i| h.app.shared.borrow().painted[*i].0 != cur)
            .unwrap();
        let other_id = h.row_id(other_idx);
        let other_row = h.row_center(other_idx);
        h.press_left(other_row);
        assert!(
            h.app.pending_switch.is_none(),
            "no modal when there are no unsaved changes"
        );
        assert_eq!(h.app.selected, Some(other_id), "switched directly");
        assert_eq!(h.app.mode, Mode::View);
    }

    // --- phase: dirtying_via_keystrokes_makes_the_unsaved_guard_reachable ---
    {
        // Proves the guard on the REAL keystroke path (not `set_text`): typing
        // leaves the store untouched, so `form_is_dirty` sees the divergence
        // and switching contacts pops the modal instead of losing the edit.
        let mut h = Harness::new(1280, 800);
        let cur = h.app.selected.unwrap();
        let c = h.app.store.get(cur).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(h.focus_field(Slot::Title));
        h.type_text("Q");
        assert!(
            h.app.form_is_dirty(),
            "keystroke-dirtied form is dirty via the production path"
        );
        let other_idx = (0..h.app.shared.borrow().painted.len())
            .find(|i| h.app.shared.borrow().painted[*i].0 != cur)
            .unwrap();
        let other_id = h.row_id(other_idx);
        h.press_left(h.row_center(other_idx));
        assert_eq!(
            h.app.pending_switch,
            Some(other_id),
            "the guard defers the switch"
        );
        assert_eq!(h.app.mode, Mode::Edit, "still editing behind the modal");
    }
}

// =================================================================
// Live-edit draft semantics — the structural fix. Editing mutates only
// the form's local buffers/draft; nothing reaches the store or the CRDT
// doc until Save. This is what makes Cancel real and the unsaved guard
// reachable, so these drive the *real* keystroke path (handle_event),
// never `set_text`.
// =================================================================

/// Typing does not commit, cancel discards (no phantom new record), the edit affordance opens and returns, and Ctrl-S commits through the keystroke path.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn edits_commit_only_on_save_and_cancel_discards() {
    // --- phase: typing_in_edit_form_does_not_commit_and_cancel_discards ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let original = h.app.store.get(id).unwrap().title.clone();
        let c = h.app.store.get(id).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        // Focus the Title field via a real click, then type through the
        // production keystroke path (NOT `set_text`).
        assert!(
            h.focus_field(Slot::Title),
            "the title field focuses on click"
        );
        h.type_text("XYZ");
        // The keystrokes landed in the field's local buffer …
        let live = h.field_value(Slot::Title);
        assert!(
            live.contains("XYZ"),
            "the field shows the typed text: {live:?}"
        );
        assert_ne!(live, original);
        // … but NOTHING was committed: editing is a true draft now.
        assert_eq!(
            h.app.store.get(id).unwrap().title,
            original,
            "store untouched mid-edit"
        );
        assert_eq!(
            h.doc_contact(id).unwrap().title,
            original,
            "CRDT doc untouched mid-edit"
        );
        assert!(h.app.form_is_dirty(), "the live-typed form reports dirty");
        // Cancel genuinely discards the draft.
        assert!(
            target(&h.app, "contacts.form.cancel").width() > 0.0,
            "the Cancel button is laid out"
        );
        h.press_left(target(&h.app, "contacts.form.cancel").center());
        assert_eq!(h.app.mode, Mode::View, "Cancel leaves edit mode");
        assert!(h.app.form.is_none(), "Cancel drops the form");
        assert_eq!(
            h.app.store.get(id).unwrap().title,
            original,
            "Cancel discarded the edit (store)"
        );
        assert_eq!(
            h.doc_contact(id).unwrap().title,
            original,
            "Cancel discarded the edit (doc)"
        );
    }

    // --- phase: cancel_on_new_contact_leaves_no_phantom_record ---
    {
        let mut h = Harness::new(1280, 800);
        let store_before = h.app.store.len();
        let doc_before = h.book().len();
        // Start a new contact via the keyboard shortcut, focus the name, type.
        assert!(h.press_char("n"), "'n' starts a new contact");
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(h.app.form.as_ref().unwrap().is_new);
        h.relayout();
        assert!(h.focus_field(Slot::First));
        h.type_text("Ghost");
        // The half-typed new contact must not exist anywhere yet.
        assert_eq!(
            h.app.store.len(),
            store_before,
            "a half-typed new contact isn't in the store"
        );
        assert_eq!(h.book().len(), doc_before, "nor in the doc");
        // Cancel → still nothing, no "No Name"/"Ghost" phantom left behind.
        h.press_left(target(&h.app, "contacts.form.cancel").center());
        assert_eq!(h.app.mode, Mode::View);
        assert_eq!(
            h.app.store.len(),
            store_before,
            "Cancel on New leaves no phantom record"
        );
        assert!(
            !h.app
                .store
                .contacts
                .iter()
                .any(|c| c.display_name() == "Ghost"),
            "the abandoned draft never entered the book",
        );
    }

    // --- phase: edit_affordance_opens_form_on_selected_contact_and_cancel_returns ---
    // The read-view Edit affordance is a plain click that opens the edit form
    // on the *same* contact, and Cancel returns tidily to the read view — the
    // clean view→edit→view round trip, driven entirely through the production
    // click path (`handle_event`), not by poking `mode`/`begin_edit` directly.
    {
        let mut h = Harness::new(1280, 800);
        assert_eq!(h.app.mode, Mode::View, "the app opens in the read view");
        let sel = h
            .app
            .selected
            .expect("a contact is selected in the read view");
        assert!(
            target(&h.app, "contacts.edit").width() > 0.0,
            "the read-view Edit button is laid out"
        );

        // Clicking Edit enters the form on the shown contact (identity carried
        // across the transition — the header's avatar + name reflect this draft).
        h.press_left(target(&h.app, "contacts.edit").center());
        assert_eq!(h.app.mode, Mode::Edit, "clicking Edit opens the edit form");
        {
            let form = h.app.form.as_ref().expect("an edit form is open");
            assert_eq!(
                form.draft.id, sel,
                "the form edits the same contact the view showed"
            );
            assert!(!form.is_new, "editing an existing record, not creating one");
        }

        // Leaving is just as direct: Cancel returns to the read view, no form.
        h.relayout();
        assert!(
            target(&h.app, "contacts.form.cancel").width() > 0.0,
            "the Cancel button is laid out in the form"
        );
        h.press_left(target(&h.app, "contacts.form.cancel").center());
        assert_eq!(h.app.mode, Mode::View, "Cancel returns to the read view");
        assert!(h.app.form.is_none(), "no edit form lingers after Cancel");
        assert_eq!(h.app.selected, Some(sel), "the same contact stays selected");
    }

    // --- phase: typed_edit_saved_with_ctrl_s_commits_through_the_keystroke_path ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(h.focus_field(Slot::Org));
        h.type_text("Z");
        let typed = h.field_value(Slot::Org);
        assert!(typed.contains('Z'));
        // Ctrl+S saves; the typed value now round-trips into store + doc.
        assert!(h.press_char_ctrl("s"), "Ctrl+S is handled in edit mode");
        assert_eq!(h.app.mode, Mode::View, "Ctrl+S leaves the form");
        assert_eq!(
            h.app.store.get(id).unwrap().org,
            typed,
            "the typed org saved to the store"
        );
        assert_eq!(h.doc_contact(id).unwrap().org, typed, "and to the CRDT doc");
    }
}

/// Bare keys never discard an open form and a new form is not dirty until a field is filled.
#[test]
fn open_edit_forms_ignore_bare_keys_and_start_clean() {
    // --- phase: bare_keys_do_not_discard_an_open_edit_form ---
    {
        // Regression: with the edit form open but no field yet focused (its
        // initial state right after opening), a stray 'n' used to start a
        // brand-new contact — silently discarding the in-progress edit with no
        // unsaved guard — and '/' yanked focus to the list search. The form's
        // own keys own the keyboard until a field is focused.
        let mut h = edit_harness();
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(h.app.form.as_ref().unwrap().is_new);
        assert!(
            h.focused_slot().is_none(),
            "the form opens with no field focused"
        );
        let draft_id = h.app.form.as_ref().unwrap().draft.id;

        // 'n' must NOT start a new contact / swap the draft out from under us.
        h.press_char("n");
        assert_eq!(h.app.mode, Mode::Edit, "'n' does not leave the edit form");
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.id,
            draft_id,
            "'n' did not replace the in-progress draft",
        );

        // '/' must NOT steal focus to the list search while editing.
        h.press_char("/");
        assert!(
            !h.app.search.is_focused(),
            "'/' does not steal focus mid-edit"
        );
        assert_eq!(h.app.mode, Mode::Edit, "still editing");

        // Once a field IS focused, the very same key types into it normally.
        assert!(h.focus_field(Slot::First));
        h.type_text("n");
        assert!(
            h.field_value(Slot::First).contains('n'),
            "with a field focused the key types into it: {:?}",
            h.field_value(Slot::First),
        );
    }

    // --- phase: new_contact_form_is_not_dirty_until_a_field_is_filled ---
    {
        // A freshly-opened New Contact form (with the two empty seeded
        // phone/email rows) must not be considered dirty — otherwise the
        // unsaved guard would fire on the very first contact switch.
        let mut app = sampled();
        let doc = AutoCommit::new();
        app.sync(&doc, &ROOT);
        app.stage_new_contact();
        assert!(
            !app.form_is_dirty(),
            "an untouched New Contact form is clean"
        );
        if let Some(form) = app.form.as_mut() {
            form.draft.first = "Jane".into();
            if let Some(fi) = form.inputs.iter_mut().find(|f| f.slot == Slot::First) {
                fi.ti.set_text("Jane");
            }
        }
        assert!(app.form_is_dirty(), "typing a name makes the form dirty");
    }
}

// =================================================================
// List controls: search, sort, group filter, keyboard nav, scroll.
// =================================================================

/// Focus chords and Ctrl-K focus the search, typing narrows the list and an empty result offers a clear action.
#[test]
fn search_field_focus_and_typing_narrow_the_list() {
    let mut h = Harness::new(1280, 800);
    let versions = h.app.app.versions().len();
    let providers = ui::search::capability::search_providers(h.app.harness_mut().root_widget());
    assert_eq!(
        providers.len(),
        1,
        "the framework publishes one resource search source"
    );
    let hits = providers[0].search("hopper");
    let people: Vec<_> = hits
        .iter()
        .filter(|hit| hit.title == "Grace Hopper")
        .collect();
    assert_eq!(
        people.len(),
        1,
        "the canonical query finds the single Hopper record"
    );
    let grace = id_named(&h.app, "Grace Hopper");
    assert_eq!(
        people[0].action_address(),
        Some(crate::actions::open_contact_address(grace))
    );
    assert!(providers[0].search("definitely no contact").is_empty());
    let request = ui::search::SearchRequest::this_app().with_query("hopper");
    h.app
        .harness_mut()
        .open_search(&request)
        .expect("the actual platform search opens");
    let query = h
        .app
        .harness_mut()
        .focused_access_node()
        .expect("platform query input is focused");
    assert!(
        matches!(query.role, Role::TextInput),
        "the host owns the query input"
    );
    h.press_named(NamedKey::Escape);
    h.app
        .harness_mut()
        .open_search(&ui::search::SearchRequest::this_app())
        .expect("clearing returns to all resources");
    assert_eq!(
        h.app.app.versions().len(),
        versions,
        "search never edits the shared book"
    );
    h.press_named(NamedKey::Escape);
    assert_eq!(
        h.app.visible_ids().len(),
        7,
        "closing search restores the complete address book"
    );
}

/// The sort chip reorders, the group filter popup filters keeping selection valid, and the filter shot defers its anchor.
#[test]
fn sort_and_group_filters_reorder_and_filter_the_list() {
    // --- phase: sort_chip_opens_selector_then_reorders_on_choice ---
    {
        let mut h = Harness::new(1280, 800);
        assert_eq!(h.app.sort, SortKey::Name);
        let first_by_name = h
            .app
            .store
            .get(h.app.visible_ids()[0])
            .unwrap()
            .display_name();
        assert_eq!(
            first_by_name, "Tim Berners-Lee",
            "name sort leads with Berners-Lee"
        );
        assert!(
            h.press_left(target(&h.app, "contacts.sort").center()),
            "the sort chip is clickable"
        );
        let Some(Popup::Sort { anchor }) = h.app.popup else {
            panic!("the sort chip opens its explicit selector");
        };
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, 2, 180.0);
        h.press_left(rows[1].center());
        assert_eq!(h.app.sort, SortKey::Org, "choosing Org changes the sort");
        let first_by_org = h
            .app
            .store
            .get(h.app.visible_ids()[0])
            .unwrap()
            .display_name();
        assert_ne!(first_by_org, first_by_name, "the list reordered");
        assert_eq!(
            first_by_org, "Ada Lovelace",
            "org sort leads with Analytical Engines"
        );
        // Choosing Name returns to the name order.
        h.press_left(target(&h.app, "contacts.sort").center());
        let Some(Popup::Sort { anchor }) = h.app.popup else {
            panic!("the sort selector opens again");
        };
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, 2, 180.0);
        h.press_left(rows[0].center());
        assert_eq!(h.app.sort, SortKey::Name);
    }

    // --- phase: group_filter_popup_selection_filters_and_keeps_selection_valid ---
    {
        let mut h = Harness::new(1280, 800);
        // The seeded selection (Ada) is not in "Friends".
        let sel_before = h.app.selected.unwrap();
        assert!(
            !h.app
                .store
                .get(sel_before)
                .unwrap()
                .groups
                .iter()
                .any(|g| g.value.eq_ignore_ascii_case("Friends")),
        );
        // Open the group filter popup and choose "Friends".
        assert!(h.press_left(h.app.hit.group.center()));
        let Some(Popup::GroupFilter { anchor }) = h.app.popup else {
            panic!("the group chip opens the filter popup");
        };
        let opts = h.app.group_filter_options();
        let idx = opts
            .iter()
            .position(|o| o == "Friends")
            .expect("Friends is a seeded group");
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, opts.len(), 180.0);
        h.press_left(rows[idx].center());
        assert_eq!(
            h.app.group_filter.as_deref(),
            Some("Friends"),
            "the filter is applied"
        );
        let visible = h.app.visible_ids();
        assert!(!visible.is_empty());
        assert!(
            visible.iter().all(|id| h
                .app
                .store
                .get(*id)
                .unwrap()
                .groups
                .iter()
                .any(|g| g.value.eq_ignore_ascii_case("Friends"))),
            "every visible contact is in Friends",
        );
        // Selection stayed valid: it now points at a still-visible contact.
        assert!(
            visible.contains(&h.app.selected.unwrap()),
            "selection moved to a visible contact"
        );
    }

    // --- phase: group_filter_shot_defers_its_anchor_until_layout_exists ---
    {
        let mut app = sampled();
        let doc = AutoCommit::new();
        app.sync(&doc, &ROOT);
        app.stage_group_filter();
        drive_frame(&mut app, 1_280, 800);
        assert!(matches!(
            app.popup,
            Some(Popup::GroupFilter { anchor }) if anchor == app.hit.group && anchor != Rect::ZERO
        ));
    }
}

/// Arrow, n and e keys drive the list; the wheel scrolls it and large books virtualise while keeping selection in view.
#[test]
fn list_keyboard_and_scroll_navigation() {
    // --- phase: arrow_keys_move_the_list_selection ---
    {
        let mut h = Harness::new(1280, 800);
        let ids = h.app.visible_ids();
        let pos = ids
            .iter()
            .position(|x| *x == h.app.selected.unwrap())
            .unwrap();
        assert!(
            pos + 1 < ids.len(),
            "the seeded selection isn't the last row"
        );
        assert!(h.press_named(NamedKey::ArrowDown));
        assert_eq!(
            h.app.selected,
            Some(ids[pos + 1]),
            "ArrowDown selects the next contact"
        );
        assert!(h.press_named(NamedKey::ArrowUp));
        assert_eq!(
            h.app.selected,
            Some(ids[pos]),
            "ArrowUp returns to the previous"
        );
    }

    // --- phase: n_key_opens_new_and_e_key_edits_selection ---
    {
        let mut h = Harness::new(1280, 800);
        assert!(h.press_char("n"));
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(
            h.app.form.as_ref().unwrap().is_new,
            "'n' opens a NEW contact form"
        );
        // Escape out, then 'e' edits the existing selection.
        h.press_named(NamedKey::Escape);
        assert_eq!(h.app.mode, Mode::View);
        assert!(h.press_char("e"));
        assert_eq!(h.app.mode, Mode::Edit);
        assert!(
            !h.app.form.as_ref().unwrap().is_new,
            "'e' edits the selected contact"
        );
    }

    // --- phase: mouse_wheel_scrolls_the_overflowing_contact_list ---
    {
        // A short, wide window overflows the list so scrolling has an effect.
        let mut h = Harness::new(1000, 320);
        assert!(
            h.app.shell.geometry().side_by_side,
            "1000px stays the docked split"
        );
        assert_eq!(h.app.list_scroll, 0.0);
        h.move_to(h.app.shell.geometry().overview_column.center());
        h.dispatch(&WindowEvent::MouseWheel {
            device_id: device_id(),
            delta: MouseScrollDelta::LineDelta(0.0, -3.0),
            phase: winit::event::TouchPhase::Moved,
        });
        h.relayout();
        assert!(
            h.app.list_scroll > 0.0,
            "scrolling down moves the list; clamp keeps it in range"
        );
    }

    // --- phase: large_books_virtualize_rows_and_keep_keyboard_selection_in_view ---
    {
        // The CRDT/search model may legitimately hold an imported company
        // directory.  Layout and accessibility must scale with the viewport,
        // not with the directory's total cardinality.
        let mut h = Harness::new(900, 640);
        h.app.store.contacts = (0..1_000)
            .map(|index| Contact {
                id: uuid::Uuid::new_v4(),
                first: format!("Directory {index:04}"),
                ..Contact::default()
            })
            .collect();
        h.app.selected = h.app.store.contacts.first().map(|contact| contact.id);
        h.relayout();

        let viewport_capacity = (list_bounds(&h.app).height() / ROW_H).ceil() as usize;
        assert_eq!(h.app.shared.borrow().rows.len(), 1_000);
        assert!(
            h.app.shared.borrow().painted.len() <= viewport_capacity + LIST_OVERSCAN_ROWS * 2,
            "only viewport rows and bounded overscan are materialized"
        );

        h.app.move_selection(40);
        h.relayout();
        let selected = h.app.selected.expect("selection remains available");
        assert!(
            h.app
                .shared
                .borrow()
                .painted
                .iter()
                .any(|(id, _)| *id == selected),
            "keyboard selection scrolls itself into the virtualized viewport"
        );

        let tree = h.app.accessibility().expect("semantic tree is present");
        let list = tree
            .children
            .iter()
            .find(|node| node.role == Role::List)
            .expect("contacts list is exposed");
        assert_eq!(list.children.len(), h.app.shared.borrow().painted.len());
        assert!(
            list.children.len() < h.app.shared.borrow().rows.len(),
            "accessibility mirrors the bounded viewport rather than allocating every row"
        );
        let selected_row = list
            .children
            .iter()
            .find(|node| node.state.selected)
            .expect("selected row is in the semantic viewport");
        assert_eq!(selected_row.pos_in_set, Some(41));
        assert_eq!(selected_row.set_size, Some(1_000));
    }
}

/// The divider resizes the split and the compact toolbar fits with tooltips at phone width.
#[test]
fn split_divider_and_compact_toolbar_fit_their_space() {
    // --- phase: dragging_the_divider_resizes_the_split ---
    {
        let mut h = Harness::new(1280, 800);
        let before = h.app.split_pos;
        // Grab the divider (between list and detail).
        let grab = Point::new(
            h.app.shell.geometry().overview_column.x1 + DIVIDER_W / 2.0,
            h.app.shell.geometry().content_column.center().y,
        );
        h.move_to(grab);
        assert!(h.app.divider_hover, "cursor over the divider arms the drag");
        h.dispatch(&WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
        assert!(h.app.divider_drag, "pressing on the divider starts a drag");
        // Drag to ~55% of the width.
        h.move_to(Point::new(
            h.app.rect.x0 + h.app.rect.width() * 0.55,
            grab.y,
        ));
        assert!(
            h.app.split_pos > before,
            "dragging right widens the list pane"
        );
        assert!(
            (h.app.split_pos - 0.55).abs() < 0.02,
            "the split follows the cursor"
        );
        h.dispatch(&WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Released,
            button: MouseButton::Left,
        });
        assert!(!h.app.divider_drag, "releasing ends the drag");
    }

    // --- phase: compact_toolbar_has_room_and_tooltips_at_phone_width ---
    {
        let mut h = Harness::new(320, 640);
        assert!(h.app.toolbar_is_compact());
        assert!(h.app.hit.import.x0 >= h.app.rect.x0 + PAD - 0.01);
        assert!(h.app.hit.import.x1 < h.app.hit.export.x0);
        assert!(h.app.hit.export.x1 < target(&h.app, "contacts.new").x0);
        h.app.last_mouse = h.app.hit.import.center();
        assert_eq!(Widget::tooltip(&*h.app), Some("Import contacts"));
        h.app.last_mouse = target(&h.app, "contacts.new").center();
        assert_eq!(Widget::tooltip(&*h.app), Some("Create contact"));
    }
}

// =================================================================
// Edit-form controls: add/remove field, type chip, calendar, favorites.
// =================================================================

/// Phone rows grow and shrink, repeated row ids survive edits, and the favourite pill toggles only the draft.
#[test]
fn repeated_rows_grow_shrink_and_keep_ids_in_the_draft() {
    // --- phase: add_and_remove_phone_buttons_grow_and_shrink_the_draft ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        let base = c.phones.len();
        h.app.begin_edit(c, false);
        h.relayout();
        // Add-phone button appends a blank row.
        assert!(
            target(&h.app, "contacts.form.add.add-phone").width() > 0.0,
            "the add-phone button is laid out"
        );
        h.press_left(target(&h.app, "contacts.form.add.add-phone").center());
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.phones.len(),
            base + 1,
            "add-phone grew the draft"
        );
        h.relayout();
        // Remove-field button on the new phone drops it again.
        let rm = h
            .app
            .hit
            .remove_btns
            .iter()
            .find(|(t, _)| matches!(t, TypeTarget::Phone(i) if *i == base))
            .map(|(_, r)| *r)
            .expect("the new phone row has a remove button");
        h.press_left(rm.center());
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.phones.len(),
            base,
            "remove-field shrank the draft"
        );
    }

    // --- phase: form_collect_preserves_repeated_row_ids_across_value_and_group_order_edits ---
    {
        let contact = sample_contacts().into_iter().next().unwrap();
        let phone_id = contact.phones[0].id;
        let first_group_id = contact.groups[0].id;
        let second_group_id = contact.groups[1].id;
        let mut form = EditForm::new(contact, false);
        form.set_value(Slot::Phone(0), "+44 20 7946 0999");
        form.set_value(Slot::Groups, "Favorites, Work");
        form.collect();
        assert_eq!(form.draft.phones[0].id, phone_id);
        assert_eq!(form.draft.phones[0].value, "+44 20 7946 0999");
        assert_eq!(form.draft.groups[0].id, second_group_id);
        assert_eq!(form.draft.groups[1].id, first_group_id);
    }

    // --- phase: edit_form_favorite_pill_click_toggles_draft_only ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        let before = c.favorite;
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(
            h.app.hit.fav_toggle.width() > 0.0,
            "the favorite pill is laid out in the form"
        );
        h.press_left(h.app.hit.fav_toggle.center());
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.favorite,
            !before,
            "the pill flips the draft flag"
        );
        assert_eq!(
            h.app.store.get(id).unwrap().favorite,
            before,
            "draft-only: store unchanged until Save"
        );
    }
}

/// Type chip and calendar popups change only the draft and popup keyboard selection matches the pointer.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn draft_popups_set_type_and_birthday_only_in_the_draft() {
    // --- phase: type_chip_menu_selection_sets_label_in_draft_only ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        assert!(!c.phones.is_empty());
        let stored_label = c.phones[0].label.clone();
        h.app.begin_edit(c, false);
        h.relayout();
        // Click the type chip for the first phone.
        let chip = h
            .app
            .hit
            .type_chips
            .iter()
            .find(|(t, _)| matches!(t, TypeTarget::Phone(0)))
            .map(|(_, r)| *r)
            .expect("phone 0 has a type chip");
        h.press_left(chip.center());
        let Some(Popup::TypeMenu { anchor, .. }) = h.app.popup else {
            panic!("the type chip opens the type menu");
        };
        // Pick a label different from the current one.
        let pick = TYPE_OPTIONS
            .iter()
            .position(|l| *l != stored_label)
            .unwrap();
        let (_panel, rows) = h.app.popup_menu_geometry(anchor, TYPE_OPTIONS.len(), 140.0);
        h.press_left(rows[pick].center());
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.phones[0].label,
            TYPE_OPTIONS[pick],
            "the draft label updated",
        );
        // Draft-only: the committed store still holds the old label.
        assert_eq!(
            h.app.store.get(id).unwrap().phones[0].label,
            stored_label,
            "the label isn't committed until Save",
        );
    }

    // --- phase: calendar_day_click_sets_birthday_in_draft_only ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.get(id).cloned().unwrap();
        let stored_bday = c.birthday;
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(
            h.app.hit.cal_btn.width() > 0.0,
            "the calendar button is laid out"
        );
        h.press_left(h.app.hit.cal_btn.center());
        let Some(Popup::Calendar {
            year,
            month,
            anchor,
        }) = h.app.popup
        else {
            panic!("the calendar button opens the calendar");
        };
        let (panel, _prev, _next, cells) = h.app.calendar_geometry(anchor, year, month);
        // Pick an in-panel day different from the stored one (10).
        let (day, rect) = cells
            .iter()
            .copied()
            .find(|(d, r)| *d != 10 && panel.contains(r.center()))
            .expect("a selectable in-panel day");
        h.press_left(rect.center());
        let expect = format!("{year:04}-{month:02}-{day:02}");
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.birthday,
            Some(Birthday::Full { year, month, day }),
            "the picked day set the draft birthday as a structured date",
        );
        assert_eq!(
            h.field_value(Slot::Birthday),
            expect,
            "and mirrored into the birthday field"
        );
        // Draft-only: not committed to the store yet.
        assert_eq!(
            h.app.store.get(id).unwrap().birthday,
            stored_bday,
            "birthday not committed until Save"
        );
    }

    // --- phase: popup_keyboard_selects_the_same_type_choice_as_a_pointer ---
    {
        let mut h = Harness::new(1_280, 800);
        let id = h.app.selected.expect("sample contact selected");
        let contact = h.app.store.get(id).cloned().expect("stored contact");
        h.app.begin_edit(contact, false);
        h.relayout();
        let chip = h
            .app
            .hit
            .type_chips
            .iter()
            .find(|(target, _)| matches!(target, TypeTarget::Phone(0)))
            .map(|(_, rect)| *rect)
            .expect("phone type chip is visible");
        h.press_left(chip.center());
        assert!(matches!(h.app.popup, Some(Popup::TypeMenu { .. })));
        assert_eq!(h.app.popup_focus, 0, "first menu row receives focus");
        h.press_named(NamedKey::ArrowDown);
        h.press_named(NamedKey::Enter);
        assert!(!h.app.menu.is_open(), "activation dismisses the menu");
        assert_eq!(
            h.app.form.as_ref().expect("still editing").draft.phones[0].label,
            TYPE_OPTIONS[1],
            "keyboard activation uses the same draft update as a click"
        );
    }
}

/// A unique scratch `.vcf` destination for an export test — the path the
/// portal Save dialog would return, driven directly since the picker can't
/// run headless.
fn scratch_export() -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "contacts-export-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("scratch export directory");
    let vcf = dir.join("out.vcf");
    (dir, vcf)
}

/// Export parks the selection, writes the VCF to the portal path and round-trips every contact.
#[test]
#[serial_test::serial]
fn vcf_export_writes_the_selected_book() {
    // --- phase: export_selection_parks_then_save_portal_path_writes_the_vcf ---
    {
        let (dir, vcf) = scratch_export();
        let mut h = Harness::live(1280, 800);
        // Open the export dialog via the toolbar Export button.
        h.press_left(h.app.hit.export.center());
        assert!(
            h.app.export_dialog.is_open(),
            "Export opens the selection dialog"
        );
        // Pre-ticked with all visible contacts.
        let total = h.app.store.filtered_sorted("", None, h.app.sort).len();
        assert_eq!(
            h.app.export_dialog.selection().len(),
            total,
            "all pre-selected"
        );
        // Select none, then tick exactly one row.
        let g = h
            .app
            .export_dialog
            .geometry(h.app.rect, &h.app.store, h.app.sort);
        h.press_left(g.select_none.center());
        let g = h
            .app
            .export_dialog
            .geometry(h.app.rect, &h.app.store, h.app.sort);
        let (first_id, _row, cb) = g.rows[0];
        h.press_left(cb.center());
        assert_eq!(h.app.export_dialog.selection().len(), 1);
        assert!(h.app.export_dialog.selection().contains(&first_id));

        // Export 3.0 → closes the dialog and asks where to save. A recording
        // host stands in for the desktop and answers the Save dialog with
        // `vcf`.
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(vcf.clone()));
        let _host = ui::host::enter(&services);
        let g = h
            .app
            .export_dialog
            .geometry(h.app.rect, &h.app.store, h.app.sort);
        h.press_left(g.export_v3.center());
        assert!(
            !h.app.export_dialog.is_open(),
            "choosing a version closes the dialog"
        );
        assert!(h.app.portal_in_flight(), "the export runs");

        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 1, "exactly one Save dialog was requested");
        assert_eq!(picks[0].title(), "Export contacts");

        // The file is written off the UI thread; the export ends on the
        // frame after.
        let deadline = ui::time::now() + std::time::Duration::from_secs(30);
        while h.app.portal_in_flight() && ui::time::now() < deadline {
            h.app.settle();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!h.app.portal_in_flight(), "the export ended");
        let body = std::fs::read_to_string(&vcf).expect("export wrote a vcf to the chosen path");
        assert_eq!(
            body.matches("BEGIN:VCARD").count(),
            1,
            "exactly the ticked contact"
        );
        assert!(body.contains("VERSION:3.0"), "as the chosen version");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: exporting_everything_writes_every_contact ---
    {
        // The export effect, as the palette or an agent invokes it, saves
        // every contact as vCard 4.0 where the person chooses.
        let (dir, vcf) = scratch_export();
        let mut h = Harness::live(1280, 800);
        let total = h.app.store.filtered_sorted("", None, h.app.sort).len();
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(vcf.clone()));
        let _host = ui::host::enter(&services);
        h.app
            .app
            .effect_confirmed(&crate::actions::ContactsEffect::ExportAllVcard)
            .expect("the export runs");
        let body = std::fs::read_to_string(&vcf).unwrap();
        assert_eq!(
            body.matches("BEGIN:VCARD").count(),
            total,
            "every contact written"
        );
        assert!(body.contains("VERSION:4.0"), "Export 4.0 emits v4 cards");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// =================================================================
// Real persistence + first-launch behaviour
// =================================================================

fn scratch_book() -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "contacts-app-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    (dir.clone(), dir.join("contacts.vcf"))
}

/// Run the live Import action under a recording host whose multi-select
/// chooser answers `outcome`, and check the app asked for that chooser.
fn import_with(app: &mut Live, outcome: ui::portal::MultiOutcome) {
    let (services, recorder) = ui::host::HostServices::recording();
    recorder.respond_to_pick_multiple(outcome);
    let result = ui::host::scoped(&services, || {
        app.app.effect(&crate::actions::ContactsEffect::ImportVcard)
    });
    let picks = recorder.pick_requests();
    assert_eq!(picks.len(), 1, "one file chooser was requested");
    assert_eq!(picks[0].title(), "Import contacts");
    assert!(picks[0].is_multiple(), "Import picks several files at once");
    result.expect("the import runs");
}

/// Export asks for a format for the people the list shows, then writes the
/// file the Save dialog chose; nothing is written before.
#[test]
#[serial_test::serial]
fn export_asks_a_format_and_writes_the_chosen_file() {
    let dir = scratch_dir();
    let vcf = dir.join("out.vcf");
    let mut app = synced();
    app.set_group_filter(Some("Friends".into()));
    app.publish();
    assert!(
        app.more_rows().contains(&(MoreAction::Export, true)),
        "the More menu offers the export"
    );
    app.choose_more(MoreAction::Export);
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
    ask(&mut app, Request::Dialog(DialogChoice::Primary));
    assert!(app.export.is_none(), "choosing Export closes the dialog");
    {
        // The live app must NOT invent sample contacts on an empty store.
        let (dir, _path) = scratch_book();
        let mut app = ContactsApp::new();
        let doc = AutoCommit::new();
        app.sync(&doc, &ROOT);
        assert!(
            app.store.is_empty(),
            "a fresh disk-backed launch shows an empty book"
        );
        assert!(
            app.selected.is_none(),
            "nothing is selected when the book is empty"
        );
        // And a full frame over the empty state must not panic.
        drive_frame(&mut app, 1280, 800);
        drive_frame(&mut app, 560, 720);
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(app.portal_in_flight());
    assert!(
        !vcf.exists(),
        "nothing is written until a destination is chosen"
    );
    assert_eq!(recorder.pick_requests()[0].title(), "Export contacts");

    // --- phase: first_run_state_is_actionable_by_pointer_keyboard_and_assistive_tech ---
    {
        let mut app = ContactsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&doc, &ROOT);
        drive_frame(&mut app, 760, 620);

        assert!(app.store.is_empty());
        assert!(app.empty_action_rect.width() >= 44.0);
        let first_run_area = Rect::new(
            app.rect.x0,
            app.rect.y0 + app.toolbar_h(),
            app.rect.x1,
            app.rect.y1,
        );
        assert!(first_run_area.contains(app.empty_action_rect.center()));

        let root = app
            .accessibility()
            .expect("Contacts exposes a semantic window");
        assert_eq!(root.role, ui::a11y::Role::Window);
        assert!(
            root.children
                .iter()
                .flat_map(|node| node.children.iter())
                .all(|node| node.action_target.as_deref() != Some("contacts.export")),
            "an empty address book does not advertise a meaningless export action",
        );
        let action = root
            .children
            .iter()
            .flat_map(|node| node.children.iter())
            .find(|node| node.action_target.as_deref() == Some("contacts.empty.new"))
            .expect("the first-run action is exposed");
        assert_eq!(action.role, ui::a11y::Role::Button);

        assert!(app.perform_semantic_action(
            "contacts.empty.new",
            &ui::a11y::Action::Focus,
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
        ));
        assert!(app.empty_action_focused);
        assert!(app.perform_semantic_action(
            "contacts.empty.new",
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
        ));
        assert_eq!(app.mode, Mode::Edit);
        assert!(app.form.as_ref().is_some_and(|form| form.is_new));
    }
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
    // The production restart path: a contact created through the real edit
    // flow goes into the book's document, which the app's record keeps, and
    // a second launch on the same profile finds the contact again.
    {
        let mut app = Live::empty(1280, 800);
        assert!(app.store.is_empty(), "a fresh profile shows an empty book");
        let mut c = Contact::new();
        c.first = "Persisted".into();
        c.last = "Person".into();
        c.phones.push(Labeled::new("mobile", "+1 555 0100"));
        app.begin_edit(c, true);
        app.save_edit();
        app.settle();
        assert_eq!(app.store.len(), 1, "the contact is in the working set");

        let app = app.reopen();
        assert_eq!(
            app.store.len(),
            1,
            "the saved contact loads on the next launch"
        );
        let loaded = &app.store.contacts[0];
        assert_eq!(loaded.first, "Persisted");
        assert_eq!(loaded.phones[0].value, "+1 555 0100");
    }

    // --- phase: import_reads_real_vcf_files_and_persists_them ---
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

        let mut app = Live::empty(1280, 800);
        assert!(app.store.is_empty());

        // Drive the parse+merge core directly with the paths the portal would
        // have handed back (the picker itself can't run headless).
        let summary = app.import_paths(&[vcf_a, vcf_b]);
        assert_eq!(
            summary.created, 3,
            "all three cards across the two files were imported"
        );
        assert_eq!(
            app.store.len(),
            3,
            "the parsed cards are in the working set"
        );
        let firsts: std::collections::HashSet<&str> = app
            .store
            .contacts
            .iter()
            .map(|c| c.first.as_str())
            .collect();
        assert!(firsts.contains("Imported"));
        assert!(firsts.contains("Ada"));
        assert!(firsts.contains("Grace"));

        // The import reached the book's document, as one step.
        app.settle();
        assert_eq!(app.book().len(), 3, "written to the doc");
        assert!(app.app.undo(), "the import is one undo step");
        assert!(app.book().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: deleting_a_contact_persists_the_removal ---
    {
        let mut app = Live::sample(1280, 800);
        let before = app.store.len();
        let removed = app.store.contacts[0].id;
        app.delete_selected(removed);
        app.settle();

        // Relaunch: the deletion stuck.
        let mut app = app.reopen();
        let reloaded = app.book();
        assert_eq!(
            reloaded.len(),
            before - 1,
            "the removal was written to the record"
        );
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

        let mut app = Live::empty(1280, 800);

        let first = app.import_paths(std::slice::from_ref(&vcf));
        assert_eq!(first.created, 1);
        assert_eq!(app.store.len(), 1);
        let second = app.import_paths(std::slice::from_ref(&vcf));
        assert_eq!(second.created, 0);
        assert_eq!(second.unchanged, 1);
        assert_eq!(
            app.store.len(),
            1,
            "an exported UID resolves to the existing contact rather than a duplicate"
        );
        assert_eq!(app.store.contacts[0].id, card.id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: reimporting_the_same_uidless_card_is_idempotent ---
    {
        let (dir, _path) = scratch_book();
        std::fs::create_dir_all(&dir).unwrap();
        let vcf = dir.join("external.vcf");
        std::fs::write(
            &vcf,
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nTEL;TYPE=work:+1 202 555 0143\r\nEMAIL:grace@example.com\r\nEND:VCARD\r\n",
        )
        .unwrap();

        let mut app = Live::empty(1280, 800);
        let first = app.import_paths(std::slice::from_ref(&vcf));
        let imported_id = app.store.contacts[0].id;
        let imported_phone_id = app.store.contacts[0].phones[0].id;
        let second = app.import_paths(std::slice::from_ref(&vcf));

        assert_eq!(first.created, 1);
        assert_eq!(second.created, 0);
        assert_eq!(second.unchanged, 1);
        assert_eq!(app.store.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: disconnected_uidless_imports_converge_on_one_contact_and_stable_rows ---
    {
        let card = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nTEL;TYPE=work:+1 202 555 0143\r\nEMAIL:grace@example.com\r\nCATEGORIES:Pioneers\r\nEND:VCARD\r\n";
        let mut ancestor = AutoCommit::new();
        ancestor.commit();
        let mut left = ancestor.fork();
        let mut right = ancestor.fork();
        let left_contact = vcard::parse_vcards(card).pop().unwrap();
        let right_contact = vcard::parse_vcards(card).pop().unwrap();
        assert_eq!(left_contact, right_contact);

        tilo_actions::Action::apply(
            &crate::actions::create_contact_action(&left_contact),
            &mut left,
        )
        .unwrap();
        tilo_actions::Action::apply(
            &crate::actions::create_contact_action(&right_contact),
            &mut right,
        )
        .unwrap();
        left.commit();
        right.commit();
        left.merge_incoming(&mut right).unwrap();

        let merged = ContactsDoc::list_contacts(&left);
        assert_eq!(
            merged.len(),
            1,
            "the repeated external import is one entity"
        );
        assert_eq!(merged[0].phones.len(), 1);
        assert_eq!(merged[0].emails.len(), 1);
        assert_eq!(merged[0].groups.len(), 1);
        assert_eq!(merged[0].id, left_contact.id);
        assert_eq!(merged[0].phones[0].id, left_contact.phones[0].id);
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

        let mut app = Live::empty(1280, 800);
        assert_eq!(app.import_paths(&[original_file]).created, 1);

        let result = app.import_paths(&[malicious_file]);
        assert_eq!(result.conflicts, 1);
        assert!(result.failure_message().contains("review"));
        let review = app
            .import_conflict_rows()
            .into_iter()
            .next()
            .expect("the pending collision has an addressable review resource")
            .into_resource();
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
        // An empty selection (or a cancelled picker) fabricates nothing: the
        // book stays empty and disk is untouched — the button never silently
        // invents contacts.
        let (dir, path) = scratch_book();
        let mut app = Live::empty(1280, 800);
        assert!(app.store.is_empty());

        // Empty selection.
        assert!(!app.import_paths(&[]).completed());
        // A cancelled picker, through the import effect.
        import_with(&mut app, ui::portal::MultiOutcome::Cancelled);
        app.settle();

        assert!(app.store.is_empty(), "import of nothing adds nothing");
        assert!(app.book().is_empty(), "and writes nothing to the doc");
        assert!(!path.exists(), "and never touches disk");
        let _ = &path;
        assert!(app.toast.is_none(), "a cancelled/empty import stays silent");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Importing reads the chosen files off the UI thread and merges them as
/// one step; an export with nothing chosen asks for no file.
#[test]
fn import_and_export_run_through_the_file_chooser() {
    // --- phase: chosen_files_import_through_the_import_effect ---
    {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let vcf = dir.join("incoming.vcf");
        std::fs::write(
            &vcf,
            "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Drained Person\r\nN:Person;Drained;;;\r\nEND:VCARD\r\n",
        )
        .unwrap();

        let mut app = Live::empty(1280, 800);
        assert!(app.store.is_empty());
        import_with(&mut app, ui::portal::MultiOutcome::Selected(vec![vcf]));
        assert!(!app.portal_in_flight(), "the import ended");
        assert_eq!(app.book().len(), 1, "the chosen file was imported");
        assert_eq!(app.book()[0].first, "Drained");
        assert!(app.app.undo(), "the import is one undo step");
        assert!(app.book().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- phase: a_running_import_refuses_a_second ---
    {
        let mut app = Live::empty(1280, 800);
        let _running = app.begin_file_work(FileWork::Import).unwrap();
        let (services, _recorder) = ui::host::HostServices::recording();
        let refused = ui::host::scoped(&services, || {
            app.app.effect(&crate::actions::ContactsEffect::ImportVcard)
        });
        assert!(refused.is_err(), "one file operation runs at a time");
    }

    // --- phase: export_with_empty_selection_never_opens_the_portal ---
    {
        // Choosing a version with nothing ticked must not spin up a Save
        // dialog — it guides the user instead.
        let mut h = Harness::live(1280, 800);
        let opened = h.app.request_export(&[], VCardVersion::V3);
        assert!(!opened, "an empty selection does not open the Save dialog");
        assert!(!h.app.portal_in_flight(), "no export runs");
        match &h.app.toast {
            Some((msg, _)) => assert!(msg.to_lowercase().contains("select")),
            None => panic!("an empty export must surface a guiding toast"),
        }
    }
}

/// An imported card that collides with a known identity is a decision:
/// keep the existing contact, or import it as a new one.
#[test]
fn duplicate_and_conflict_review_is_accessible() {
    // --- phase: import_conflict_review_is_visible_accessible_and_resolves_both_choices ---
    {
        fn conflicting_app(w: u32, h: u32) -> (Live, Contact, Contact) {
            let original = sample_contacts().into_iter().next().unwrap();
            let mut incoming = original.clone();
            incoming.emails[0].value = "attacker@example.invalid".into();
            let mut app = Live::empty(w, h);
            let mut summary = ImportSummary::default();
            let mut first_new = None;
            app.absorb_import_contact(original.clone(), &mut summary, &mut first_new);
            app.settle();
            app.absorb_import_contact(incoming.clone(), &mut summary, &mut first_new);
            assert_eq!(summary.conflicts, 1);
            app.recompose();
            (app, original, incoming)
        }

        // Wide rendering exposes a real modal rather than only a toast or a
        // palette resource, and its two decisions are in the semantic tree.
        let (mut app, original, _) = conflicting_app(1280, 800);
        let token = app.import_conflicts[0].token;
        let keep = import_conflict_target(token, "keep-existing");
        let import_as_new = import_conflict_target(token, "import-as-new");
        let tree = app.accessibility().expect("review dialog is accessible");
        assert_eq!(tree.role, Role::Dialog);
        assert!(tree.state.modal);
        assert!(access_target(&tree, &keep).is_some());
        assert!(access_target(&tree, &import_as_new).is_some());
        app.ensure_import_conflict_dialog();
        let mut tr = TextRenderer::new();
        let rect = app.rect;
        app.import_conflict_dialog
            .as_mut()
            .unwrap()
            .dialog
            .layout_in(rect, &Theme::dark(), &mut tr);
        assert!(app.modal_capture().is_some(), "review dialog is painted");
        assert!(perform(&mut app, &keep, &ui::a11y::Action::Activate));
        app.settle();
        assert!(app.import_conflicts.is_empty());
        assert_eq!(app.store.len(), 1);
        assert_eq!(
            app.store.get(original.id).unwrap().emails[0].value,
            original.emails[0].value
        );

        // Import-as-new is the destructive, confirmation-labelled decision.
        // It mints a fresh local id and dispatches the normal CreateContact
        // action, never patching the contact identified by the untrusted UID.
        let (mut app, original, incoming) = conflicting_app(560, 720);
        let token = app.import_conflicts[0].token;
        let import_as_new = import_conflict_target(token, "import-as-new");
        app.ensure_import_conflict_dialog();
        let mut tr = TextRenderer::new();
        let rect = app.rect;
        app.import_conflict_dialog
            .as_mut()
            .unwrap()
            .dialog
            .layout_in(rect, &Theme::dark(), &mut tr);
        assert!(
            app.modal_capture().is_some(),
            "review is also rendered narrow"
        );
        assert!(perform(
            &mut app,
            &import_as_new,
            &ui::a11y::Action::Activate,
        ));
        app.settle();
        assert!(app.import_conflicts.is_empty());
        assert_eq!(app.store.len(), 2);
        assert_eq!(
            app.store.get(original.id).unwrap().emails[0].value,
            original.emails[0].value
        );
        assert!(app.store.contacts.iter().any(|contact| {
            contact.id != original.id && contact.emails[0].value == incoming.emails[0].value
        }));
        assert_eq!(app.book().len(), 2, "Import as new reached the document");
    }

    // --- phase: narrow_duplicate_review_is_scrollable_and_exposes_explicit_value_choices ---
    {
        let mut app = Live::scenario("duplicate-review", 390, 844);
        app.text_scale = 2.0;

        let review = app.duplicate_review.as_ref().expect("review is open");
        let geometry = app.duplicate_review_geometry(review);
        assert!(geometry.full_screen);
        assert_eq!(
            geometry.rows.len(),
            7,
            "the fixture exercises every scalar conflict"
        );
        assert!(
            geometry.max_scroll > 0.0,
            "every conflict remains reachable by scrolling"
        );
        assert!(
            geometry.cancel.y1 < geometry.merge.y0,
            "footer actions stack instead of colliding"
        );

        let tree = app.accessibility().expect("duplicate review has semantics");
        assert_eq!(tree.role, Role::Dialog);
        assert!(tree.state.modal);
        let current = "contacts.duplicate.field:Organization:current";
        let other = "contacts.duplicate.field:Organization:other";
        assert!(access_target(&tree, current).is_some());
        assert!(access_target(&tree, other).is_some());
        assert!(perform(&mut app, other, &ui::a11y::Action::Activate));
        assert!(app.duplicate_review.as_ref().is_some_and(|review| {
            review
                .take_from_source
                .contains(&crate::actions::MergeField::Organization)
        }));
        assert!(perform(&mut app, current, &ui::a11y::Action::Activate));
        assert!(app.duplicate_review.as_ref().is_some_and(|review| {
            !review
                .take_from_source
                .contains(&crate::actions::MergeField::Organization)
        }));
    }

    // --- phase: discovered_duplicates_have_a_canonical_responsive_review_route ---
    {
        let mut app = Live::scenario("duplicates", 1_280, 800);
        let candidate = app
            .duplicate_candidate
            .clone()
            .expect("shared email/name makes a candidate");

        // The wide toolbar reserves a separate hit target, so its action never
        // crowds import/export/new and is exposed with the safety explanation.
        assert!(app.hit.duplicates.area() > 0.0);
        for other in [app.hit.import, app.hit.export, target(&app, "contacts.new")] {
            assert!(
                app.hit.duplicates.intersect(other).area() <= 0.0,
                "duplicate review target must not overlap another toolbar action"
            );
        }
        app.last_mouse = app.hit.duplicates.center();
        assert_eq!(
            Widget::tooltip(&*app),
            Some("Review possible duplicate — nothing merges automatically")
        );
        let tree = app.accessibility().expect("toolbar has semantics");
        let review = access_target(&tree, "contacts.duplicates").expect("review is focusable");
        assert_eq!(review.role, Role::Button);
        assert!(
            review
                .description
                .as_deref()
                .is_some_and(|copy| copy.contains("Nothing is merged automatically"))
        );

        // Semantic activation serializes the same typed View operation published
        // by the exact duplicate resource, then opens the field-level review.
        assert!(perform(
            &mut app,
            "contacts.duplicates",
            &ui::a11y::Action::Activate,
        ));
        let opened = app.duplicate_review.as_ref().expect("review opened");
        assert_eq!(opened.survivor.id, candidate.survivor_id);
        assert_eq!(opened.source.id, candidate.source_id);

        // At 200% on a phone width, every compact toolbar target remains a
        // distinct touch/focus rectangle rather than turning into overlaid icons.
        app.duplicate_review = None;
        app.text_scale = 2.0;
        app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
        app.layout();
        assert!(app.toolbar_is_compact());
        for (left, right) in [
            (app.hit.import, app.hit.duplicates),
            (app.hit.duplicates, app.hit.export),
            (app.hit.export, target(&app, "contacts.new")),
        ] {
            assert!(
                left.intersect(right).area() <= 0.0,
                "compact duplicate controls remain disjoint at 200%"
            );
            assert!(left.width() >= 40.0 && right.width() >= 40.0);
        }
    }
}

fn access_target<'a>(node: &'a AccessNode, target: &str) -> Option<&'a AccessNode> {
    if node.action_target.as_deref() == Some(target) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| access_target(child, target))
}

/// Export and unsaved modals are focus-trapped and semantic; detail, editor and popup controls reach the durable action path.
#[allow(
    clippy::too_many_lines,
    reason = "this end-to-end semantic route test intentionally exercises each popup control in one readable scenario"
)]
#[test]
fn modals_and_editors_expose_semantic_action_routes() {
    // --- phase: export_modal_is_focus_trapped_semantic_and_virtualized ---
    {
        let mut h = Harness::new(900, 640);
        h.press_left(h.app.hit.export.center());
        assert!(h.app.traps_focus(), "export blocks focus behind its scrim");
        let tree = h.app.accessibility().expect("export has semantics");
        assert_eq!(tree.role, Role::Dialog);
        assert!(tree.state.modal);
        assert!(access_target(&tree, "contacts.export.select-none").is_some());

        assert!(h.press_named(NamedKey::Tab));
        assert!(
            access_target(
                &h.app.accessibility().expect("focused export semantics"),
                "contacts.export.select-all",
            )
            .is_some_and(|node| node.state.focused),
            "Tab moves focus into the export dialog rather than behind its scrim",
        );
        assert!(h.press_named(NamedKey::Space));
        assert_eq!(h.app.export_dialog.selection().len(), h.app.store.len());

        assert!(perform(
            &mut h.app,
            "contacts.export.select-none",
            &ui::a11y::Action::Activate
        ));
        assert!(h.app.export_dialog.selection().is_empty());
        let id = h
            .app
            .export_dialog
            .geometry(h.app.rect, &h.app.store, h.app.sort)
            .rows[0]
            .0;
        let target = format!("contacts.export.row:{id}");
        assert!(perform(&mut h.app, &target, &ui::a11y::Action::ToggleCheck));
        assert!(h.app.export_dialog.selection().contains(&id));

        h.app.store.contacts = (0..1_000)
            .map(|index| Contact {
                id: uuid::Uuid::new_v4(),
                first: format!("Export {index:04}"),
                ..Contact::default()
            })
            .collect();
        let tree = h
            .app
            .accessibility()
            .expect("large export remains semantic");
        let list = tree
            .children
            .iter()
            .find(|node| node.role == Role::List)
            .expect("export choice list is exposed");
        assert!(
            list.children.len() < h.app.store.len(),
            "export materializes only visible semantic rows"
        );
    }

    // --- phase: unsaved_modal_is_focus_trapped_semantic_and_resolves_via_action ---
    {
        let mut h = Harness::new(1280, 800);
        let current = h.app.selected.expect("sample selects one contact");
        let contact = h.app.store.get(current).cloned().unwrap();
        h.app.begin_edit(contact, false);
        h.app
            .form
            .as_mut()
            .and_then(|form| {
                form.inputs
                    .iter_mut()
                    .find(|input| input.slot == Slot::Title)
            })
            .expect("sample editor has title")
            .ti
            .set_text("Discard me");
        let other = h
            .app
            .visible_ids()
            .into_iter()
            .find(|id| *id != current)
            .expect("sample has another contact");
        h.app.request_select(other);
        assert!(h.app.traps_focus(), "unsaved choice traps focus");
        let tree = h.app.accessibility().expect("unsaved dialog has semantics");
        assert_eq!(tree.role, Role::Dialog);
        assert!(tree.state.modal);
        assert!(access_target(&tree, "contacts.unsaved.discard").is_some());
        assert!(h.press_named(NamedKey::Tab));
        assert_eq!(h.app.unsaved_focus, UnsavedFocus::Discard);
        assert!(perform(
            &mut h.app,
            "contacts.unsaved.discard",
            &ui::a11y::Action::Activate
        ));
        assert!(h.app.pending_switch.is_none());
        assert_eq!(h.app.selected, Some(other));
    }

    // --- phase: detail_and_editor_semantics_reach_the_same_durable_action_path ---
    {
        let mut app = Live::sample(1280, 800);

        let id = app.selected.expect("sample contact is selected");
        let favorite = app.store.get(id).expect("selected contact exists").favorite;
        let favorite_target = format!("contacts.detail.favorite:{id}");
        let tree = app.accessibility().expect("Contacts exposes semantics");
        let favorite_button = access_target(&tree, &favorite_target)
            .expect("the painted favorite star is reachable to assistive tech");
        assert_eq!(favorite_button.role, Role::ToggleButton);
        assert_eq!(favorite_button.state.checked, Some(favorite));

        assert!(perform(
            &mut app,
            &favorite_target,
            &ui::a11y::Action::Activate,
        ));
        assert_eq!(
            app.store.get(id).map(|contact| contact.favorite),
            Some(!favorite),
            "semantic activation updates the visible model"
        );
        assert_eq!(
            app.book()
                .into_iter()
                .find(|contact| contact.id == id)
                .map(|contact| contact.favorite),
            Some(!favorite),
            "and dispatches the canonical collaborative action into the document"
        );

        let contact = app.store.get(id).cloned().unwrap();
        app.begin_edit(contact.clone(), false);
        rerender_at(&mut app, 1280, 800);
        let first_target = form_field_target(id, Slot::First);
        let save_target = format!("contacts.form.save:{id}");
        let tree = app.accessibility().expect("editor semantics are exposed");
        assert_eq!(
            access_target(&tree, &first_target).map(|node| node.role),
            Some(Role::TextInput),
            "a painted draft field is a real accessible text input"
        );
        assert!(access_target(&tree, &save_target).is_some());

        assert!(perform(
            &mut app,
            &first_target,
            &ui::a11y::Action::SetValue("Augusta".into()),
        ));
        assert_eq!(
            app.form.as_ref().map(|form| form.draft.first.as_str()),
            Some(contact.first.as_str()),
            "SetValue remains a cancellable local draft until Save"
        );
        assert!(perform(&mut app, &save_target, &ui::a11y::Action::Activate,));
        assert_eq!(
            app.book()
                .into_iter()
                .find(|stored| stored.id == id)
                .map(|stored| stored.first),
            Some("Augusta".into()),
            "Save commits the assistive edit through ContactsAction"
        );
    }

    // --- phase: semantic_popup_controls_cover_type_remove_calendar_group_and_unique_targets ---
    {
        fn assert_unique_targets(node: &AccessNode, seen: &mut std::collections::HashSet<String>) {
            if let Some(target) = &node.action_target {
                assert!(
                    seen.insert(target.clone()),
                    "duplicate semantic target: {target}"
                );
            }
            for child in &node.children {
                assert_unique_targets(child, seen);
            }
        }

        let mut app = Live::sample(1280, 800);
        let id = app.selected.unwrap();
        let contact = app.store.get(id).cloned().unwrap();
        app.begin_edit(contact, false);
        app.recompose();

        let (type_row, _) = app.hit.type_chips[0];
        let type_target = type_chip_target(id, type_row);
        assert!(perform(&mut app, &type_target, &ui::a11y::Action::Activate,));
        assert!(matches!(app.popup, Some(Popup::TypeMenu { .. })));
        let option_target = type_option_target(id, type_row, "other");
        let tree = app.accessibility().unwrap();
        assert_eq!(
            access_target(&tree, &option_target).map(|node| node.role),
            Some(Role::RadioButton)
        );
        assert!(perform(
            &mut app,
            &option_target,
            &ui::a11y::Action::Activate,
        ));
        assert_eq!(
            type_label(&app.form.as_ref().unwrap().draft, type_row),
            "other"
        );

        let before = app.form.as_ref().unwrap().draft.phones.len();
        let remove_target = remove_type_target(id, type_row);
        assert!(perform(
            &mut app,
            &remove_target,
            &ui::a11y::Action::Activate,
        ));
        assert_eq!(app.form.as_ref().unwrap().draft.phones.len(), before - 1);

        rerender_at(&mut app, 1280, 800);
        let calendar = calendar_target(id);
        assert!(perform(&mut app, &calendar, &ui::a11y::Action::Activate,));
        let Some(Popup::Calendar { year, month, .. }) = app.popup else {
            panic!("semantic calendar button opens the date picker");
        };
        let day = calendar_day_target(id, year, month, 1);
        let tree = app.accessibility().unwrap();
        assert!(
            access_target(&tree, &day).is_some(),
            "calendar days are exposed"
        );
        assert!(perform(&mut app, &day, &ui::a11y::Action::Activate));
        assert_eq!(
            app.form
                .as_ref()
                .unwrap()
                .draft
                .birthday
                .map(|birthday| birthday.iso()),
            Some(format!("{year:04}-{month:02}-01")),
            "calendar selection remains in the cancellable draft"
        );

        app.cancel_edit();
        rerender_at(&mut app, 1280, 800);
        assert!(perform(
            &mut app,
            "contacts.group",
            &ui::a11y::Action::Expand,
        ));
        let group = "Friends";
        let group_target = group_option_target(group);
        let tree = app.accessibility().unwrap();
        assert_eq!(
            access_target(&tree, &group_target).map(|node| node.role),
            Some(Role::RadioButton)
        );
        assert!(perform(
            &mut app,
            &group_target,
            &ui::a11y::Action::Activate,
        ));
        assert_eq!(app.group_filter.as_deref(), Some(group));

        let mut targets = std::collections::HashSet::new();
        assert_unique_targets(&app.accessibility().unwrap(), &mut targets);
    }
}

/// The contacts app as every host runs it — the framework root over the
/// address book, its edits through the book's door — with its surface at hand
/// for assertions about view state.
struct Live {
    app: ui::framework::testing::TestApp<crate::ContactsShell>,
    /// The surface inside `app`'s widget tree, which owns it for as long as
    /// `app` lives; the tree is built once and never replaced.
    surface: *mut ContactsApp,
    viewport: (u32, u32),
}

impl Live {
    /// The sample book, as a capture stages it, at `w`x`h`.
    fn sample(w: u32, h: u32) -> Self {
        Self::mount(
            ui::framework::testing::TestApp::capture_at(None, (w, h)).unwrap(),
            (w, h),
        )
    }

    /// The sample book in a window someone uses, where effects run.
    fn live_sample(w: u32, h: u32) -> Self {
        let mut app = ui::framework::testing::TestApp::<crate::ContactsShell>::new().unwrap();
        for invoke in <crate::ContactsShell as ui::framework::ResourceApp>::scenario(None) {
            app.invoke(invoke).unwrap();
        }
        app.settle();
        Self::mount(app, (w, h))
    }

    /// The capture of `scenario`, at `w`x`h`.
    fn scenario(scenario: &str, w: u32, h: u32) -> Self {
        Self::mount(
            ui::framework::testing::TestApp::capture_at(Some(scenario), (w, h)).unwrap(),
            (w, h),
        )
    }

    /// A fresh, empty book at `w`x`h`.
    fn empty(w: u32, h: u32) -> Self {
        Self::mount(ui::framework::testing::TestApp::new().unwrap(), (w, h))
    }

    fn mount(
        mut app: ui::framework::testing::TestApp<crate::ContactsShell>,
        viewport: (u32, u32),
    ) -> Self {
        app.harness().set_theme(Theme::dark());
        app.harness()
            .resize(viewport)
            .expect("contacts frame lays out");
        let surface = app
            .with_widget(|surface: &mut ContactsApp| std::ptr::from_mut(surface))
            .expect("the contacts surface is hosted");
        Self {
            app,
            surface,
            viewport,
        }
    }

    fn harness_mut(&mut self) -> &mut ui::headless::FrameHarness {
        self.app.harness()
    }

    /// Run the frame after an event: submitted edits apply, and what they
    /// change is presented and drawn.
    fn settle(&mut self) {
        self.app.settle();
    }

    fn recompose(&mut self) {
        self.app
            .harness()
            .recompose()
            .expect("contacts frame lays out");
    }

    /// Deliver one window event to the app. What it submits applies on
    /// the next frame ([`Self::settle`]).
    fn dispatch(&mut self, event: &WindowEvent, mods: ModifiersState) -> bool {
        self.app
            .harness()
            .dispatch_to_root_with(event, &mods.into())
    }

    /// The address book as the document holds it once the edits submitted
    /// so far have applied.
    fn book(&mut self) -> Vec<Contact> {
        self.settle();
        self.app.state().contacts
    }

    /// Close the app and launch it again on the same profile, as after a
    /// restart.
    fn reopen(self) -> Self {
        let viewport = self.viewport;
        Self::mount(self.app.reopen().expect("the app relaunches"), viewport)
    }

    fn request(&mut self, request: Request) {
        self.shared.borrow_mut().push(request);
        self.app.settle();
    }
    fn invoke(&mut self, action: &crate::actions::ContactsAction) {
        self.app
            .invoke(ui::tilo_resource::Invoke::apply(action).unwrap())
            .unwrap();
        self.settle();
    }
    fn choose_menu(&mut self, index: usize) {
        let rect = self
            .menu
            .item_rect(index)
            .expect("the actual context-menu row is laid out");
        self.app
            .harness()
            .click_at(rect.center().x, rect.center().y);
        self.settle();
    }
    fn choose_more(&mut self, action: MoreAction) {
        self.more_pick.lock().unwrap().replace(action);
        self.settle();
    }
    fn save_edit(&mut self) {
        self.request(Request::Save);
    }
    fn toggle_favorite(&mut self, id: ContactId) -> bool {
        let Some(before) = self.store.get(id).map(|contact| contact.favorite) else {
            return false;
        };
        self.selected = Some(id);
        self.request(Request::ToggleFavorite);
        self.store
            .get(id)
            .is_some_and(|contact| contact.favorite != before)
    }

    /// The address book as it would be after `action`, applied to a copy of
    /// the document as the palette's action of the same name applies it.
    fn book_after(&self, action: &crate::actions::ContactsAction) -> Vec<Contact> {
        let mut peer = self.app.peer_document();
        tilo_actions::Action::apply(action, &mut peer).expect("the palette's action applies");
        ContactsDoc::list_contacts(&peer)
    }
}

impl std::ops::Deref for Live {
    type Target = ContactsApp;

    fn deref(&self) -> &ContactsApp {
        // SAFETY: `surface` points into the tree `app` owns and never
        // rebuilds; `&self` keeps `app` alive and unborrowed.
        unsafe { &*self.surface }
    }
}

impl std::ops::DerefMut for Live {
    fn deref_mut(&mut self) -> &mut ContactsApp {
        // SAFETY: as in `deref`; `&mut self` makes this the only access.
        unsafe { &mut *self.surface }
    }
}

/// Lay the same instance out at `w`x`h`, to simulate a live window resize (the
/// ui loop re-runs layout on the existing widget when the allocation changes).
fn rerender_at(app: &mut Live, w: u32, h: u32) {
    app.harness_mut()
        .relayout_themed((w, h), Theme::dark())
        .expect("contacts frame lays out");
}

/// Perform `action` on the semantic `target` the surface publishes, as
/// assistive technology does, and run the frame after it.
fn perform(app: &mut Live, target: &str, action: &ui::a11y::Action) -> bool {
    let handled = app
        .harness_mut()
        .perform_semantic_named("contacts", Some(target), action)
        .is_ok();
    app.settle();
    handled
}

/// Form rows, pills, panel lifts, rows and toolbar faces are authored at 1x and grow with the text scale on every preset.
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
)]
#[test]
fn duplicates_are_reviewed_field_by_field_before_merging() {
    let mut app = synced();
    let ada = selected_contact(&app);
    let mut copy = ada.clone();
    copy.id = uuid::Uuid::from_u128(ada.id.as_u128() + 0x100);
    copy.org = "Babbage & Co.".into();
    app.invoke(&crate::actions::create_contact_action(&copy));
    app.settle();
    assert!(
        app.duplicate_candidate.is_some(),
        "the copy is a likely duplicate"
    );

    assert!(
        app.more_rows()
            .contains(&(MoreAction::ReviewDuplicate, true)),
        "the More menu offers the review"
    );
    app.choose_more(MoreAction::ReviewDuplicate);
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

    ask(&mut app, Request::Dialog(DialogChoice::Primary));
    app.settle();
    let book = app.book();
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

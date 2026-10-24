use super::files::ImportSummary;
use super::*;
use crate::form::{Slot, TYPE_OPTIONS, TypeTarget};
use crate::model::normalize_text;
use crate::model::sample_contacts;
use crate::model::{Address, Birthday};
use crate::resource::Contacts;
use crate::vcard;
use crate::view::DialogChoice;
use automerge::AutoCommit;
use automerge::ROOT;
use std::collections::HashSet;
use std::path::PathBuf;
use taffy::{AvailableSpace, Size, Style};
use ui::a11y::{AccessNode, Role};
use ui::tilo_resource::{Invoke, Replica};
use ui::time::Duration;
use ui::tokens::colors::ON_FILL_MIN_CONTRAST;
use vello::kurbo::Point;

fn target(app: &ContactsApp, id: &str) -> Rect {
    ui::widget::find_test_id(app, id)
        .unwrap_or_else(|| panic!("production target {id} is absent"))
        .get_rect()
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
fn act_named(app: &mut Live, name: &str, action: &ui::a11y::Action) {
    app.settle();
    let id = app
        .harness_mut()
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("actual semantic control {name} is absent"));
    app.harness_mut()
        .perform_a11y_action(id, action)
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    app.settle();
}

fn menu_rows(app: &ContactsApp) -> std::collections::HashMap<usize, Rect> {
    let Some(OpenMenu::Contact(_, rows)) = &app.open_menu else {
        panic!("the actual contact menu is open");
    };
    rows.iter()
        .enumerate()
        .filter(|(_, action)| action.is_some())
        .map(|(index, _)| {
            (
                index,
                app.menu
                    .item_rect(index)
                    .expect("actual action-row geometry"),
            )
        })
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
    app.present_book(&std::rc::Rc::new(Contacts {
        contacts: crate::resource::sample_book().into(),
        ..Contacts::default()
    }));
    app.stage_capture(None);
    app
}

/// The sample book synced into a fresh document.
fn synced() -> Live {
    Live::sample(1280, 800)
}

/// Queue `request` as a pane or button would, and let the app apply it.
fn ask(app: &mut Live, request: Request) {
    app.request(request);
}

/// Whether the window's bar offers action `id` now.
fn bar_offers(id: &str) -> bool {
    ui::window_toolbar::declared().is_some_and(|toolbar| {
        toolbar
            .actions
            .iter()
            .chain(&toolbar.leading)
            .any(|action| action.id == id && action.enabled)
    })
}

/// Press `id` in the window's bar, as the desktop hands the press to the
/// app, and let the app apply it.
fn press_bar(app: &mut Live, id: &str) {
    assert!(
        bar_offers(id),
        "{id} is in the window's bar: {:?}",
        ui::window_toolbar::declared()
    );
    ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action { id: id.into() });
    app.settle();
}

fn selected_contact(app: &ContactsApp) -> Contact {
    app.selected
        .and_then(|id| app.store.contact(id))
        .cloned()
        .expect("a contact is selected")
}

/// A real host chooser kept open until the test supplies its destination.
/// Recording's pre-answered picker would race the in-flight/no-write proof.
struct DeferredPicker {
    recorder: ui::host::HostRecorder,
    answer: std::sync::Mutex<Option<std::sync::mpsc::Sender<ui::portal::PortalOutcome>>>,
}

impl DeferredPicker {
    fn new(recorder: ui::host::HostRecorder) -> Self {
        Self {
            recorder,
            answer: std::sync::Mutex::new(None),
        }
    }

    fn choose(&self, path: std::path::PathBuf) {
        self.answer
            .lock()
            .unwrap()
            .take()
            .expect("the actual Save chooser is awaiting a destination")
            .send(ui::portal::PortalOutcome::Opened(path))
            .unwrap();
    }
}

impl ui::host::FilePicker for DeferredPicker {
    fn pick(&self, request: ui::portal::PickRequest) -> ui::portal::PortalChannel {
        let _ = ui::host::FilePicker::pick(&self.recorder, request);
        let (sender, receiver) = std::sync::mpsc::channel();
        assert!(self.answer.lock().unwrap().replace(sender).is_none());
        receiver
    }

    fn pick_multiple(&self, request: ui::portal::PickRequest) -> ui::portal::MultiChannel {
        ui::host::FilePicker::pick_multiple(&self.recorder, request)
    }

    fn open_file(&self, request: ui::portal::PickRequest) -> ui::portal::OpenFileChannel {
        ui::host::FilePicker::open_file(&self.recorder, request)
    }

    fn launch(&self, request: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        ui::host::FilePicker::launch(&self.recorder, request)
    }
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

fn harness(viewport: (u32, u32)) -> Live {
    Live::sample(viewport.0, viewport.1)
}

fn named(harness: &Live, name: &str) -> bool {
    fn contains(widget: &dyn Widget, name: &str) -> bool {
        widget.accessibility().is_some_and(|node| {
            node.name.as_deref() == Some(name)
                || node.children.iter().any(|child| contains_node(child, name))
        }) || widget
            .children()
            .into_iter()
            .any(|child| contains(child, name))
    }
    fn contains_node(node: &AccessNode, name: &str) -> bool {
        node.name.as_deref() == Some(name)
            || node.children.iter().any(|child| contains_node(child, name))
    }
    contains(&**harness, name)
}

/// Whether the window's bar says `message`: its notice, which the bar
/// reads out as a live region.
fn announced(_app: &ContactsApp, message: &str) -> bool {
    ui::window_toolbar::notice().is_some_and(|notice| notice.text == message)
}

/// Press the button `name` beside the notice in the window's bar.
fn press_notice_button(app: &mut Live, name: &str) {
    app.settle();
    let button = ui::window_toolbar::notice()
        .and_then(|notice| notice.action)
        .unwrap_or_else(|| panic!("the window's bar offers {name} beside its notice"));
    assert_eq!(button.name, name);
    app.harness_mut()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: button.id });
    app.settle();
}

fn toast(app: &ContactsApp) -> Option<&str> {
    app.shell.toast_text()
}

/// The window is the shell: groups beside the people beside the person,
/// every header command named, the sample book listed and its first
/// contact shown.
#[test]
fn the_window_lists_groups_people_and_the_selected_person() {
    let harness = harness((1280, 800));
    let app = &*harness;
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
        "All contacts",
        "Favorites",
        "Work",
        shown.display_name().as_str(),
    ] {
        assert!(named(&harness, name), "{name} is exposed");
    }
    assert_eq!(
        ui::window_toolbar::declared()
            .and_then(|toolbar| toolbar.location)
            .map(|location| location.label),
        Some(shown.display_name())
    );
    // What can be done to the person is the window bar's.
    for id in [
        BAR_NEW,
        BAR_FAVORITE,
        BAR_EDIT,
        BAR_DELETE,
        BAR_IMPORT,
        BAR_EXPORT,
    ] {
        assert!(bar_offers(id), "{id} is in the window's bar");
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
        let app = &*harness;
        assert!(
            !app.shell.geometry().stack.is_empty(),
            "the window is a stack"
        );
        assert_eq!(app.shell.front_pane(), NavPane::Overview);
    }
    let grace = id_named(&harness, "Grace Hopper");
    harness.shared.borrow_mut().push(Request::Select(grace));
    harness.harness_mut().send(&WindowEvent::RedrawRequested);
    let app = &*harness;
    assert_eq!(app.selected, Some(grace));
    assert_eq!(
        app.shell.front_pane(),
        NavPane::Content,
        "choosing a person pushes them in"
    );
    assert!(
        bar_offers("nav-shell-back"),
        "the window's bar offers the way back"
    );
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
            .all(|id| app.store.contact(*id).unwrap().favorite)
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
                .contact(*id)
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
        let mut contact = app.store.contact(id).cloned().unwrap();
        let before = contact.clone();
        contact.groups.clear();
        app.invoke(Contacts::save(&before, &contact).unwrap());
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
        .map(|id| app.store.contact(*id).unwrap().last.clone())
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
        .map(|id| normalize_text(&app.store.contact(*id).unwrap().org))
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
    let ((other, other_center), (target, center)) = {
        let app = &*harness;
        let bridge = app.shared.borrow();
        let row = |index: usize| {
            let (id, rect) = bridge.painted[index];
            (id, rect.center())
        };
        (row(2), row(1))
    };
    harness
        .harness_mut()
        .click_at(other_center.x, other_center.y);
    assert_eq!(harness.selected, Some(other));
    assert_eq!(harness.mode, Mode::View, "a single press only selects");

    // A double press on another row selects it and opens it, however long
    // a slow host takes to draw the frame between the two presses.
    harness.harness_mut().double_click_at(center.x, center.y);
    let app = &*harness;
    assert_eq!(
        app.mode,
        Mode::Edit,
        "a double press opens the person for editing"
    );
    assert_eq!(app.form.as_ref().map(|form| form.draft.id), Some(target));

    // Escape leaves the form; the arrows then move through the list.
    harness.harness_mut().key("Escape");
    assert_eq!(harness.mode, Mode::View);
    let rows = list_ids(&harness);
    harness.harness_mut().key("Down");
    assert_eq!(harness.selected, rows.get(2).copied());
    harness.harness_mut().key("Up");
    harness.harness_mut().key("Up");
    assert_eq!(harness.selected, rows.first().copied());

    // The list's semantic rows select too.
    let last = *rows.last().unwrap();
    let name = harness.store.contact(last).unwrap().display_name();
    act_named(&mut harness, &name, &ui::a11y::Action::Activate);
    assert_eq!(harness.selected, Some(last));
}

/// The list's keys: Delete deletes your contact at once and its notice's
/// Undo brings it back, `e` edits, `n` starts a new contact, and an open
/// form ignores bare keys.
#[test]
fn list_keys_delete_edit_and_create() {
    let mut harness = harness((1280, 800));
    let selected = harness.selected.unwrap();
    harness.harness_mut().key("Delete");
    harness.settle();
    assert!(
        !host_confirmation_is_up(&mut harness),
        "deleting your own contact, which Undo takes back, asks nothing"
    );
    assert!(
        harness.store.contact(selected).is_none(),
        "the Delete key deletes the contact"
    );
    assert!(
        toast(&harness).is_some_and(|notice| notice.starts_with("Deleted ")),
        "the notice names what was deleted: {:?}",
        toast(&harness)
    );
    // The notice's Undo asks the host for the undo Ctrl+Z performs.
    press_notice_button(&mut harness, "Undo");
    harness.settle();
    assert!(
        harness.store.contact(selected).is_some(),
        "the notice's Undo brings the contact back"
    );

    harness.harness_mut().key("e");
    assert_eq!(harness.mode, Mode::Edit);
    // Bare letters belong to the form now, not to list shortcuts.
    harness.harness_mut().key("n");
    assert!(
        harness.form.as_ref().is_some_and(|form| !form.is_new),
        "an open form ignores the new-contact key"
    );
    harness.harness_mut().key("Escape");
    harness.harness_mut().key("n");
    let app = &*harness;
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
        let app = &*harness;
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
        .shared
        .borrow_mut()
        .push(Request::ContextMenu(grace, None));
    harness.harness_mut().send(&WindowEvent::RedrawRequested);
    harness.harness_mut().send(&WindowEvent::RedrawRequested);
    let app = &*harness;
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
    let was_favorite = app.store.contact(grace).unwrap().favorite;
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
    assert_eq!(app.store.contact(grace).unwrap().favorite, !was_favorite);
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
        app.export.is_none(),
        "the sensitive export waits for the person's consent"
    );
    assert!(
        host_confirmation_is_up(&mut app),
        "the actual host confirms the contact menu's sensitive operation"
    );
    confirm_on_host(&mut app);
    if app.export.is_none() {
        eprintln!(
            "Contacts menu export after explicit confirmation: controls={:?}; failures={:?}; refusal={:?}",
            app.harness_mut().controls(),
            app.harness_mut().control_failures().to_vec(),
            app.dispatcher
                .as_ref()
                .map(|dispatcher| dispatcher.refused().with(Clone::clone))
        );
    }
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
    app.settle();
    assert!(
        !host_confirmation_is_up(&mut app),
        "the menu's Delete of your own contact asks nothing"
    );
    assert!(app.store.contact(grace).is_none());
    assert!(!app.book().iter().any(|c| c.id == grace));
    assert!(
        app.app
            .state()
            .contacts
            .iter()
            .all(|contact| contact.id != grace),
        "the deletion reaches the book"
    );

    let next = app.selected.unwrap();
    ask(&mut app, Request::ContextMenu(next, Some(Point::ORIGIN)));
    app.choose_menu(index(ContextAction::Edit));
    assert_eq!(app.mode, Mode::Edit);
}

/// The person's header commands: favourite toggles through the document,
/// Delete deletes and Ctrl+Z brings the contact back, and the website
/// opens.
#[test]
fn header_commands_favorite_delete_and_open_the_website() {
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let work = ui::background::Pumped::new();
    let _work = work.background().enter();
    let mut app = synced();
    let ada = selected_contact(&app);
    ask(&mut app, Request::ToggleFavorite);
    assert_eq!(app.store.contact(ada.id).unwrap().favorite, !ada.favorite);
    let star = ui::window_toolbar::declared()
        .and_then(|toolbar| toolbar.actions.into_iter().find(|a| a.id == BAR_FAVORITE))
        .expect("the window's bar offers Favorite");
    assert_eq!(star.on, !ada.favorite, "the toggle follows the person");

    ask(&mut app, Request::DeleteSelected);
    assert!(!host_confirmation_is_up(&mut app), "nothing is asked");
    assert!(
        app.store.contact(ada.id).is_none(),
        "the contact is deleted"
    );
    assert!(app.app.undo(), "the deletion is the person's undo step");
    app.settle();
    assert!(
        app.store.contact(ada.id).is_some(),
        "Ctrl+Z brings the contact back"
    );

    ask(&mut app, Request::OpenWebsite(ada.website.clone()));
    assert_eq!(app.opened_url.as_deref(), Some(ada.website.as_str()));
    work.run_pending();
    assert_eq!(recorder.opened_uris(), [ada.website]);
}

/// Saves round-trip through the document; refused saves, favourites and
/// deletes reconcile and announce errors without claiming success.
#[test]
fn retained_forms_preserve_persistence_and_refusals() {
    check_save_round_trips_through_doc();

    check_refused_save_reconciles_and_announces_an_error_without_claiming_success();

    check_refused_favorite_and_delete_reconcile_without_false_success();

    check_seeds_sample_contacts_and_selects_one();

    check_view_mode_frame_does_not_panic();

    check_edit_mode_with_repeated_fields_frame_does_not_panic();

    check_calendar_and_type_popups_frame_does_not_panic();

    check_importing_state_is_visible_and_announced_as_busy();
}

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
        let entries = <Contacts as ui::tilo_resource::Resource>::search(&Contacts {
            contacts: app.book().into(),
            ..Contacts::default()
        });
        let hits: Vec<_> = entries
            .iter()
            .filter(|entry| entry.text.to_lowercase().contains("navy"))
            .collect();
        assert_eq!(hits.len(), 1, "the organisation finds one contact");
        assert_eq!(hits[0].title, "Grace Hopper");
        let anchor = hits[0].anchor.clone().expect("the hit opens a contact");
        assert_eq!(anchor, crate::resource::contact_resource_id(id));
        app.app
            .call("open", serde_json::json!({ "anchor": anchor }))
            .expect("the open operation succeeds");
        app.settle();
        assert_eq!(app.selected, Some(id), "the open action selected Grace");
    }

    // --- phase: search_activation_reveals_a_contact_hidden_by_the_group ---
    {
        let mut app = Live::sample(1280, 800);
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

        app.app
            .call(
                "open",
                serde_json::json!({ "anchor": crate::resource::contact_resource_id(id) }),
            )
            .expect("the addressed open succeeds");
        app.settle();

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
        let mut app = Live::sample(1280, 800);
        app.group_filter = Some("Friends".into());
        let group = app
            .store
            .all_groups()
            .into_iter()
            .find(|group| group == "Work")
            .expect("samples expose Work");
        app.app
            .effect_on(
                &crate::resource::place_resource_id(&crate::resource::group_place(&group)),
                &crate::actions::ContactsEffect::OpenGroup {
                    name: group.clone(),
                },
            )
            .expect("group open succeeds");
        app.settle();
        assert_eq!(app.group_filter.as_deref(), Some(group.as_str()));
        assert_eq!(
            app.visible_ids().len(),
            app.store
                .contacts
                .iter()
                .filter(|contact| contact
                    .groups
                    .iter()
                    .any(|entry| group_names_equal(&entry.value, &group)))
                .count(),
            "the complete group is revealed"
        );
        assert!(
            !app.visible_ids().is_empty()
                && app.visible_ids().iter().all(|id| app
                    .store
                    .contact(*id)
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
                size: Size::from_lengths(w as f32, h as f32),
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
    app.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    // Encoding the scene is enough to exercise every draw path; the
    // GPU blit is covered by the `--shots` harness.
    let _ = &scene;
}

/// Narrow windows push the detail over the list (by selection, row press or Enter) and back; wide windows stay docked; resizing reflows the same instance.
#[test]
fn single_pane_push_navigation_follows_width() {
    check_narrow_frame_resolves_single_pane_push();

    check_narrow_selection_pushes_detail_then_back_returns_to_list();

    check_wide_frame_stays_docked_side_by_side();

    check_narrow_row_press_pushes_detail_then_back_returns_to_list();

    check_enter_opens_the_selected_contact_in_narrow_push_navigation();

    check_resizing_wide_to_narrow_reflows_the_same_instance();
}

/// 200% text reflows and scales interaction geometry and full names fit the detail row.
#[test]
fn text_scale_reflows_geometry_and_names_fit() {
    let mut app = Live::sample(1280, 900);
    app.request(Request::EditSelected);
    assert!(
        app.shell.geometry().side_by_side,
        "normal text fits both panes"
    );
    let first_1x = form_widget(&app, Slot::First).unwrap().get_rect();
    let row_1x = app.shared.borrow().painted[0].1.height();
    let theme = Theme::dark().with_text_scale(2.0);
    app.harness_mut()
        .relayout_themed((1280, 900), theme)
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
        notes.accessibility().unwrap().state.multiline,
        "the real notes editor publishes its multiline text contract"
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
        let name = h.app.store.contact(id).unwrap().display_name();
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
    fn set_title(app: &ContactsApp, title: &str) {
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
            .and_then(|id| app.store.contact(id))
            .cloned()
            .unwrap();
        let id = c.id;
        app.begin_edit(c, false);
        set_title(&app, "Edited Title");
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
            .and_then(|id| app.store.contact(id))
            .cloned()
            .unwrap();
        app.begin_edit(contact.clone(), false);
        set_title(&app, "Unsaved draft");
        // A collaborator deletes the record while it is being edited.
        app.app
            .merge_peer_edit(|doc| peer_applies(doc, Contacts::delete(contact.id)));
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
            app.store.contact(contact.id).is_none(),
            "the store follows the document"
        );
        let message = toast(&app).unwrap().to_owned();
        assert!(message.starts_with("Couldn't save contact:"), "{message}");
        assert!(
            announced(&app, &message),
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
            .and_then(|id| app.store.contact(id))
            .cloned()
            .unwrap();
        app.app
            .merge_peer_edit(|doc| peer_applies(doc, Contacts::delete(contact.id)));
        app.settle();
        std::rc::Rc::make_mut(&mut app.store)
            .contacts
            .push(contact.clone());
        app.selected = Some(contact.id);

        assert!(
            app.submit(Invoke::edit(&Contacts::favorite(contact.id, !contact.favorite)).unwrap())
                .is_ok(),
            "the stale control submits the typed favorite action"
        );
        app.settle();
        assert!(
            app.store.contact(contact.id).is_none(),
            "favorite refusal reconciles"
        );
        let favorite_error = toast(&app).unwrap().to_owned();
        assert!(
            favorite_error.starts_with("Couldn't change the address book:"),
            "{favorite_error}"
        );
        assert!(
            announced(&app, &favorite_error),
            "the favorite refusal is announced"
        );

        // Recreate the stale projection to exercise the independently visible
        // delete-confirmation path against the same tombstone.
        std::rc::Rc::make_mut(&mut app.store)
            .contacts
            .push(contact.clone());
        app.selected = Some(contact.id);
        app.request(Request::DeleteSelected);
        if host_confirmation_is_up(&mut app) {
            confirm_on_host(&mut app);
        }
        app.settle();
        assert!(
            app.store.contact(contact.id).is_none(),
            "delete refusal reconciles"
        );
        let delete_error = toast(&app).unwrap().to_owned();
        assert!(
            delete_error.starts_with("Couldn't change the address book:"),
            "{delete_error}"
        );
        assert_ne!(delete_error, "Contact deleted", "no false success toast");
        assert!(
            announced(&app, &delete_error),
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
        set_title(&app, "Not mine to add");
        app.save_edit();
        app.settle();
        assert_eq!(app.mode, Mode::Edit, "the draft stays open");
        assert!(app.book().is_empty(), "nothing reached the document");
        let message = toast(&app).unwrap().to_owned();
        assert!(message.starts_with("Couldn't save contact:"), "{message}");
    }
}

// =================================================================
// Interaction harness — drives real winit events through the
// production `handle_event` path (not test-only shortcuts), so the
// assertions prove the shipped behaviour.
// =================================================================

use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey, SmolStr};

/// `text` is what a focused `TextInput` actually inserts.
fn key_input_event(logical_key: Key, text: Option<&str>) -> WindowEvent {
    // `Key`/`PhysicalKey`/`KeyLocation`/ `ElementState` first variant; `bool` → false) and we
    // overwrite the ones we read below; the private field is inert for our widgets.
    let ev =
        ui::synthetic_input::key_event(logical_key, text.map(SmolStr::new), ElementState::Pressed);
    ui::synthetic_input::keyboard_input(ev)
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
        self.dispatch(&ui::synthetic_input::cursor_moved(p.x, p.y));
    }

    /// Press `id` in the window's bar, as the desktop hands the press to
    /// the app.
    fn press_bar(&mut self, id: &str) {
        press_bar(&mut self.app, id);
    }

    fn press_left(&mut self, p: Point) -> bool {
        self.move_to(p);
        let press = self.dispatch(&ui::synthetic_input::mouse_press());
        let release = self.dispatch(&ui::synthetic_input::mouse_release());
        self.app.settle();
        press || release
    }

    fn press_right(&mut self, p: Point) -> bool {
        self.move_to(p);
        let handled = self.dispatch(&ui::synthetic_input::mouse_input(
            ElementState::Pressed,
            MouseButton::Right,
        ));
        self.app.settle();
        handled
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
        let handled = self.app.dispatch(event, mods);
        self.app.settle();
        handled
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
        self.app.harness_mut().type_text(s);
        self.app.settle();
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
        // The ordinary host applies the control's requested focus; raw
        // root dispatch deliberately only reports that request.
        self.app
            .harness_mut()
            .click_at(rect.center().x, rect.center().y);
        self.app.settle();
        form_widget(&self.app, slot).is_some_and(Widget::is_focused)
    }
}

/// Detail fields enter edit on double click only, focus scrolls below-the-fold fields into view, and every slot has a placeholder.
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
        .a11y_node_named("Edit Birthday")
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
    let first_rect = form_widget(&h.app, Slot::First).unwrap().get_rect();
    let column = h.app.shell.geometry().content_column;
    assert!(
        // Fractional layout rounds a field resting at the column's top a
        // few hundred-thousandths of a pixel either side.
        first_rect.y0 >= column.y0 - 0.01,
        "focus reveals the first field inside the content column: {first_rect:?} in {column:?}, scrolled {:?}",
        h.app.form.as_ref().unwrap().scroll.get()
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

/// Tab walks all retained controls in the window; reverse traversal wraps
/// through the same owners, and Enter progresses through the text fields.
#[test]
fn form_focus_moves_by_tab_shift_tab_and_enter() {
    form_tab_visits_and_wraps_every_control();
    form_enter_advances_and_submits();
    let mut h = edit_harness();
    act_named(&mut h.app, "First name", &ui::a11y::Action::Focus);
    h.app.harness_mut().key("Tab");
    h.app.harness_mut().set_modifiers(&["shift".to_owned()]);
    h.app.harness_mut().key("Tab");
    h.app.harness_mut().set_modifiers(&[]);
    h.app.settle();
    assert_eq!(h.focused_slot(), Some(Slot::First));
    h.type_text("!");
    assert_eq!(
        h.field_value(Slot::First),
        "Ada!",
        "returning through Tab preserves the caret at the field's end"
    );
}

fn form_tab_visits_and_wraps_every_control() {
    let mut h = edit_harness();
    assert_eq!(h.app.mode, Mode::Edit);
    act_named(&mut h.app, "First name", &ui::a11y::Action::Focus);
    let first = h
        .app
        .harness_mut()
        .focused()
        .expect("First name owns keyboard focus");
    let count = h.app.harness_mut().focus_order().len();
    assert!(
        count > h.app.form.as_ref().unwrap().fields.len(),
        "non-input controls participate in global focus order"
    );
    let mut forward = Vec::new();
    let mut labels = HashSet::new();
    for _ in 0..count {
        let node = h
            .app
            .harness_mut()
            .focused()
            .expect("Tab keeps a focus owner");
        forward.push(node);
        let access = h
            .app
            .harness_mut()
            .focused_access_node()
            .expect("focused control has semantics");
        if let Some(name) = access.name {
            labels.insert(name);
        }
        h.app.harness_mut().key("Tab");
        h.app.settle();
    }
    assert_eq!(
        h.app.harness_mut().focused(),
        Some(first),
        "Tab wraps to its starting owner"
    );
    assert_eq!(
        forward.iter().copied().collect::<HashSet<_>>().len(),
        count,
        "every eligible control is visited once"
    );
    for slot in form_slots(h.app.form.as_ref().unwrap()) {
        assert!(
            labels.contains(&crate::form::field_name(slot)),
            "Tab reaches {slot:?}"
        );
    }
    for control in [
        "Favorite",
        "Phone 1 type",
        "Email 1 type",
        "Remove phone 1",
        "Remove email 1",
        "Add phone",
        "Add email",
    ] {
        assert!(
            labels.contains(control),
            "Tab reaches the retained control {control}"
        );
    }
    for expected in forward.iter().rev() {
        h.app.harness_mut().set_modifiers(&["shift".to_owned()]);
        h.app.harness_mut().key("Tab");
        h.app.harness_mut().set_modifiers(&[]);
        h.app.settle();
        assert_eq!(
            h.app.harness_mut().focused(),
            Some(*expected),
            "Shift+Tab reverses the same window order, including wrap"
        );
    }
}

fn form_enter_advances_and_submits() {
    let mut h = edit_harness();
    assert!(
        h.press_named(NamedKey::Enter),
        "Enter starts an unfocused form"
    );
    h.app.settle();
    assert_eq!(h.focused_slot(), Some(Slot::First));
    let slots: Vec<_> = h
        .app
        .form
        .as_ref()
        .unwrap()
        .fields
        .iter()
        .map(|(slot, _)| *slot)
        .collect();
    for expected in slots.iter().skip(1) {
        h.app.harness_mut().key("Enter");
        h.app.settle();
        assert_eq!(
            h.focused_slot(),
            Some(*expected),
            "the shared input submits to the next draft field"
        );
        assert_eq!(h.app.mode, Mode::Edit, "progression does not save early");
    }
    h.app.harness_mut().key("Enter");
    h.app.settle();
    assert_eq!(
        h.app.mode,
        Mode::View,
        "Enter on the last field submits the form"
    );
    assert!(h.app.form.is_none(), "the edit form closes after Save");
    assert!(
        h.book()
            .iter()
            .any(|contact| contact.first == "Ada" && contact.last == "Byron"),
        "the canonical document owns the saved contact"
    );
}

/// The shared property publishes a measured website action and opens its URI.
#[test]
fn website_renders_and_opens_through_the_link_widget() {
    // --- phase: website_renders_via_the_link_widget ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.expect("a contact is selected");
        let website = h.app.store.contact(id).unwrap().website.clone();
        assert!(!website.is_empty());
        let (_, button) = h
            .app
            .harness_mut()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| {
                node.label() == Some("Open Website") && node.role() == ui::accesskit::Role::Button
            })
            .expect("the shared property offers its website action");
        assert!(
            button.bounds().is_some_and(|bounds| bounds.width() > 0.0),
            "the action publishes real pointer geometry"
        );
        assert!(
            h.app
                .harness_mut()
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.value() == Some(website.as_str())),
            "the website value remains readable"
        );
    }

    // --- phase: clicking_the_website_opens_it_through_the_link_widget ---
    {
        // Drive the whole click path through the shared property action and
        // confirm the contact's URL is what gets opened. A recording host
        // captures the URL instead of spawning the OS browser, so this exercises
        // the production click path hermetically — no xdg-open side effect.
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        // The link opens through background work; pump it so the open is seen.
        let work = ui::background::Pumped::new();
        let _work = work.background().enter();
        let mut h = Harness::live(1280, 800);
        let id = h.app.selected.unwrap();
        let url = h.app.store.contact(id).unwrap().website.clone();
        assert!(!url.is_empty(), "the selected seed has a website");

        h.relayout();
        let (_, action) = h
            .app
            .harness_mut()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| {
                node.label() == Some("Open Website") && node.role() == ui::accesskit::Role::Button
            })
            .expect("website action");
        let bounds = action.bounds().expect("published website action geometry");
        let rect = Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1);
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
            "the click opened the contact's website through the host URI service",
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
        // Click the "Delete" row in the menu: your own contact goes at once,
        // and its notice offers Undo.
        let rows = menu_rows(&h.app);
        let delete_idx = menu_index(&h.app, ContextAction::Delete);
        h.press_left(rows[&delete_idx].center());
        h.app.settle();
        assert!(
            !host_confirmation_is_up(&mut h.app),
            "context-menu Delete of your own contact asks nothing"
        );
        assert!(h.app.store.contact(id).is_none(), "the contact is deleted");
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
        let before = h.app.store.contact(id).unwrap().favorite;

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

        let rows = menu_rows(&h.app);
        h.press_left(rows[&fav_idx].center());

        assert_eq!(
            h.app.store.contact(id).unwrap().favorite,
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
        h.press_left(rows[&fav_idx].center());
        assert_eq!(
            fav_in_doc(h.book(), id),
            before,
            "second toggle restores the CRDT flag"
        );
    }
}

/// The person's own Delete does exactly what the palette's delete does,
/// without asking, and Undo leaves the document as it was.
#[test]
fn deleting_your_contact_is_the_palette_action_and_undo_takes_it_back() {
    let mut h = Harness::new(1280, 800);
    let id = h.app.selected.unwrap();
    let before = h.book();
    let expected = h.app.book_after(Contacts::delete(id));
    assert!(h.press_named(NamedKey::Delete));
    h.app.settle();
    assert!(!host_confirmation_is_up(&mut h.app), "nothing is asked");
    assert_eq!(
        h.book(),
        expected,
        "the Delete key does what the palette's Delete does",
    );
    assert!(
        !h.book().iter().any(|c| c.id == id),
        "and it actually deleted the contact",
    );
    press_notice_button(&mut h.app, "Undo");
    h.app.settle();
    assert_eq!(h.book(), before, "Undo leaves the book as it was");
    assert!(h.app.store.contact(id).is_some(), "the contact is back");
}

/// Favourite toggles dispatch the palette action, palette actions reconcile on sync, the detail star toggles store and doc and a toast expires after a real action.
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
        let before = h.app.store.contact(id).unwrap().favorite;
        let expected = h
            .app
            .book_after(Invoke::edit(&Contacts::favorite(id, !before)).unwrap());

        assert!(h.press_right(row));
        let rows = menu_rows(&h.app);
        let fav_idx = menu_index(&h.app, ContextAction::Favorite);
        h.press_left(rows[&fav_idx].center());
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
                &crate::resource::contact_resource_id(id),
                "delete-contact",
                serde_json::json!({}),
            )
            .expect_err("deleting asks for confirmation");
        h.app.app.invoke_confirmed(Contacts::delete(id)).unwrap();
        h.app.settle();
        assert!(
            h.app.store.contact(id).is_none(),
            "the presented book dropped the deleted contact"
        );
        assert_eq!(h.app.store.len(), before - 1);
    }

    // --- phase: detail_favorite_star_click_toggles_store_and_doc ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let before = h.app.store.contact(id).unwrap().favorite;
        assert!(
            bar_offers(BAR_FAVORITE),
            "the detail favorite star is laid out"
        );
        h.press_bar(BAR_FAVORITE);
        assert_eq!(
            h.app.store.contact(id).unwrap().favorite,
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
        let shown = ui::time::now();
        ui::time::testing::with_monotonic_time(shown, || {
            // A real action: creating a contact raises the confirmation (a
            // stored edit to an existing one says nothing).
            let mut app = Live::sample(1280, 800);
            let mut contact = Contact::new();
            contact.first = "Toasted".into();
            app.begin_edit(contact, true);
            app.request(Request::Save);
            // Said once the address book kept it, the pass after.
            app.app.settle();
            assert_eq!(
                toast(&app),
                Some("Contact created"),
                "creating a contact raises a toast"
            );

            let deadline = app
                .shell
                .next_wake_deadline()
                .expect("the production notice exposes its expiry deadline");
            let host_deadline = app
                .harness_mut()
                .frame_schedule()
                .next_wake_deadline
                .expect("the real host schedules the notice's dismissal without input");
            assert!(
                host_deadline <= deadline,
                "the host must wake no later than its owned notice's expiry"
            );
            assert!(deadline > shown, "a fresh notice has its full lifetime");
            app.harness_mut().relayout();
            assert!(
                toast(&app).is_some(),
                "a fresh toast survives an immediate frame"
            );

            // The animation clock does not advance the real record writer.
            // Await the production durability boundary before asking whether
            // the whole host, including its save-status feedback, is idle.
            app.app
                .flush()
                .expect("the actual delete reaches durable record storage");
            assert!(
                toast(&app).is_some(),
                "durable storage completion does not dismiss a fresh notice"
            );

            let expired = deadline + Duration::from_millis(50);
            ui::time::testing::with_monotonic_time(expired, || {
                assert!(
                    app.harness_mut().poll_frame().unwrap(),
                    "the due deadline drives the production frame without further input"
                );
                assert!(
                    toast(&app).is_none(),
                    "the toast auto-dismisses once its lifetime elapses; first host wake {:?}, shared notice lifetime {:?}",
                    deadline.duration_since(shown),
                    ui::widget::toast::NOTICE_LIFETIME
                );
            });
            // Allow the real host to finish any action animation; never force
            // a draw or mutate the notice to manufacture an idle result.
            for frame in 1..=100 {
                ui::time::testing::with_monotonic_time(
                    expired + Duration::from_millis(frame * 16),
                    || {
                        app.harness_mut().poll_frame().unwrap();
                    },
                );
            }
            // Inspect the settled host at the last polled instant: restoring
            // the outer clock first would move time backwards by 4.65 seconds.
            ui::time::testing::with_monotonic_time(expired + Duration::from_millis(1600), || {
                let idle = app.harness_mut().frame_schedule();
                let future_requesters = idle.next_wake_deadline.map(|at| {
                    ui::time::testing::with_monotonic_time(at, || {
                        app.harness_mut().frame_requesters()
                    })
                });
                assert!(
                    !idle.needs_animation_frame && idle.next_wake_deadline.is_none(),
                    "with the toast gone the loop can settle back to idle; schedule {:?}; requesters {:?}; future requesters {:?}; contact animation {}, deadline {:?}",
                    idle,
                    app.harness_mut().frame_requesters(),
                    future_requesters,
                    app.needs_animation_frame(),
                    app.next_wake_deadline()
                );
            });
        });
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
        let cur = h.app.selected.unwrap();
        let c = h.app.store.contact(cur).cloned().unwrap();
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
        let original_title = h.app.store.contact(cur).unwrap().title.clone();
        let c = h.app.store.contact(cur).cloned().unwrap();
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
        let c = h.app.store.contact(cur).cloned().unwrap();
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
        let c = h.app.store.contact(cur).cloned().unwrap();
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
        let original = h.app.store.contact(id).unwrap().title.clone();
        let c = h.app.store.contact(id).cloned().unwrap();
        h.app.begin_edit(c, false);
        h.relayout();
        // Focus the Title field via a real click, then type through the
        // production keystroke path (NOT `set_text`).
        assert!(
            h.focus_field(Slot::Title),
            "the title field focuses on click"
        );
        h.type_text("XYZ");
        let live = h.field_value(Slot::Title);
        assert!(
            live.contains("XYZ"),
            "the field shows the typed text: {live:?}"
        );
        assert_ne!(live, original);
        // … but NOTHING was committed: editing is a true draft now.
        assert_eq!(
            h.app.store.contact(id).unwrap().title,
            original,
            "store untouched mid-edit"
        );
        assert_eq!(
            h.doc_contact(id).unwrap().title,
            original,
            "CRDT doc untouched mid-edit"
        );
        assert!(h.app.form_is_dirty(), "the live-typed form reports dirty");
        assert!(bar_offers(BAR_CANCEL), "the Cancel button is laid out");
        h.press_bar(BAR_CANCEL);
        assert_eq!(h.app.mode, Mode::View, "Cancel leaves edit mode");
        assert!(h.app.form.is_none(), "Cancel drops the form");
        assert_eq!(
            h.app.store.contact(id).unwrap().title,
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
        h.press_bar(BAR_CANCEL);
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
            bar_offers(BAR_EDIT),
            "the read-view Edit button is laid out"
        );

        // Clicking Edit enters the form on the shown contact (identity carried
        // across the transition — the header's avatar + name reflect this draft).
        h.press_bar(BAR_EDIT);
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
            bar_offers(BAR_CANCEL),
            "the Cancel button is laid out in the form"
        );
        h.press_bar(BAR_CANCEL);
        assert_eq!(h.app.mode, Mode::View, "Cancel returns to the read view");
        assert!(h.app.form.is_none(), "no edit form lingers after Cancel");
        assert_eq!(h.app.selected, Some(sel), "the same contact stays selected");
    }

    // --- phase: typed_edit_saved_with_ctrl_s_commits_through_the_keystroke_path ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.contact(id).cloned().unwrap();
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
            h.app.store.contact(id).unwrap().org,
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
            h.app
                .harness_mut()
                .a11y_focus()
                .is_none_or(|(_, node)| node.label() != Some("Search")),
            "'/' does not steal focus mid-edit"
        );
        assert_eq!(h.app.mode, Mode::Edit, "still editing");

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
        app.begin_new_contact();
        assert!(
            !app.form_is_dirty(),
            "an untouched New Contact form is clean"
        );
        if let Some(form) = app.form.as_mut() {
            form.set_value(Slot::First, "Jane");
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
        people[0].action_id,
        crate::resource::contact_resource_id(grace),
        "the hit opens the contact"
    );
    assert!(providers[0].search("definitely no contact").is_empty());
    let request = ui::search::SearchRequest::this_app().with_query("hopper");
    h.app
        .harness_mut()
        .open_search(&request)
        .expect("the actual platform search opens");
    let (_, query) = h
        .app
        .harness_mut()
        .a11y_focus()
        .expect("platform query input is focused");
    assert!(
        query.role() == ui::accesskit::Role::TextInput,
        "the host owns the query input"
    );
    h.app.harness_mut().key("Escape");
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
            .contact(h.app.visible_ids()[0])
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
        assert!(
            h.app.harness_mut().a11y_nodes().iter().any(|(_, node)| {
                node.role() == ui::accesskit::Role::ComboBox
                    && node.label() == Some("Sort contacts")
                    && node.is_expanded() == Some(true)
            }),
            "the shared selector is expanded"
        );
        act_named(
            &mut h.app,
            "Sort contacts",
            &ui::a11y::Action::SetValue("Sort by organization".into()),
        );
        assert_eq!(h.app.sort, SortKey::Org, "choosing Org changes the sort");
        let first_by_org = h
            .app
            .store
            .contact(h.app.visible_ids()[0])
            .unwrap()
            .display_name();
        assert_ne!(first_by_org, first_by_name, "the list reordered");
        assert_eq!(
            first_by_org, "Ada Lovelace",
            "org sort leads with Analytical Engines"
        );
        // Choosing Name returns to the name order.
        h.press_left(target(&h.app, "contacts.sort").center());
        act_named(
            &mut h.app,
            "Sort contacts",
            &ui::a11y::Action::SetValue("Sort by last name".into()),
        );
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
                .contact(sel_before)
                .unwrap()
                .groups
                .iter()
                .any(|g| g.value.eq_ignore_ascii_case("Friends")),
        );
        act_named(&mut h.app, "Friends", &ui::a11y::Action::Activate);
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
                .contact(*id)
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

    // --- phase: group_capture_publishes_its_sidebar_destination ---
    {
        let mut app = Live::scenario("groups", 1280, 800);
        assert!(
            app.harness_mut().a11y_node_named("Friends").is_some(),
            "the capture publishes actual group navigation after layout"
        );
        let rows = app.sidebar_rows();
        assert!(rows.iter().any(|(_, label, _)| label == "Friends"));
    }
}

/// Arrow, n and e keys drive the list; the wheel scrolls it and large books virtualise while keeping selection in view.
#[test]
fn list_keyboard_and_scroll_navigation() {
    check_arrow_keys_move_the_list_selection();

    check_n_key_opens_new_and_e_key_edits_selection();

    check_mouse_wheel_scrolls_the_overflowing_contact_list();

    check_large_books_virtualize_rows_and_keep_keyboard_selection_in_view();
}

#[test]
fn split_divider_and_compact_toolbar_fit_their_space() {
    // --- phase: dragging_the_divider_resizes_the_split ---
    {
        let mut h = Harness::new(1280, 800);
        let before = h.app.shell.geometry().sidebar_column.width();
        let grab = h.app.shell.geometry().resize_handle.center();
        assert!(h.app.shell.geometry().resize_handle.width() > 0.0);
        h.move_to(grab);
        assert!(h.dispatch(&ui::synthetic_input::mouse_press()));
        h.move_to(Point::new(grab.x + 60.0, grab.y));
        h.relayout();
        assert!(
            h.app.shell.geometry().sidebar_column.width() > before,
            "dragging the shared sidebar grip widens navigation"
        );
        h.dispatch(&ui::synthetic_input::mouse_release());
        let released = h.app.shell.geometry().sidebar_column.width();
        h.move_to(Point::new(grab.x + 100.0, grab.y));
        h.relayout();
        assert_eq!(
            h.app.shell.geometry().sidebar_column.width(),
            released,
            "releasing ends the drag"
        );
    }

    // --- phase: compact_window_keeps_every_action_in_the_bar ---
    {
        let h = Harness::new(320, 640);
        let toolbar = ui::window_toolbar::declared().expect("the window declares its slots");
        let new = toolbar.actions.iter().position(|a| a.id == BAR_NEW);
        assert_eq!(new, Some(0), "New contact leads the bar's actions");
        assert!(
            toolbar
                .actions
                .iter()
                .filter(|a| a.id == BAR_IMPORT || a.id == BAR_EXPORT)
                .all(|a| a.menu),
            "file operations are in the bar's menu"
        );
        assert!(h.app.more_rows().contains(&(MoreAction::Import, true)));
        assert!(
            h.app.more_rows().contains(&(MoreAction::Export, true)),
            "file operations remain reachable through overflow"
        );
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
        let c = h.app.store.contact(id).cloned().unwrap();
        let base = c.phones.len();
        h.app.begin_edit(c, false);
        h.relayout();
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
        let rm = target(&h.app, &format!("contacts.form.remove.phone-{base}"));
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
        let reordered_groups = format!("{}, {}", contact.groups[1].value, contact.groups[0].value);
        let mut form = EditForm::new(contact, false);
        form.set_value(Slot::Phone(0), "+44 20 7946 0999");
        form.set_value(Slot::Groups, &reordered_groups);
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
        let c = h.app.store.contact(id).cloned().unwrap();
        let before = c.favorite;
        h.app.begin_edit(c, false);
        h.relayout();
        assert!(
            target(&h.app, "contacts.form.favorite").width() > 0.0,
            "the shared Favorite switch is laid out"
        );
        h.press_left(target(&h.app, "contacts.form.favorite").center());
        assert!(
            h.app.form_is_dirty(),
            "a changed Favorite is a cancellable local edit"
        );
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.favorite,
            !before,
            "the pill flips the draft flag"
        );
        assert_eq!(
            h.app.store.contact(id).unwrap().favorite,
            before,
            "draft-only: store unchanged until Save"
        );
    }
}

/// In the narrow detail pane of a half-screen window the phone's kind still
/// reads ("Mobile", not "…") and stands at its number's height.
#[test]
fn a_new_contacts_phone_kind_keeps_its_label_and_the_fields_height() {
    let mut h = Harness::new(960, 820);
    h.app.request(Request::NewContact);
    h.relayout();
    let kind = target(&h.app, "contacts.form.type.phone-0");
    let number = target(&h.app, "contacts.form.phone-0");
    assert!(
        kind.width() >= 80.0,
        "the kind keeps room for its label: {kind:?}"
    );
    assert!(
        (kind.height() - number.height()).abs() < 0.5,
        "kind {kind:?} and number {number:?} share a height"
    );
    assert!(kind.x1 <= number.x0, "{kind:?} sits before {number:?}");
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
        let c = h.app.store.contact(id).cloned().unwrap();
        assert!(!c.phones.is_empty());
        let stored_label = c.phones[0].label.clone();
        h.app.begin_edit(c, false);
        h.relayout();
        // Click the type chip for the first phone.
        let chip = target(&h.app, "contacts.form.type.phone-0");
        h.press_left(chip.center());
        assert!(
            h.app.harness_mut().a11y_nodes().iter().any(|(_, node)| {
                node.role() == ui::accesskit::Role::ComboBox
                    && node.label() == Some("Phone 1 type")
                    && node.is_expanded() == Some(true)
            }),
            "shared type selector opens"
        );
        let pick = TYPE_OPTIONS
            .iter()
            .position(|(value, _)| *value != stored_label)
            .unwrap();
        act_named(
            &mut h.app,
            "Phone 1 type",
            &ui::a11y::Action::SetValue(TYPE_OPTIONS[pick].1.into()),
        );
        h.app.form.as_mut().unwrap().collect();
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.phones[0].label,
            TYPE_OPTIONS[pick].0,
            "the draft label updated"
        );
        // Draft-only: the committed store still holds the old label.
        assert_eq!(
            h.app.store.contact(id).unwrap().phones[0].label,
            stored_label,
            "the label isn't committed until Save",
        );
    }

    // --- phase: calendar_day_click_sets_birthday_in_draft_only ---
    {
        let mut h = Harness::new(1280, 800);
        let id = h.app.selected.unwrap();
        let c = h.app.store.contact(id).cloned().unwrap();
        let stored_bday = c.birthday;
        h.app.begin_edit(c, false);
        h.relayout();
        act_named(&mut h.app, "Choose from calendar", &ui::a11y::Action::Focus);
        let button = target(&h.app, "contacts.form.calendar");
        assert!(button.width() > 0.0, "the calendar button is laid out");
        assert!(
            Rect::new(0.0, 0.0, 1280.0, 800.0).contains(button.center()),
            "the shared form reveals its calendar command before the pointer click"
        );
        h.press_left(button.center());
        assert!(h.app.form.as_ref().unwrap().calendar_open);
        let picker = named_widget(&*h.app, "Birthday calendar", Role::ComboBox)
            .expect("shared birthday calendar");
        assert!(picker.get_rect().area() > 0.0, "the calendar is laid out");
        act_named(
            &mut h.app,
            "Birthday calendar",
            &ui::a11y::Action::SetValue("1815-12-15".into()),
        );
        h.app.form.as_mut().unwrap().collect();
        let expected = Birthday::parse(&h.field_value(Slot::Birthday))
            .expect("calendar writes an ISO birthday");
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.birthday,
            Some(expected),
            "chosen date is a structured draft birthday"
        );
        // Draft-only: not committed to the store yet.
        assert_eq!(
            h.app.store.contact(id).unwrap().birthday,
            stored_bday,
            "birthday not committed until Save"
        );
    }

    // --- phase: popup_keyboard_selects_the_same_type_choice_as_a_pointer ---
    {
        let mut h = Harness::new(1_280, 800);
        let id = h.app.selected.expect("sample contact selected");
        let contact = h.app.store.contact(id).cloned().expect("stored contact");
        h.app.begin_edit(contact, false);
        h.relayout();
        let chip = target(&h.app, "contacts.form.type.phone-0");
        h.app
            .harness_mut()
            .click_at(chip.center().x, chip.center().y);
        h.app.settle();
        let (_, focused) = h
            .app
            .harness_mut()
            .a11y_focus()
            .expect("the actual host applies the pointer's focus request");
        assert_eq!(focused.role(), ui::accesskit::Role::ComboBox);
        assert_eq!(focused.label(), Some("Phone 1 type"));
        h.app.harness_mut().key("Down");
        h.app.harness_mut().key("Enter");
        h.app.settle();
        h.app.form.as_mut().unwrap().collect();
        assert_eq!(
            h.app.form.as_ref().unwrap().draft.phones[0].label,
            TYPE_OPTIONS[1].0,
            "keyboard activation uses the same draft choice as the pointer"
        );
        assert!(
            h.app.harness_mut().a11y_nodes().iter().any(|(_, node)| {
                node.role() == ui::accesskit::Role::ComboBox
                    && node.label() == Some("Phone 1 type")
                    && node.is_expanded() == Some(false)
            }),
            "activation dismisses the shared selector"
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
        h.app.choose_more(MoreAction::Export);
        h.relayout();
        assert!(
            h.app.export.is_some(),
            "Export opens the shared selection dialog"
        );
        let total = h.app.store.filtered_sorted("", None, h.app.sort).len();
        assert_eq!(
            h.app.export.as_ref().unwrap().selected.with(HashSet::len),
            total,
            "all visible contacts preselected"
        );
        assert!(h.press_left(target(&h.app, "contacts.export.none").center()));
        h.relayout();
        let checked = h
            .app
            .harness_mut()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.role() == ui::accesskit::Role::CheckBox)
            .expect("actual virtualized contact checkbox");
        assert_eq!(checked.1.toggled(), Some(ui::accesskit::Toggled::False));
        h.app
            .harness_mut()
            .perform_a11y_action(checked.0, &ui::a11y::Action::Activate)
            .expect("select one contact");
        h.app.settle();
        assert_eq!(
            h.app.export.as_ref().unwrap().selected.with(HashSet::len),
            1,
            "exactly one person is included"
        );
        let first_id = h
            .app
            .export
            .as_ref()
            .unwrap()
            .selected
            .with(|set| *set.iter().next().unwrap());
        assert!(
            h.app.store.contact(first_id).is_some(),
            "selection refers to a real contact"
        );
        act_named(
            &mut h.app,
            "Format",
            &ui::a11y::Action::SetValue("vCard 3.0, for older apps".into()),
        );
        let (mut services, recorder) = ui::host::HostServices::recording();
        let chooser = std::sync::Arc::new(DeferredPicker::new(recorder.clone()));
        services.files = chooser.clone();
        let _host = ui::host::enter(&services);
        act_named(&mut h.app, "Export", &ui::a11y::Action::Activate);
        assert!(
            h.app.export.is_none(),
            "confirming closes the selection dialog"
        );
        assert!(h.app.portal_in_flight(), "the export runs");

        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 1, "exactly one Save dialog was requested");
        assert_eq!(picks[0].title(), "Export contacts");
        assert!(!vcf.exists(), "no file exists while the chooser is open");
        chooser.choose(vcf.clone());

        // The file is written off the UI thread; the export ends on the
        // frame after.
        let deadline = tilo_test_env::hang_deadline();
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

fn column_dialog(widget: &dyn Widget) -> &ui::widget::ModalDialog<ui::widget::Column> {
    fn find(widget: &dyn Widget) -> Option<&ui::widget::ModalDialog<ui::widget::Column>> {
        if let Some(dialog) = widget.as_any().and_then(|value| value.downcast_ref()) {
            return Some(dialog);
        }
        widget.children().into_iter().find_map(find)
    }
    find(widget).expect("the shared column modal is actually mounted")
}

fn scratch_dir() -> PathBuf {
    scratch_book().0
}

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
    std::fs::create_dir_all(&dir).unwrap();
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

    let (mut services, recorder) = ui::host::HostServices::recording();
    let chooser = std::sync::Arc::new(DeferredPicker::new(recorder.clone()));
    services.files = chooser.clone();
    let _host = ui::host::enter(&services);
    ask(&mut app, Request::Dialog(DialogChoice::Primary));
    assert!(app.export.is_none(), "choosing Export closes the dialog");
    assert!(app.portal_in_flight());
    assert!(
        !vcf.exists(),
        "nothing is written until a destination is chosen"
    );
    let picks = recorder.pick_requests();
    assert_eq!(picks.len(), 1);
    assert_eq!(picks[0].title(), "Export contacts");
    chooser.choose(vcf.clone());
    let deadline = tilo_test_env::hang_deadline();
    while app.portal_in_flight() && ui::time::now() < deadline {
        app.settle();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!app.portal_in_flight(), "the real export finishes");
    let body = std::fs::read_to_string(&vcf).expect("the chosen file is written");
    assert_eq!(body.matches("BEGIN:VCARD").count(), 2);
    assert!(body.contains("VERSION:3.0"));
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

    // --- phase: first_run_state_is_actionable_by_pointer_keyboard_and_assistive_tech ---
    {
        let mut app = Live::empty(760, 620);
        assert!(app.store.is_empty());
        let bounds = named_widget(&*app, "No contacts yet", Role::Group)
            .expect("the actual first-run empty state is mounted")
            .synthetic_accessibility_rect(&[0])
            .expect("the shared empty state measures its virtual action");
        let action = app
            .harness_mut()
            .controls()
            .into_iter()
            .find(|control| {
                control.name.as_deref() == Some("New contact") && control.bounds == Some(bounds)
            })
            .expect("the actual first-run body action is exported");
        assert!(bounds.width() >= 44.0);
        assert!(Rect::new(0.0, 0.0, 760.0, 620.0).contains(bounds.center()));
        assert!(
            !app.more_rows().contains(&(MoreAction::Export, true)),
            "an empty book offers no meaningless export"
        );
        app.harness_mut()
            .operate_control(&action.id, &ui::headless::ControlInput::Focus)
            .unwrap();
        assert!(
            app.harness_mut()
                .controls()
                .iter()
                .any(|control| control.id == action.id && control.focused)
        );
        app.harness_mut()
            .operate_control(&action.id, &ui::headless::ControlInput::Press)
            .unwrap();
        app.settle();
        assert_eq!(app.mode, Mode::Edit);
        assert!(app.form.as_ref().is_some_and(|form| form.is_new));
    }
}

/// The on-disk book persists creations and deletions, merges with a
/// diverged replica, and imports real vCard files.
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
        app.invoke(Contacts::delete(removed));
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
    check_reimporting_an_export_recognises_its_uid();

    check_reimporting_the_same_uidless_card_is_idempotent();

    check_disconnected_uidless_imports_converge_on_one_contact_and_stable_rows();

    check_hostile_known_uid_is_reported_for_review_without_overwriting_contact();

    check_import_of_nothing_is_a_clean_noop();
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
        match toast(&h.app) {
            Some(msg) => assert!(msg.to_lowercase().contains("select")),
            None => panic!("an empty export must surface a guiding toast"),
        }
    }
}

/// An imported card that collides with a known identity is a decision:
/// keep the existing contact, or import it as a new one.
#[test]

fn duplicate_and_conflict_review_is_accessible() {
    check_import_conflict_decisions();
    check_responsive_duplicate_choices();
    check_duplicate_resource_and_compact_header();
}

fn check_import_conflict_decisions() {
    fn conflicting_app(w: u32, h: u32) -> (Live, Contact, Contact) {
        let original = sample_contacts().into_iter().next().unwrap();
        let mut incoming = original.clone();
        incoming.emails[0].value = "attacker@example.invalid".into();
        let mut app = Live::empty(w, h);
        let mut summary = ImportSummary::default();
        let mut first_new = None;
        app.absorb_import_contacts([original.clone()], &mut summary, &mut first_new);
        app.settle();
        app.absorb_import_contacts([incoming.clone()], &mut summary, &mut first_new);
        assert_eq!(summary.conflicts, 1);
        app.recompose();
        (app, original, incoming)
    }
    for (w, h, choice) in [(1280, 800, "Keep existing"), (560, 720, "Import as new")] {
        let (mut app, original, incoming) = conflicting_app(w, h);
        assert!(
            app.harness_mut()
                .a11y_nodes()
                .iter()
                .any(|(_, node)| format!("{:?}", node.role()) == "Dialog")
        );
        assert!(
            app.modal_capture().is_some(),
            "the review is actually painted"
        );
        for name in ["Keep existing", "Import as new"] {
            assert!(
                app.harness_mut()
                    .controls()
                    .iter()
                    .any(|control| control.name.as_deref() == Some(name)),
                "explicit decision {name}"
            );
        }
        act_named(&mut app, choice, &ui::a11y::Action::Activate);
        assert!(app.import_conflicts.is_empty());
        assert_eq!(
            app.store.contact(original.id).unwrap().emails[0].value,
            original.emails[0].value
        );
        if choice == "Keep existing" {
            assert_eq!(app.store.len(), 1);
        } else {
            assert_eq!(app.store.len(), 2);
            assert!(
                app.store
                    .contacts
                    .iter()
                    .any(|contact| contact.id != original.id
                        && contact.emails[0].value == incoming.emails[0].value)
            );
            assert_eq!(app.book().len(), 2, "Import as new reached the document");
        }
    }
}

fn check_responsive_duplicate_choices() {
    let mut app = Live::scenario("duplicate-review", 390, 844);
    app.harness_mut()
        .set_theme(Theme::dark().with_text_scale(2.0));
    app.recompose();
    let review = app.duplicate_review.as_ref().expect("review is open");
    assert_eq!(
        review.choices.len(),
        7,
        "every scalar conflict is exercised"
    );
    let modal = column_dialog(&*app);
    let body = modal.body_rect();
    assert!(
        modal.content().get_rect().height() > body.height(),
        "the full conflict content overflows its scrollable body"
    );
    assert!(
        modal.secondary_rect().y1 <= modal.primary_rect().y0,
        "footer actions stack at large text phone width"
    );
    assert!(
        app.harness_mut()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| format!("{:?}", node.role()) == "Dialog")
    );
    act_named(
        &mut app,
        "Organization to keep",
        &ui::a11y::Action::SetValue("Keep the other card's".into()),
    );
    assert!(
        app.duplicate_review
            .as_ref()
            .unwrap()
            .take_from_source()
            .contains(&crate::actions::MergeField::Organization)
    );
    act_named(
        &mut app,
        "Organization to keep",
        &ui::a11y::Action::SetValue("Keep this card's".into()),
    );
    assert!(
        !app.duplicate_review
            .as_ref()
            .unwrap()
            .take_from_source()
            .contains(&crate::actions::MergeField::Organization)
    );
}

fn check_duplicate_resource_and_compact_header() {
    let mut app = Live::scenario("duplicates", 1280, 800);
    let candidate = app
        .duplicate_candidate
        .clone()
        .expect("a shared email/name identifies a candidate");
    let published = app.app.agent_surface().to_string();
    assert!(
        published.contains("contacts.book/derived/duplicate:"),
        "the exact duplicate resource is published"
    );
    app.choose_more(MoreAction::ReviewDuplicate);
    let opened = app
        .duplicate_review
        .as_ref()
        .expect("canonical review opens");
    assert_eq!(opened.survivor.id, candidate.survivor_id);
    assert_eq!(opened.source.id, candidate.source_id);
    act_named(&mut app, "Cancel", &ui::a11y::Action::Activate);
    app.harness_mut()
        .set_theme(Theme::dark().with_text_scale(2.0));
    app.harness_mut().resize((390, 844)).unwrap();
    // Narrowing to one column slides the panes; measure the header where it
    // comes to rest instead of wherever the slide stands on this machine.
    app.shell.snap();
    app.recompose();
    let controls = app.harness_mut().controls();
    assert!(
        !controls
            .iter()
            .any(|control| control.name.as_deref() == Some("New contact")),
        "no header row of the window's own: {controls:?}"
    );
    assert!(bar_offers(BAR_NEW), "New contact is the window bar's");
}

/// Export and unsaved modals are focus-trapped and semantic; detail, editor and popup controls reach the durable action path.
#[test]

fn modals_and_editors_expose_semantic_action_routes() {
    export_modal_semantics();
    unsaved_modal_semantics();
    detail_and_editor_semantics();
    form_choice_semantics();
}

fn export_modal_semantics() {
    let mut app = Live::sample(900, 640);
    app.choose_more(MoreAction::Export);
    assert!(app.traps_focus(), "export traps focus behind its scrim");
    assert!(
        app.harness_mut()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| format!("{:?}", node.role()) == "Dialog")
    );
    act_named(&mut app, "Select none", &ui::a11y::Action::Activate);
    assert!(app.export.as_ref().unwrap().selected.get().is_empty());
    act_named(&mut app, "Select all", &ui::a11y::Action::Focus);
    assert!(
        app.harness_mut()
            .controls()
            .iter()
            .any(|control| control.name.as_deref() == Some("Select all") && control.focused),
        "focus remains inside export"
    );
    app.harness_mut().key("Space");
    app.settle();
    assert_eq!(
        app.export.as_ref().unwrap().selected.get().len(),
        app.store.len()
    );
    act_named(&mut app, "Select none", &ui::a11y::Action::Activate);
    let checkbox = app
        .harness_mut()
        .controls()
        .into_iter()
        .find(|control| format!("{:?}", control.role) == "CheckBox")
        .expect("the shared table exposes a real checkbox");
    app.harness_mut()
        .operate_control(&checkbox.id, &ui::headless::ControlInput::Toggle)
        .unwrap();
    app.settle();
    assert_eq!(
        app.export.as_ref().unwrap().selected.get().len(),
        1,
        "exactly one actual row was selected"
    );
    act_named(&mut app, "Cancel", &ui::a11y::Action::Activate);
    std::rc::Rc::make_mut(&mut app.store).contacts = (0..1000)
        .map(|index| Contact {
            id: uuid::Uuid::new_v4(),
            first: format!("Export {index:04}"),
            ..Contact::default()
        })
        .collect();
    app.publish();
    app.choose_more(MoreAction::Export);
    let mounted = app
        .harness_mut()
        .controls()
        .into_iter()
        .filter(|control| format!("{:?}", control.role) == "CheckBox")
        .count();
    assert!(
        mounted > 0 && mounted < app.store.len(),
        "the shared table virtualizes its actual semantic checkbox rows"
    );
}

fn unsaved_modal_semantics() {
    let mut app = Live::sample(1280, 800);
    let current = app.selected.unwrap();
    let contact = app.store.contact(current).cloned().unwrap();
    app.begin_edit(contact, false);
    app.recompose();
    act_named(
        &mut app,
        &crate::form::field_name(Slot::Title),
        &ui::a11y::Action::SetValue("Discard me".into()),
    );
    let other = app
        .visible_ids()
        .into_iter()
        .find(|id| *id != current)
        .unwrap();
    app.request_select(other);
    app.recompose();
    assert!(app.traps_focus(), "unsaved decision traps focus");
    assert!(
        app.harness_mut()
            .a11y_nodes()
            .iter()
            .any(|(_, node)| format!("{:?}", node.role()) == "Dialog")
    );
    act_named(&mut app, "Don't save", &ui::a11y::Action::Focus);
    assert!(
        app.harness_mut()
            .controls()
            .iter()
            .any(|control| control.name.as_deref() == Some("Don't save") && control.focused)
    );
    act_named(&mut app, "Don't save", &ui::a11y::Action::Activate);
    assert!(app.pending_switch.is_none());
    assert_eq!(app.selected, Some(other));
}

fn detail_and_editor_semantics() {
    let mut app = Live::sample(1280, 800);
    let id = app.selected.unwrap();
    let favorite = app.store.contact(id).unwrap().favorite;
    let star = ui::window_toolbar::declared()
        .and_then(|toolbar| toolbar.actions.into_iter().find(|a| a.id == BAR_FAVORITE))
        .expect("the window's bar offers Favorite");
    assert_eq!(star.on, favorite);
    press_bar(&mut app, BAR_FAVORITE);
    assert_eq!(
        app.store.contact(id).map(|contact| contact.favorite),
        Some(!favorite)
    );
    assert_eq!(
        app.book()
            .into_iter()
            .find(|contact| contact.id == id)
            .map(|contact| contact.favorite),
        Some(!favorite),
        "activation dispatches the canonical collaborative action"
    );
    let contact = app.store.contact(id).cloned().unwrap();
    app.begin_edit(contact, false);
    app.recompose();
    let first = crate::form::field_name(Slot::First);
    assert!(
        named_widget(&*app, &first, Role::TextInput).is_some(),
        "painted draft is an actual input"
    );
    let before = app.book();
    act_named(
        &mut app,
        &first,
        &ui::a11y::Action::SetValue("Augusta".into()),
    );
    assert_eq!(
        app.book(),
        before,
        "assistive typing remains a cancellable local draft"
    );
    // Save is the window bar's, as the desktop hands its press to the app.
    press_bar(&mut app, BAR_SAVE);
    assert_eq!(
        app.book()
            .into_iter()
            .find(|stored| stored.id == id)
            .map(|stored| stored.first),
        Some("Augusta".into()),
        "Save commits through ContactsAction"
    );
}

fn form_choice_semantics() {
    let mut app = Live::sample(1280, 800);
    let id = app.selected.unwrap();
    let contact = app.store.contact(id).cloned().unwrap();
    app.begin_edit(contact, false);
    app.recompose();
    let kind = TypeTarget::Phone(0);
    let name = format!("{} type", kind.spoken());
    act_named(&mut app, &name, &ui::a11y::Action::SetValue("Other".into()));
    assert_eq!(app.form.as_ref().unwrap().kind(kind), "other");
    let before = app.form.as_ref().unwrap().draft.phones.len();
    act_named(&mut app, "Remove phone 1", &ui::a11y::Action::Activate);
    assert_eq!(app.form.as_ref().unwrap().draft.phones.len(), before - 1);
    act_named(
        &mut app,
        "Choose from calendar",
        &ui::a11y::Action::Activate,
    );
    let date = ui::clock::today().format("%Y-%m-01").to_string();
    let calendar = app
        .harness_mut()
        .a11y_node_named("Birthday calendar")
        .expect("the actual calendar is accessible");
    app.harness_mut()
        .perform_a11y_action(calendar, &ui::a11y::Action::SetValue(date.clone()))
        .unwrap();
    app.settle();
    assert_eq!(
        app.form
            .as_ref()
            .unwrap()
            .draft
            .birthday
            .map(|birthday| birthday.iso()),
        Some(date),
        "calendar choice remains a cancellable draft"
    );
    press_bar(&mut app, BAR_CANCEL);
    act_named(&mut app, "Friends", &ui::a11y::Action::Activate);
    assert_eq!(app.group_filter.as_deref(), Some("Friends"));
    let controls = app.harness_mut().controls();
    let mut targets = std::collections::HashSet::new();
    for control in controls {
        assert!(
            targets.insert(control.id),
            "each actual semantic control has one identity"
        );
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
    /// Import `paths` as the portal would hand them back, and let the
    /// book present what the import added.
    fn import_paths(&mut self, paths: &[PathBuf]) -> ImportSummary {
        let summary = ContactsApp::import_paths(self, paths);
        self.settle();
        summary
    }

    /// The declared sample book in the production live host, at `w`x`h`.
    /// Its pose is presentation-only; actions keep their real admission route.
    fn sample(w: u32, h: u32) -> Self {
        Self::fixture(None, w, h)
    }

    fn live_sample(w: u32, h: u32) -> Self {
        Self::fixture(None, w, h)
    }

    fn scenario(scenario: &str, w: u32, h: u32) -> Self {
        Self::fixture(Some(scenario), w, h)
    }

    fn fixture(scenario: Option<&str>, w: u32, h: u32) -> Self {
        let mut app = ui::framework::testing::TestApp::<crate::ContactsShell>::new().unwrap();
        for invoke in <crate::ContactsShell as ui::framework::ResourceApp>::scenario(scenario) {
            app.invoke(invoke).unwrap();
        }
        app.settle();
        app.with_widget(|surface: &mut ContactsApp| surface.stage_capture(scenario))
            .expect("the live Contacts surface stages its declared fixture");
        Self::mount(app, (w, h))
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
        // The host opened the window at its default size; a viewport narrow
        // enough for one column at a time slides the columns into place.
        // Tests look at the window as it stands at `viewport`, once that
        // resize motion has ended.
        app.with_widget(|surface: &mut ContactsApp| surface.shell.snap())
            .expect("the contacts surface is hosted");
        app.harness().relayout();
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
        self.app.state().contacts.to_vec()
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
    fn invoke(&mut self, invoke: Invoke<Contacts>) {
        self.app.invoke_confirmed(invoke).unwrap();
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
    /// The address book as it would be after `action`, applied to a copy of
    /// the document as the palette's action of the same name applies it.
    fn book_after(&self, invoke: Invoke<Contacts>) -> Vec<Contact> {
        let mut peer = Replica::<Contacts>::from_doc(self.app.peer_document());
        peer.apply_confirmed(invoke)
            .expect("the palette's action applies");
        peer.state().unwrap().contacts.to_vec()
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

/// Form rows, pills, panel lifts, rows and toolbar faces are authored at 1x and grow with the text scale on every preset.
#[test]
fn duplicates_are_reviewed_field_by_field_before_merging() {
    let mut app = synced();
    let ada = selected_contact(&app);
    let mut copy = ada.clone();
    copy.id = uuid::Uuid::from_u128(ada.id.as_u128() + 0x100);
    copy.org = "Babbage & Co.".into();
    app.invoke(Contacts::add(&copy));
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
    assert!(
        app.duplicate_review.is_some(),
        "the review stays up behind the host's question"
    );
    confirm_on_host(&mut app);
    assert!(app.duplicate_review.is_none(), "a landed merge closes it");
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
}

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
        "import-conflict",
        "duplicate-review",
        "empty",
    ] {
        for (viewport, theme) in [((1280, 800), Theme::dark()), ((390, 844), Theme::light())] {
            let mut app = Live::scenario(scenario, viewport.0, viewport.1);
            app.harness_mut().set_theme(theme);
            app.recompose();
            assert!(
                !app.harness_mut().scene().encoding().is_empty(),
                "actual {scenario} capture paints"
            );
            match scenario {
                "new" | "edit" | "calendar" => assert_eq!(app.mode, Mode::Edit, "{scenario}"),
                "export" => assert!(app.export.is_some()),
                "duplicate-review" => assert!(app.duplicate_review.is_some()),
                "empty" => assert!(app.store.is_empty()),
                _ => {}
            }
        }
    }
}

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

fn check_save_round_trips_through_doc() {
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
    assert_eq!(toast(&app), None, "a stored edit says nothing");
}

fn check_refused_save_reconciles_and_announces_an_error_without_claiming_success() {
    let mut app = synced();
    let contact = selected_contact(&app);
    app.app
        .merge_peer_edit(|doc| peer_applies(doc, Contacts::delete(contact.id)));
    app.settle();
    app.begin_edit(contact.clone(), false);
    app.save_edit();

    assert_eq!(app.mode, Mode::Edit, "the rejected draft remains editable");
    assert!(app.form.is_some());
    assert!(
        app.store.contact(contact.id).is_none(),
        "store reconciles from the document"
    );
    let message = toast(&app).unwrap().to_owned();
    assert!(message.starts_with("Couldn't save contact:"));
    assert!(
        announced(&app, &message),
        "the refusal is announced through the live region"
    );
}

fn check_refused_favorite_and_delete_reconcile_without_false_success() {
    let mut app = synced();
    let contact = selected_contact(&app);
    app.app
        .merge_peer_edit(|doc| peer_applies(doc, Contacts::delete(contact.id)));
    app.settle();
    // A control can still have the last rendered contact when the peer's
    // tombstone reaches the door. Submit that real stale view request;
    // a helper looking up the already-reconciled store would do nothing.
    std::rc::Rc::make_mut(&mut app.store)
        .contacts
        .push(contact.clone());
    app.selected = Some(contact.id);
    app.request(Request::ToggleFavorite);
    assert!(
        !app.book().iter().any(|row| row.id == contact.id),
        "a tombstoned toggle is rejected"
    );
    assert!(
        app.store.contact(contact.id).is_none(),
        "favorite refusal reconciles"
    );
    assert!(
        toast(&app)
            .unwrap()
            .starts_with("Couldn't change the address book:")
    );

    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let ground = theme.tokens.colors.surfaces.base;
        let selected = ui::widget::ChipStyle::Live.colors(&theme, ground, true, false);
        assert!(
            ui::a11y::contrast_ratio(selected.ink, selected.fill) >= ON_FILL_MIN_CONTRAST,
            "selected chip label must remain readable on its resolved fill"
        );
        assert!(
            ui::a11y::contrast_ratio(selected.edge.unwrap_or(selected.fill), ground)
                >= ui::tokens::colors::NON_TEXT_MIN_CONTRAST,
            "the painted outline or filled chip boundary remains distinguishable from its pane"
        );
    }
}

fn check_seeds_sample_contacts_and_selects_one() {
    let mut app = sampled();
    let doc = AutoCommit::new();
    app.sync(&doc, &ROOT);
    assert_eq!(app.store.len(), 7);
    assert!(app.selected.is_some(), "a contact must be auto-selected");
    // The seeded selection points at a real, richly-populated record.
    let sel = app.store.contact(app.selected.unwrap()).unwrap();
    assert!(!sel.phones.is_empty());
}

fn check_view_mode_frame_does_not_panic() {
    let mut app = sampled();
    drive_frame(&mut app, 1280, 800);
}

fn check_edit_mode_with_repeated_fields_frame_does_not_panic() {
    let mut app = sampled();
    // Seed first.
    let doc = AutoCommit::new();
    app.sync(&doc, &ROOT);
    // Enter edit on the selected contact and add repeated fields.
    let c = app
        .selected
        .and_then(|id| app.store.contact(id))
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

fn check_calendar_and_type_popups_frame_does_not_panic() {
    let mut h = Harness::new(1280, 800);
    h.app.request(Request::EditSelected);
    h.app.request(Request::ToggleCalendar);
    h.relayout();
    assert!(h.app.form.as_ref().unwrap().calendar_open);
    let calendar = h.app.harness_mut().a11y_node_named("Birthday calendar");
    assert!(calendar.is_some(), "the shared date picker is hosted");
    let menu = target(&h.app, "contacts.form.type.phone-0");
    assert!(h.press_left(menu.center()));
    assert!(
        h.app.harness_mut().a11y_nodes().iter().any(|(_, node)| {
            node.role() == ui::accesskit::Role::ComboBox
                && node.label() == Some("Phone 1 type")
                && node.is_expanded() == Some(true)
        }),
        "the shared type selector opens"
    );
}

fn check_importing_state_is_visible_and_announced_as_busy() {
    let mut app = sampled();
    let work = app
        .begin_file_work(FileWork::Import)
        .expect("production import guard");
    drive_frame(&mut app, 1280, 800);
    let progress = named_widget(&app, "Importing contacts", Role::ProgressBar)
        .expect("the shared live progress indicator")
        .accessibility()
        .unwrap();
    assert!(progress.state.busy);
    assert!(!progress.state.focusable);
    assert!(progress.live.is_some(), "the busy label is announced");
    assert!(
        app.more_rows().contains(&(MoreAction::Import, false)),
        "the actual import action is unavailable while running"
    );
    assert!(
        app.begin_file_work(FileWork::Import).is_err(),
        "duplicate file work is refused"
    );
    drop(work);
    drive_frame(&mut app, 1280, 800);
    assert!(
        named_widget(&app, "Importing contacts", Role::ProgressBar).is_none(),
        "completion removes the running indicator"
    );
    assert!(app.more_rows().contains(&(MoreAction::Import, true)));
}

fn check_narrow_frame_resolves_single_pane_push() {
    let mut app = sampled();
    // A narrow viewport drives the single-pane master-detail push.
    drive_frame(&mut app, 480, 860);
    assert!(
        (!app.shell.geometry().side_by_side),
        "a narrow viewport resolves to single-pane push"
    );
    assert_eq!(
        app.shell.front_pane(),
        NavPane::Overview,
        "a fresh narrow frame opens to the list"
    );
}

fn check_narrow_selection_pushes_detail_then_back_returns_to_list() {
    let mut app = sampled();
    let doc = AutoCommit::new();
    app.sync(&doc, &ROOT);
    drive_frame(&mut app, 480, 860);
    assert!((!app.shell.geometry().side_by_side));
    // Snap the push to the detail pane (as a row selection would) and
    // confirm the pushed-detail frame renders without panicking.
    app.shell.show(NavPane::Content);
    app.shell.snap();
    assert_eq!(app.shell.front_pane(), NavPane::Content);
    drive_frame(&mut app, 480, 860);
    // The window's bar offers Back in the pushed pane.
    app.publish();
    assert!(bar_offers("nav-shell-back"), "pushed detail offers Back");
}

fn check_wide_frame_stays_docked_side_by_side() {
    let mut app = sampled();
    drive_frame(&mut app, 1280, 800);
    assert!(
        app.shell.geometry().side_by_side,
        "a desktop-width viewport keeps the docked split"
    );
    // The list pane sits left of the detail pane (classic split).
    assert!(
        app.shell.geometry().overview_column.x1 <= app.shell.geometry().content_column.x0 + 0.001
    );
}

fn check_narrow_row_press_pushes_detail_then_back_returns_to_list() {
    fn narrow_row_press_pushes_detail_then_back_returns_to_list() {
        let mut h = Harness::new(460, 880);
        assert!((!h.app.shell.geometry().side_by_side));
        assert_eq!(h.app.shell.front_pane(), NavPane::Overview);
        // Press a list row → the detail pane is pushed in.
        let row = h.row_center(1);
        assert!(h.press_left(row));
        assert_eq!(
            h.app.shell.front_pane(),
            NavPane::Content,
            "selecting pushes the detail in"
        );
        h.relayout();
        h.app.settle();
        // Press the window bar's Back → return to the list.
        h.press_bar("nav-shell-back");
        assert_eq!(
            h.app.shell.front_pane(),
            NavPane::Overview,
            "Back returns to the full-width list"
        );
    }
    narrow_row_press_pushes_detail_then_back_returns_to_list();
}

fn check_enter_opens_the_selected_contact_in_narrow_push_navigation() {
    let mut h = Harness::new(420, 800);
    assert!(
        (!h.app.shell.geometry().side_by_side),
        "phone width uses push navigation"
    );
    assert_eq!(h.app.shell.front_pane(), NavPane::Overview);

    assert!(
        h.press_named(NamedKey::Enter),
        "Enter opens the selected contact"
    );
    assert_eq!(h.app.shell.front_pane(), NavPane::Content);
}

fn check_resizing_wide_to_narrow_reflows_the_same_instance() {
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
        app.shell.geometry().overview_column.x1 <= app.shell.geometry().content_column.x0 + 0.001,
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

fn check_arrow_keys_move_the_list_selection() {
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

fn check_n_key_opens_new_and_e_key_edits_selection() {
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

fn check_mouse_wheel_scrolls_the_overflowing_contact_list() {
    // A short, wide window overflows the list so scrolling has an effect.
    let mut h = Harness::new(1000, 320);
    assert!(
        h.app.shell.geometry().side_by_side,
        "1000px stays the docked split"
    );
    let before = h.app.shared.borrow().painted.clone();
    h.move_to(h.app.shell.geometry().overview_column.center());
    h.dispatch(&ui::synthetic_input::mouse_wheel(0.0, -3.0));
    h.relayout();
    assert!(
        h.app.shared.borrow().painted != before,
        "scrolling down moves the list; clamp keeps it in range"
    );
}

fn check_large_books_virtualize_rows_and_keep_keyboard_selection_in_view() {
    // The CRDT/search model may legitimately hold an imported company
    // directory.  Layout and accessibility must scale with the viewport,
    // not with the directory's total cardinality.
    let mut h = Harness::new(900, 640);
    let directory: Vec<Contact> = (0..1_000)
        .map(|index| Contact {
            id: uuid::Uuid::new_v4(),
            first: format!("Directory {index:04}"),
            ..Contact::default()
        })
        .collect();
    h.app.app.merge_peer_edit(|doc| {
        let book = Replica::<Contacts>::from_doc(doc.fork()).state().unwrap();
        let invokes = book
            .contacts
            .iter()
            .map(|contact| Contacts::delete(contact.id))
            .chain([Contacts::add_all(directory.clone())]);
        peer_applies(doc, Invoke::group(invokes));
    });
    h.app.settle();
    h.app.selected = Some(directory[0].id);
    h.relayout();
    let row_height = h.app.shared.borrow().painted[0].1.height();
    let viewport_capacity = (list_bounds(&h.app).height() / row_height).ceil() as usize;
    assert_eq!(h.app.shared.borrow().rows.len(), 1_000);
    assert!(
        h.app.shared.borrow().painted.len() <= viewport_capacity + 1,
        "only the viewport and its boundary row are materialized"
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

    let nodes = h.app.harness_mut().a11y_nodes();
    let (_, list) = nodes
        .iter()
        .find(|(_, node)| node.role() == ui::accesskit::Role::List)
        .expect("contacts list is exported within the shared shell");
    assert_eq!(list.children().len(), h.app.shared.borrow().painted.len());
    assert!(
        list.children().len() < h.app.shared.borrow().rows.len(),
        "accessibility mirrors the bounded viewport rather than allocating every row"
    );
    let selected_row = nodes
        .iter()
        .find(|(id, node)| list.children().contains(id) && node.is_selected() == Some(true))
        .expect("selected row is in the semantic viewport")
        .1
        .clone();
    assert_eq!(selected_row.position_in_set(), Some(41));
    assert_eq!(selected_row.size_of_set(), Some(1_000));
}

fn check_reimporting_an_export_recognises_its_uid() {
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

fn check_reimporting_the_same_uidless_card_is_idempotent() {
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
    let second = app.import_paths(std::slice::from_ref(&vcf));

    assert_eq!(first.created, 1);
    assert_eq!(second.created, 0);
    assert_eq!(second.unchanged, 1);
    assert_eq!(app.store.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

fn check_disconnected_uidless_imports_converge_on_one_contact_and_stable_rows() {
    let card = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Grace Hopper\r\nN:Hopper;Grace;;;\r\nTEL;TYPE=work:+1 202 555 0143\r\nEMAIL:grace@example.com\r\nCATEGORIES:Pioneers\r\nEND:VCARD\r\n";
    let mut left = Replica::<Contacts>::new();
    let mut right = left.fork();
    let left_contact = vcard::parse_vcards(card).pop().unwrap();
    let right_contact = vcard::parse_vcards(card).pop().unwrap();
    assert_eq!(left_contact, right_contact);

    left.apply(Contacts::add(&left_contact)).unwrap();
    right.apply(Contacts::add(&right_contact)).unwrap();
    left.sync_with(&mut right).unwrap();

    let merged = left.state().unwrap().contacts.to_vec();
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

fn check_hostile_known_uid_is_reported_for_review_without_overwriting_contact() {
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
        app.store.contact(original.id).unwrap().emails[0].value,
        original.emails[0].value
    );
    assert!(toast(&app).is_some_and(|message| message.contains("review")));
    let _ = std::fs::remove_dir_all(&dir);
}

fn check_import_of_nothing_is_a_clean_noop() {
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
    assert!(
        toast(&app).is_none(),
        "a cancelled/empty import stays silent"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn narrow_sort_row_has_full_aligned_control_without_extra_gutters() {
    for scale in [1.0, 2.0] {
        let mut app = Live::sample(390, 844);
        app.harness_mut()
            .relayout_themed((390, 844), Theme::light().with_text_scale(scale))
            .unwrap();
        let row = target(&app, "contacts.sort-row");
        let control = target(&app, "contacts.sort");
        assert!(control.width() > 0.0 && control.height() >= 32.0);
        assert!(control.x0 >= row.x0 && control.x1 <= row.x1);
        assert!(control.y0 >= row.y0 && control.y1 <= row.y1);
        assert_eq!(control.x0, row.x0, "sort aligns with its overview rail");
        assert_eq!(
            control.y0 - row.y0,
            f64::from(Theme::light().tokens.spacing.s2),
            "the sort row is separated from the header divider"
        );
        assert!(control.x0 >= 0.0 && control.x1 <= 390.0);
        assert!(app.sort_choice.with(|value| value == "name"));
    }
}

/// A collaborator's `invoke`, written on their replica of `doc` and merged
/// back into it.
fn peer_applies(doc: &mut ui::tilo_resource::Doc, invoke: Invoke<Contacts>) {
    let mut peer = Replica::<Contacts>::from_doc(doc.fork());
    peer.apply_confirmed(invoke)
        .expect("the peer's change applies");
    tilo_automerge::ApplyIncoming::merge_incoming(doc, peer.doc_mut())
        .expect("the peer's change merges");
}

fn host_confirmation_is_up(app: &mut Live) -> bool {
    app.settle();
    app.harness_mut().asking().is_some()
}

/// Confirm the action the host asks about, as the person does: its
/// dialog's button named for the verb.
fn confirm_on_host(app: &mut Live) {
    assert!(
        host_confirmation_is_up(app),
        "the host asks to confirm the action"
    );
    assert!(app.harness_mut().confirm_asked().is_some());
    app.settle();
}

/// A surface that closes its own dialog in the same event its destructive
/// action asks the host to confirm: the host remembers where focus returns
/// from the window as it now stands, never from the dialog that is gone.
#[test]
fn closing_a_dialog_while_the_host_asks_to_confirm_is_safe() {
    let mut app = ui::framework::testing::TestApp::<crate::ContactsShell>::new().unwrap();
    let mut survivor = Contact::new();
    survivor.first = "Ada".into();
    survivor.id = uuid::Uuid::from_u128(1);
    let mut source = survivor.clone();
    source.id = uuid::Uuid::from_u128(2);
    source.org = "Analytical Engines".into();
    app.invoke(Contacts::add_all(vec![survivor.clone(), source.clone()]))
        .unwrap();
    app.effect_on(
        &crate::resource::place_resource_id(&crate::resource::duplicate_place(
            survivor.id,
            source.id,
        )),
        &crate::actions::ContactsEffect::OpenReview {
            survivor: survivor.id,
            source: source.id,
        },
    )
    .unwrap();
    app.settle();
    let merge = app.harness().a11y_node_named("Merge contacts").unwrap();
    app.harness()
        .perform_a11y_action(merge, &ui::a11y::Action::Focus)
        .unwrap();
    // In one event: the review closes, so its focused Merge button is
    // dropped, and the merge goes to the host, which asks to confirm.
    app.with_widget(|surface: &mut ContactsApp| {
        surface.duplicate_review = None;
        surface.dirty = true;
        surface
            .submit(Contacts::merge(survivor.id, source.id, []))
            .unwrap();
    });
    app.settle();
    assert!(app.asking().is_some(), "the host asks to confirm the merge");
}

/// A narrow window opens on the list, which stands alone: an empty book says
/// so there instead of showing nothing (its New contact is in the bar). Beside
/// the content, the content's first-run pane says it once.
#[test]
fn a_narrow_windows_empty_list_says_the_book_is_empty() {
    let mut narrow = Live::empty(460, 880);
    narrow.harness_mut().relayout();
    assert_eq!(narrow.shell.front_pane(), NavPane::Overview);
    assert_eq!(
        narrow.shared.borrow().empty_title.as_deref(),
        Some("No contacts yet"),
        "the list a narrow window opens on is not blank"
    );
    let mut wide = Live::empty(1280, 800);
    wide.harness_mut().relayout();
    assert_eq!(
        wide.shared.borrow().empty_title,
        None,
        "beside the first-run pane the list stays quiet"
    );
}

/// The node the accessibility tree holds under `name`, as the host exports it.
fn exported_node(app: &mut Live, name: &str) -> Option<ui::accesskit::Node> {
    app.settle();
    app.harness_mut()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name))
        .map(|(_, node)| node)
}

/// Writing the first contact replaces the content's first-run pane with the
/// form, so the list beside it says the book is empty and where the new
/// contact will appear instead of standing blank. Someone who may only view
/// is told so, and offered no New contact that would be refused.
#[test]
fn the_list_beside_the_first_contacts_form_says_the_book_is_empty() {
    let mut app = Live::empty(1280, 800);
    let first_run = exported_node(&mut app, "No contacts yet").expect("the first-run pane");
    assert_eq!(
        first_run.children().len(),
        1,
        "someone who may edit is offered New contact"
    );
    app.request(Request::NewContact);
    app.settle();
    assert_eq!(app.mode, Mode::Edit);
    assert!(app.form.as_ref().is_some_and(|form| form.is_new));
    let state = exported_node(&mut app, "No contacts yet")
        .expect("the list beside the form names the empty book");
    assert_eq!(state.role(), ui::accesskit::Role::Group);
    assert_eq!(
        state.description(),
        Some("The contact you're adding appears here once you save it.")
    );
    assert!(
        app.shared.borrow().painted.is_empty(),
        "no row stands where the empty state is drawn"
    );

    let mut viewer = Live::mount(
        ui::framework::testing::TestApp::with_role(ui::Role::Viewer).unwrap(),
        (1280, 800),
    );
    let state = exported_node(&mut viewer, "No contacts yet")
        .expect("an empty book is named to someone who may only view it");
    assert_eq!(state.description(), Some(VIEW_ONLY_MESSAGE));
    assert!(
        state.children().is_empty(),
        "the empty state offers no New contact to someone who may only view"
    );
    viewer.request(Request::NewContact);
    viewer.settle();
    let state = exported_node(&mut viewer, "No contacts yet")
        .expect("the list beside a viewer's form names the empty book");
    assert_eq!(state.description(), Some(VIEW_ONLY_MESSAGE));
}

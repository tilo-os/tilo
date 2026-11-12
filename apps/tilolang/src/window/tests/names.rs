//! Who a change reads as: an app that changed Notes' record is named by its own name in the
//! person's language — never by its principal id, and never by part of one.

use tilolang_tilo::runtime::{Principal, Value as RtValue};
use ui::component::RowSource as _;

use super::Workspace;

const NOTES: &str = "so.tilo.lang.notes";
/// The app whose call names notes in Notes' notebook (`morning_cleanup`).
const TODAY: &str = "so.tilo.lang.today";
/// What Today calls itself in German, from its package's catalogue.
const TODAY_IN_GERMAN: &str = include_str!("../../../../tl/today/locales/de-DE.ftl");

/// Today names the note the window shows, as the broker delivers the app's call: the change is
/// the app's, in Notes' record.
fn today_names_the_note(workspace: &mut Workspace) {
    let key = workspace
        .root_rows()
        .borrow()
        .key_at(0)
        .expect("the window shows a note");
    let note = workspace.item_of(&key);
    let host = workspace.window.current_host().expect("the host started");
    let retitle = host
        .package(NOTES)
        .expect("the notes package")
        .meta
        .actions
        .iter()
        .find(|action| action.name == "retitle")
        .expect("Notes declares `retitle`")
        .id
        .clone();
    host.runtime()
        .invoke_action(
            &Principal::new(format!("app:{TODAY}")),
            (
                retitle,
                vec![RtValue::resource(&note), RtValue::text("Morning: 2 done")],
            ),
        )
        .expect("Today names the note");
    workspace.window.commit_planned();
    workspace.settle();
    let host = workspace.window.current_host().expect("the host started");
    assert_eq!(
        host.runtime().read(&note, "title").expect("its title"),
        RtValue::text("Morning: 2 done"),
        "the app's call changed Notes' record"
    );
}

#[test]
fn a_note_another_app_named_reads_as_that_app_never_as_its_id() {
    let mut workspace = Workspace::seeded("notes");
    today_names_the_note(&mut workspace);
    let names = workspace.window.names.clone();
    assert_eq!(
        names.label(&format!("app:{TODAY}")),
        "Today",
        "the app that changed the record reads as its own name"
    );
}

#[test]
fn the_same_change_reads_as_the_app_s_own_name_in_german() {
    let mut workspace = Workspace::seeded("notes");
    today_names_the_note(&mut workspace);
    // The window a German person reads: it speaks German, and the words of the apps this device
    // has ride beside its own (an installed app's catalogue, `tilo_ui_host::i18n`).
    ui::i18n::I18nManager::install("en-US", "de-DE", &[("en-US", ""), ("de-DE", "")])
        .expect("the window speaks German");
    tilo_ui_host::i18n::add_catalogues(&[("de-DE".to_owned(), TODAY_IN_GERMAN.to_owned())]);
    let names = workspace.window.names.clone();
    assert_eq!(
        names.label(&format!("app:{TODAY}")),
        "Heute",
        "the app's own name in German"
    );
}

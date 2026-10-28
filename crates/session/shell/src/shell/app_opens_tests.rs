use super::*;

use crate::compositor::activation::{PersonInput, person_initiated};
use crate::shell::chrome_state::ChromeState;
use crate::shell::tab_binding::Placement;
use os_actions::{DisksRoute, TOOL_PROTOCOL_VERSION};

const MESSAGES: &str = "so.tilo.messages";
const NOTES: &str = "so.tilo.notes";
const CONTACTS: &str = "so.tilo.contacts";

/// A desktop with a Messages window and a Notes window in its active
/// workspace, each a client of its own: `messages` and `notes` are the
/// client indexes `PersonInput` records their presses under. Each test passes
/// its own, since the record is the session's.
fn desktop(messages: u32, notes: u32) -> (Chrome, Caller) {
    let mut tree = WorkspaceTree::bootstrap_demo();
    for (app, client) in [(MESSAGES, messages), (NOTES, notes)] {
        let window = Tab {
            id: crate::shell::state::next_id(),
            title: app.into(),
            app_title: None,
            tile: TileContent::Foreign {
                app_id: app.into(),
                surface_id: u64::from(client) << 32 | 1,
            },
        };
        crate::shell::tab_binding::place_tab(&mut tree, window, Placement::NewGroup);
    }
    let workspace = tree.desktop.active_workspace().unwrap().room().to_string();
    (
        ChromeState::new(tree),
        Caller {
            package: MESSAGES.into(),
            workspace,
        },
    )
}

fn request(caller: &Caller, intent: ToolIntent, on_behalf_of: Option<String>) -> ToolRequest {
    ToolRequest {
        protocol: TOOL_PROTOCOL_VERSION,
        workspace_id: caller.workspace.clone(),
        intent,
        on_behalf_of,
    }
}

fn contacts() -> ToolIntent {
    ToolIntent::OpenApplication {
        app: CONTACTS.into(),
    }
}

fn disks() -> ToolIntent {
    ToolIntent::Disks {
        route: DisksRoute::Overview,
    }
}

fn press(client: u32) {
    PersonInput::session().note(client, std::time::Instant::now());
}

/// What `request` from `caller` is decided as on `chrome`'s desktop with no
/// broker: only the person's press can be behind it.
fn decided(
    chrome: &Chrome,
    caller: &Caller,
    request: &ToolRequest,
    target: &str,
) -> Option<Authority> {
    authorize(
        Some(caller),
        request,
        target,
        |package| person_initiated(&chrome.borrow().tree(), package),
        |_, _, _| Err("no broker".into()),
    )
}

// An app opens another app or a system tool for the person's press in one
// of its own windows, the one press used up by it. Nothing behind it, or a
// press in another app's window, opens nothing.
#[test]
fn a_press_in_the_callers_window_opens_and_one_elsewhere_does_not() {
    let (chrome, caller) = desktop(0x7a01, 0x7a02);
    let open_contacts = request(&caller, contacts(), None);
    let open_disks = request(&caller, disks(), None);

    let unpressed = decided(&chrome, &caller, &open_contacts, CONTACTS);
    press(0x7a02);
    let pressed_in_notes = decided(&chrome, &caller, &open_contacts, CONTACTS);
    press(0x7a01);
    let pressed_in_messages = decided(&chrome, &caller, &open_contacts, CONTACTS);
    let again = decided(&chrome, &caller, &open_disks, "so.tilo.disks");
    press(0x7a01);
    let tool = decided(&chrome, &caller, &open_disks, "so.tilo.disks");

    assert_eq!(unpressed, None, "no press, no open");
    assert_eq!(
        pressed_in_notes, None,
        "a press in Notes opens nothing for Messages"
    );
    assert_eq!(pressed_in_messages, Some(Authority::Press));
    assert_eq!(again, None, "one press opens one thing");
    assert_eq!(tool, Some(Authority::Press), "a system tool alike");
}

// An open made for a delivered invocation is the broker's alone to admit:
// the shell neither lets the person's press stand in for a refused one, nor
// uses that press up. A malformed id names nothing.
#[test]
fn an_open_for_an_invocation_is_the_brokers_to_admit() {
    let (chrome, caller) = desktop(0x7b01, 0x7b02);
    let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(0x0c5a_1000));
    let named = Some(os_actions::invocation_hex(invocation.0.into_bytes()));
    let for_agent = request(&caller, contacts(), named);
    let admitted = std::cell::RefCell::new(Vec::new());
    let admit = |verdict: Result<AppOpen, String>| {
        let admitted = &admitted;
        move |caller: &Caller, id: tilo_actions::InvocationId, target: &str| {
            admitted
                .borrow_mut()
                .push((caller.package.clone(), id, target.to_owned()));
            verdict
        }
    };
    let pressed = std::cell::Cell::new(false);
    let take_press = |package: &str| {
        pressed.set(true);
        person_initiated(&chrome.borrow().tree(), package)
    };

    let opened = authorize(
        Some(&caller),
        &for_agent,
        CONTACTS,
        take_press,
        admit(Ok(AppOpen::Admitted)),
    );
    press(0x7b01);
    let dead = authorize(
        Some(&caller),
        &for_agent,
        CONTACTS,
        take_press,
        admit(Err("the invocation the open is for is not running".into())),
    );
    let malformed = authorize(
        Some(&caller),
        &request(&caller, contacts(), Some("not-an-invocation".into())),
        CONTACTS,
        take_press,
        admit(Ok(AppOpen::Admitted)),
    );

    assert_eq!(opened, Some(Authority::Invocation));
    assert_eq!(dead, None, "a refused invocation is not rescued by a press");
    assert_eq!(malformed, None);
    assert!(!pressed.get(), "an invocation's open never reads the press");
    assert!(
        person_initiated(&chrome.borrow().tree(), MESSAGES),
        "the person's press is still theirs to use"
    );
    assert_eq!(
        *admitted.borrow(),
        vec![
            (MESSAGES.to_owned(), invocation, CONTACTS.to_owned()),
            (MESSAGES.to_owned(), invocation, CONTACTS.to_owned()),
        ],
        "the broker is asked for the verified caller, the invocation and the target"
    );
}

// A button in the app ran its own published action, and the open names
// that invocation: it is the app's, so only the person's press opens for it.
#[test]
fn an_open_for_the_apps_own_invocation_needs_the_press() {
    let (chrome, caller) = desktop(0x7e01, 0x7e02);
    let own = tilo_actions::InvocationId(uuid::Uuid::from_u128(0x0c5a_2000));
    let named = request(
        &caller,
        contacts(),
        Some(os_actions::invocation_hex(own.0.into_bytes())),
    );
    let decide = || {
        authorize(
            Some(&caller),
            &named,
            CONTACTS,
            |package| person_initiated(&chrome.borrow().tree(), package),
            |_, _, _| Ok(AppOpen::NeedsPress),
        )
    };

    let unpressed = decide();
    press(0x7e02);
    let pressed_in_notes = decide();
    press(0x7e01);
    let pressed = decide();

    assert_eq!(unpressed, None);
    assert_eq!(pressed_in_notes, None);
    assert_eq!(pressed, Some(Authority::Press));
}

// The caller's workspace is the one its launch recorded; a request naming
// another, or from a process the shell did not launch, opens nothing.
#[test]
fn a_request_for_another_workspace_or_from_no_launched_app_is_refused() {
    let (chrome, caller) = desktop(0x7c01, 0x7c02);
    press(0x7c01);
    let mut elsewhere = request(&caller, contacts(), None);
    elsewhere.workspace_id = "another-workspace".into();

    let other_workspace = decided(&chrome, &caller, &elsewhere, CONTACTS);
    let unlaunched = authorize(
        None,
        &request(&caller, contacts(), None),
        CONTACTS,
        |_| true,
        |_, _, _| Ok(AppOpen::Admitted),
    );

    assert_eq!(other_workspace, None);
    assert_eq!(unlaunched, None);
}

// An authorized open of an app this computer doesn't have becomes the
// shell's Get card when the catalogue offers it, beside the asking app's
// window, and a notice naming only the asking app when nothing offers it.
// The app is answered the same for both and for an installed app: what
// became of its open is the person's to see.
#[test]
fn a_missing_app_is_shown_to_the_person_and_answered_like_an_installed_one() {
    let (chrome, caller) = desktop(0x7d01, 0x7d02);
    let catalogue = || {
        vec![crate::shell::catalogue::CatalogueApp {
            package_id: CONTACTS.into(),
            display_name: "Contacts".into(),
            ..crate::shell::catalogue::CatalogueApp::default()
        }]
    };
    let unknown = ToolIntent::OpenApplication {
        app: "org.example.nowhere".into(),
    };

    let installed_app = resolve(&contacts(), |_| true, catalogue);
    let tool = resolve(&disks(), |_| false, catalogue);
    let offered = resolve(&contacts(), |_| false, catalogue);
    let nowhere = resolve(&unknown, |_| false, catalogue);
    let notices_before = chrome.borrow().notifications().len();
    show(&chrome, &caller, CONTACTS, &offered);
    show(&chrome, &caller, "org.example.nowhere", &nowhere);

    assert_eq!(installed_app, Resolution::Launch);
    assert_eq!(tool, Resolution::Launch);
    assert_eq!(offered, Resolution::Offer("Contacts".into()));
    assert_eq!(nowhere, Resolution::Unavailable);
    let (notices, notice) = {
        let state = chrome.borrow();
        let notices = state.notifications();
        (notices.len(), format!("{:?}", notices.last().unwrap()))
    };
    let (card, beside) = {
        let tree = chrome.borrow().tree().clone();
        let workspace = tree.desktop.active_workspace().unwrap();
        let messages = crate::shell::placement::group_showing(workspace, MESSAGES).unwrap();
        let groups: Vec<_> = workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .collect();
        let card = groups.iter().position(|group| {
            group.tabs.iter().any(|tab| {
                matches!(&tab.tile, TileContent::LocalApp { app_id, note, .. }
                    if app_id == CONTACTS && note == crate::shell::peer_windows::NOT_INSTALLED)
            })
        });
        let beside = groups.iter().position(|group| group.id == messages);
        (card, beside)
    };
    let card = card.expect("the Get card for Contacts is on the desktop");
    let beside = beside.unwrap();
    assert_eq!(card, beside + 1, "right of the Messages window");
    assert_eq!(notices, notices_before + 1);
    assert!(
        !notice.contains("org.example.nowhere"),
        "the notice carries nothing the request did: {notice}"
    );
}

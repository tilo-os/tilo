//! A view of an app installed on the device: the daemon starts an instance of the app with no
//! session, and the instance's window, a sandboxed tilolang process on the signed runtimes the
//! app names, lays the view out for whoever opened it. Nothing here stands in for the app, its
//! runtimes or the sandbox: they are the ones a person's computer starts.

#![cfg(unix)]

use tilo_server_protocol::page::window::{WindowAsk, WindowLink};
use tilo_server_protocol::page::{PageAction, PageAddress};
use tilo_workspace_service::app_serving::PageAct;

use crate::language::host;

/// The app the views show.
const NOTES: &str = "so.tilo.notes";

/// A member opens a view of Notes on the device and its window answers the page whole: the
/// app's own name over what the app lays out.
#[test]
fn a_view_of_an_installed_app_is_laid_out_by_its_own_window() {
    host::run_with(&["notes"], |host| async move {
        let (daemon, caller, workspace) =
            (host.daemon(), host.caller(), host.workspace().to_owned());
        let (title, css, body) = tokio::task::spawn_blocking(move || {
            let link = daemon
                .open_view(&caller, NOTES, &workspace, None, None)
                .expect("the view opens");
            WindowLink::over(link).open(None)
        })
        .await
        .unwrap()
        .expect("its window answers the page");
        assert_eq!(title, "Notes", "{body}");
        assert!(!css.is_empty(), "the theme's stylesheet comes with it");
        assert!(body.contains('<'), "the body is the view laid out: {body}");
    });
}

/// A page that takes a phone's width tells its window so through the daemon, and the view lays
/// itself out again at that size, as a narrow window does; the same size told again changes
/// nothing.
#[test]
fn a_page_that_narrows_lays_its_view_out_again_through_the_daemon() {
    host::run_with(&["notes"], |host| async move {
        let (daemon, caller, workspace) =
            (host.daemon(), host.caller(), host.workspace().to_owned());
        let (narrowed, again) = tokio::task::spawn_blocking(move || {
            let link = daemon
                .open_view(&caller, NOTES, &workspace, None, None)
                .expect("the view opens");
            let mut window = WindowLink::over(link);
            window.open(None).expect("its window answers the page");
            let resize = || WindowAsk::Act {
                id: 0,
                target: None,
                action: PageAction::Resize {
                    width: 390,
                    height: 844,
                },
                signed: None,
                invoker: None,
            };
            let narrowed = window.act(&resize()).expect("the window takes the size");
            let again = window.act(&resize()).expect("the window takes it again");
            (narrowed, again)
        })
        .await
        .unwrap();
        assert!(
            !narrowed.is_empty(),
            "the view laid out at a phone's width changes the page"
        );
        assert!(
            again.is_empty(),
            "the same size again changes nothing: {again:?}"
        );
    });
}

/// A caller admitted to another workspace opens no view of this one.
#[test]
fn a_caller_of_another_workspace_opens_no_view() {
    host::run(|host| async move {
        let caller =
            tilo_actions::remote::Caller::local_person(host.person()).in_workspace("elsewhere");
        let refused = host
            .daemon()
            .open_view(&caller, NOTES, host.workspace(), None, None)
            .expect_err("a caller admitted elsewhere is refused");
        assert!(refused.contains("another workspace"), "{refused}");
    });
}

/// A member opens the page of an installed tilolang app (`/w/<workspace>/calculator`) on the
/// daemon's page path, as their browser's page session asks it: the app's own window lays the
/// view out and the page shows it, and a press on one of its keys comes back as what changed.
#[test]
fn a_page_of_an_installed_app_is_drawn_by_its_own_window() {
    host::run_with(&["calculator"], |host| async move {
        let (daemon, caller, workspace) =
            (host.daemon(), host.caller(), host.workspace().to_owned());
        let (opened, pressed) = tokio::task::spawn_blocking(move || {
            let pages = daemon
                .pages()
                .expect("the daemon serves its workspace's pages");
            let workspace_id =
                tilo_desktop_model::WorkspaceId::parse(&workspace).expect("a workspace id");
            let opened = settled(pages.open(
                workspace_id,
                &caller,
                PageAddress {
                    workspace,
                    app: "calculator".to_owned(),
                    show: None,
                },
            ))
            .expect("the calculator's page opens");
            let seven = button_labelled(&opened.body, "7")
                .unwrap_or_else(|| panic!("the page shows the calculator's keys: {}", opened.body));
            let pressed = settled(pages.act(
                workspace_id,
                opened.page,
                Some(PageAct {
                    id: seven,
                    target: None,
                    action: PageAction::Activate,
                    signed: None,
                }),
            ))
            .expect("the press is taken");
            pages.close(workspace_id, opened.page);
            (opened, pressed)
        })
        .await
        .unwrap();
        assert_eq!(opened.title, "Calculator", "{}", opened.body);
        assert!(
            !opened.css.is_empty(),
            "the theme's stylesheet comes with it"
        );
        assert!(
            pressed
                .iter()
                .any(|patch| patch.html().is_some_and(|html| html.contains('7'))),
            "the display shows the key pressed: {pressed:?}"
        );
    });
}

/// What `answer` settles to, waited for as a page's session waits.
fn settled<T>(answer: tilo_actions::remote::Pending<T>) -> T {
    answer
        .wait_timeout(std::time::Duration::from_secs(60))
        .ok()
        .flatten()
        .expect("the page answers")
}

/// The element number (`data-p`) of the button on `body` whose label is `label`.
fn button_labelled(body: &str, label: &str) -> Option<u32> {
    body.split("<button").skip(1).find_map(|button| {
        let (tag, rest) = button.split_once('>')?;
        let text = rest.split("</button>").next().unwrap_or_default();
        let attribute = |name: &str| {
            let (_, value) = tag.split_once(&format!("{name}=\""))?;
            value.split('"').next()
        };
        let named = attribute("aria-label") == Some(label) || text.trim() == label;
        named.then(|| attribute("data-p")?.parse().ok()).flatten()
    })
}

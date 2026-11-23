//! A view of an app installed on the device: the daemon starts an instance of the app with no
//! session, and the instance's window, a sandboxed tilolang process on the signed runtimes the
//! app names, lays the view out for whoever opened it. Nothing here stands in for the app, its
//! runtimes or the sandbox: they are the ones a person's computer starts.

#![cfg(target_os = "linux")]

use tilo_server_protocol::page::window::WindowLink;

use crate::host;

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

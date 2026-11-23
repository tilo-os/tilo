//! The system's copy link on a package's resources: every resource that shows a view offers it,
//! and the host answers it with a URL that names the resource again, under the workspace's page.

use crate::fixtures::{Replica, Workspace};
use serde_json::Map;
use tilo_reference::link::{Link, WebEntries, WebEntry};
use tilo_service::{Service, Started};
use tilolang_tilo::resource_service::ResourceService;
use tilolang_tilo::share::LinkHome;

const NOTES_APP: &str = "so.tilo.notes";
const WORKSPACE: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";
const SITE: &str = "https://tilo.so/";
const ORIGIN: &str = "https://ada.tilo.cloud";
const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);

/// Ada's notebook, published by the session's service, with the workspace's page to mint links
/// under.
fn service() -> ResourceService {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[DISCUSSION, NOTES]);
    ada.host.root_of(NOTES_APP).unwrap();
    ada.commit();
    let mut service = ada.into_service("ada");
    service.set_link_home(Some(LinkHome {
        origin: ORIGIN.to_owned(),
        workspace: WORKSPACE.to_owned(),
        host_device: "ab".repeat(32),
        web_entry: SITE.to_owned(),
    }));
    service
}

/// A new note, made through the app's own action on the published notebook.
fn new_note(service: &mut ResourceService) -> String {
    let notebook = service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{NOTES_APP}.Notebook"))
        .expect("the notebook is published");
    let Started::Done(output) = service
        .invoke(notebook.id.as_str(), "new_note", &Map::new())
        .unwrap()
    else {
        panic!("a tilolang action is done once committed");
    };
    service.settle();
    output.resources.first().cloned().expect("the note it made")
}

/// The link `url` names, read back with the entry the workspace serves its page under.
fn link(url: &str) -> Link {
    let entries = WebEntries::new(vec![WebEntry::new(SITE).unwrap()]);
    Link::parse(url, &entries).expect("the link the host minted reads back")
}

tilo_test_env::scenario! {
    fn every_resource_with_a_view_offers_its_link_and_the_host_mints_it;

    phase a_note_offers_the_systems_copy_link_among_its_own_actions;
    {
        let mut service = service();
        let note = new_note(&mut service);
        let published = service
            .resources()
            .into_iter()
            .flat_map(|resource| std::iter::once(resource.clone()).chain(resource.children))
            .find(|resource| resource.id.as_str() == note)
            .expect("the note is published");
        assert!(
            published.views.iter().any(|view| view.primary),
            "the note shows a view, which is what a link opens: {:?}",
            published.views
        );
        assert!(
            published.actions.iter().any(|spec| spec.id == "share-link"),
            "the palette's copy link is offered"
        );
        assert!(
            !published
                .actions
                .iter()
                .any(|spec| spec.id == "share-invite-link"),
            "an invitation is the person's own shell's to make, not an app's"
        );
    }

    phase copying_the_link_answers_a_url_that_names_the_note_again;
    {
        let mut service = service();
        let note = new_note(&mut service);
        let Started::Done(output) = service
            .invoke(&note, "share-link", &Map::new())
            .expect("the host mints the link")
        else {
            panic!("the link is answered at once");
        };
        let url = output
            .value
            .as_ref()
            .and_then(|value| value.get("url"))
            .and_then(serde_json::Value::as_str)
            .expect("an answer carries its link")
            .to_owned();
        let read = link(&url);
        assert_eq!(
            read.target.app.as_ref().map(tilo_reference::link::AppId::as_str),
            Some(NOTES_APP)
        );
        let names = read.target.resource.as_ref().expect("the link names the note");
        assert_eq!(names.kind, format!("tl.{NOTES_APP}.Note"));
        assert_eq!(names.id, note);
        assert_eq!(names.span, None, "a whole note, not a range of it");
        assert_eq!(
            read.target.view, None,
            "it opens through the app's own `open`, which shows the note"
        );
    }

    phase a_host_that_serves_no_page_refuses_the_link_with_the_systems_words;
    {
        let mut service = service();
        let note = new_note(&mut service);
        service.set_link_home(None);
        let refused = service
            .invoke(&note, "share-link", &Map::new())
            .expect_err("a host with no page makes no link");
        assert!(
            refused.to_string().contains("serves no web page"),
            "the system's own words: {refused}"
        );
    }
}

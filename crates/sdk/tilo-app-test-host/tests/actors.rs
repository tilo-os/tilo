//! Every replica of a record writes under an Automerge actor of its own. Two
//! windows sharing one would number their changes from the same sequence,
//! and a peer would drop the second window's changes as duplicates.
//! Each window that opens a record on a device, the window's own record or
//! the workspace's, which every window of an app opens, writes as a copy of
//! its own that the device's part of its writer names as the device's: what
//! the person did in an earlier window there is still told apart as theirs.

use tilo_app::prelude::*;
use tilo_app_test_host::TestApp;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shopping {
    pub items: Collection<Entry>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Entry {
    pub text: String,
}

#[resource(kind = "test.shopping", catalogue = "tests/locales")]
impl Shopping {
    /// Add an entry.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, text: String) {
        self.items.push(cx.id(), Entry { text });
    }
}

struct ShoppingApp;

impl ResourceApp for ShoppingApp {
    type Resource = Shopping;
    const ID: &'static str = "so.tilo.test-shopping";
    const TITLE: &'static str = "Shopping";

    fn view(cx: &mut ViewCx<Shopping>) -> Element {
        Box::new(cx.text(|list| format!("{} items", list.items.len())))
    }
}

fn actor<A: ResourceApp<Resource = Shopping>>(app: &TestApp<A>) -> tilo_app::automerge::ActorId {
    app.read_document(|doc| doc.get_actor().clone())
}

/// The part of `app`'s writer that names the device it writes on.
fn device<A: ResourceApp<Resource = Shopping>>(app: &TestApp<A>) -> String {
    collab_types::actors::device_part(&actor(app))
}

fn texts<A: ResourceApp<Resource = Shopping>>(app: &TestApp<A>) -> Vec<String> {
    let mut texts: Vec<String> = app
        .state()
        .items
        .values()
        .map(|entry| entry.text.clone())
        .collect();
    texts.sort();
    texts
}

tilo_test_env::scenario! {
    fn every_window_writes_under_an_actor_of_its_own;

    phase two_windows_on_one_record_write_under_distinct_actors;
    // Two windows of one app on one record edit it concurrently under actors of
    // their own, so both windows' edits survive the exchange.
    {
        let mut first = TestApp::<ShoppingApp>::new().unwrap();
        let mut second = TestApp::<ShoppingApp>::new().unwrap();
        assert_eq!(first.record(), second.record(), "one record");
        let mut shared = first.peer_document();
        second.merge_peer(&mut shared).unwrap();
        assert_ne!(actor(&first), actor(&second));

        first.invoke(Shopping::ADD.with("milk")).unwrap();
        second.invoke(Shopping::ADD.with("eggs")).unwrap();
        first.invoke(Shopping::ADD.with("bread")).unwrap();
        let mut theirs = second.peer_document();
        first.merge_peer(&mut theirs).unwrap();
        let mut ours = first.peer_document();
        second.merge_peer(&mut ours).unwrap();
        assert_eq!(texts(&first), ["bread", "eggs", "milk"]);
        assert_eq!(texts(&second), texts(&first));
    }

    phase a_window_record_opened_twice_on_a_device_is_written_by_two_copies;
    // A second window on a window's record another window on this device is
    // writing never writes under the first window's actor: it is a copy of its
    // own, on the same device. Reopened after both close, the record holds what
    // each wrote and the device goes on writing it.
    {
        let profile = tempfile::tempdir().unwrap();
        let mut first = TestApp::<ShoppingDocumentApp>::open_data_dir(profile.path()).unwrap();
        assert!(
            !first.record().is_package_primary(),
            "a window's own record"
        );
        first.invoke(Shopping::ADD.with("milk")).unwrap();
        first.flush().unwrap();
        let writer = actor(&first);

        let mut second = TestApp::<ShoppingDocumentApp>::open_data_dir(profile.path()).unwrap();
        assert_eq!(second.record(), first.record(), "one record");
        assert_ne!(actor(&second), writer, "a writer of its own");
        assert_eq!(device(&second), device(&first), "on the same device");
        assert_eq!(texts(&second), ["milk"], "what the first window wrote");
        second.invoke(Shopping::ADD.with("eggs")).unwrap();
        second.flush().unwrap();
        drop(second);
        drop(first);

        let mut again = TestApp::<ShoppingDocumentApp>::open_data_dir(profile.path()).unwrap();
        assert_ne!(actor(&again), writer, "each window is a copy of its own");
        assert_eq!(
            collab_types::actors::device_part(&writer),
            device(&again),
            "the device goes on writing it"
        );
        assert_eq!(texts(&again), ["eggs", "milk"], "both windows' edits");
        again.invoke(Shopping::ADD.with("bread")).unwrap();
        assert_eq!(texts(&again), ["bread", "eggs", "milk"]);
    }

    phase a_second_window_joins_the_workspace_record_under_an_actor_of_its_own;
    // Every window of an app opens the workspace's one record, so a second
    // window on this device joins it as a writer of its own: its changes are
    // numbered under another actor and neither window's edits are lost. Once
    // both close, a window opened again writes as the same device.
    {
        let profile = tempfile::tempdir().unwrap();
        let mut first = TestApp::<ShoppingApp>::open_data_dir(profile.path()).unwrap();
        assert!(
            first.record().is_package_primary(),
            "the workspace's record"
        );
        first.invoke(Shopping::ADD.with("milk")).unwrap();
        first.flush().unwrap();
        let writer = actor(&first);

        let mut second = TestApp::<ShoppingApp>::open_data_dir(profile.path()).unwrap();
        assert_eq!(second.record(), first.record(), "one record");
        assert_ne!(actor(&second), writer, "a writer of its own");
        assert_eq!(texts(&second), ["milk"], "what the first window wrote");
        second.invoke(Shopping::ADD.with("eggs")).unwrap();
        second.flush().unwrap();
        drop(second);
        drop(first);

        let mut again = TestApp::<ShoppingApp>::open_data_dir(profile.path()).unwrap();
        assert_ne!(actor(&again), writer, "each window is a copy of its own");
        assert_eq!(
            collab_types::actors::device_part(&writer),
            device(&again),
            "the device goes on writing it"
        );
        assert_eq!(texts(&again), ["eggs", "milk"], "both windows' edits");
        again.invoke(Shopping::ADD.with("bread")).unwrap();
        assert_eq!(texts(&again), ["bread", "eggs", "milk"]);
    }
}

/// The same list as a document of its own in each window, as an editor's
/// windows are.
struct ShoppingDocumentApp;

impl ResourceApp for ShoppingDocumentApp {
    type Resource = Shopping;
    const ID: &'static str = "so.tilo.test-shopping-document";
    const TITLE: &'static str = "Shopping";
    const WINDOW_RESOURCE: bool = true;

    fn view(cx: &mut ViewCx<Shopping>) -> Element {
        Box::new(cx.text(|list| format!("{} items", list.items.len())))
    }
}

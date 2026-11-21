//! A record the workspace keeps for every app of it
//! (`collab_types::WORKSPACE_DOCUMENTS`), opened as a person record would
//! be: one record in the workspace for every member, not one per person, and
//! what a person kept of it in their own scope before is taken into the
//! workspace they own, never into one they are only a member of.

use std::rc::Rc;

use tilo_app::Role;
use tilo_app::prelude::*;
use tilo_app::resource::person::PersonResources;
use tilo_app::resource::{ResourceOwners, ResourceType};

/// The app keeping the record: Browser, whose bookmarks the workspace keeps.
const BROWSER: &str = "so.tilo.browser";

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shelf {
    pub items: Vec<String>,
}

#[resource(
    kind = "test.shelf",
    app = "so.tilo.browser",
    catalogue = "tests/locales"
)]
impl Shelf {}

/// What the workspace bookmarked.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Bookmarks {
    pub pages: Collection<String>,
}

#[resource(
    kind = "browser.personal-bookmarks",
    app = "so.tilo.browser",
    catalogue = "tests/locales"
)]
impl Bookmarks {
    /// Keep a page.
    #[action]
    pub fn keep(&mut self, cx: &mut ActionCx, page: String) {
        self.pages.push(cx.id(), page);
    }
}

impl tilo_app::tilo_resource::SharedResource for Bookmarks {
    const OWNER: &'static str = BROWSER;
}

struct BrowserApp;

impl ResourceApp for BrowserApp {
    type Resource = Shelf;
    const ID: &'static str = BROWSER;
    const TITLE: &'static str = "Browser";
    const PERSON_RESOURCES: &'static [PersonKind] = &[PersonKind::shared::<Bookmarks>()];

    fn view(_cx: &mut ViewCx<Shelf>) -> Element {
        Box::new(Column::new())
    }
}

fn ada() -> String {
    "ad".repeat(32)
}

fn bob() -> String {
    "b0".repeat(32)
}

fn pages(record: &PersonResource<Bookmarks>) -> Vec<String> {
    let mut pages: Vec<String> = record.with(|state| state.pages.values().cloned().collect());
    pages.sort();
    pages
}

/// `person`'s own scope on a device, holding what they bookmarked there
/// before the workspace kept it.
fn kept_before(person: &str, page: &str) -> Result<PersonResources, String> {
    let mut owners = ResourceOwners::with_store(Rc::new(durable_store::MemoryStore::new()));
    let held = PersonResources::open(
        &mut owners,
        BROWSER,
        tilo_app::permissions::MemberId::person(person),
    )
    .map_err(|error| error.to_string())?;
    let record = held
        .open_resource(ResourceType::new(Bookmarks::KIND), &|doc| {
            <Bookmarks as tilo_app::tilo_resource::Resource>::initialize(doc)
                .map_err(|error| error.to_string())
        })
        .map_err(|error| error.to_string())?;
    let mut before = tilo_app::tilo_resource::Replica::<Bookmarks>::new().as_author(person);
    before
        .apply(Bookmarks::KEEP.with(page))
        .map_err(|error| error.to_string())?;
    record.edit(false, |doc| {
        tilo_app::tilo_resource::evolution::copy_into(before.doc(), doc)
            .map_err(|error| error.to_string())
    })?;
    Ok(held)
}

tilo_test_env::scenario! {
    fn a_record_the_workspace_keeps_is_one_record_for_every_member;

    phase two_members_open_the_same_record_of_the_workspace;
    {
        let mut adas = TestApp::<BrowserApp>::for_person(&ada()).unwrap();
        let mut bobs = TestApp::<BrowserApp>::for_person_with_role(&bob(), Role::Editor).unwrap();
        let on_adas = adas.shared_person::<Bookmarks>().unwrap();
        let on_bobs = bobs.shared_person::<Bookmarks>().unwrap();
        let route = on_adas.record().route().expect("the workspace's record syncs");
        assert_eq!(route, on_bobs.record().route().unwrap(), "one record for both of them");
        assert_eq!(route.app_id, BROWSER);
        assert_eq!(
            route.cluster_id.to_string(),
            tilo_app::framework::headless::TEST_WORKSPACE,
            "it is the workspace's, not a person's own store's"
        );
        assert_eq!(
            route.document.as_deref(),
            Some(collab_types::workspace_document_id(Bookmarks::KIND).as_str())
        );

        on_adas.invoke(&Bookmarks::KEEP.with("from-ada")).unwrap();
        on_bobs.invoke(&Bookmarks::KEEP.with("from-bob")).unwrap();
        on_bobs
            .record()
            .merge_peer_document(on_adas.record().document().unwrap())
            .unwrap();
        on_adas
            .record()
            .merge_peer_document(on_bobs.record().document().unwrap())
            .unwrap();
        assert_eq!(pages(&on_adas), ["from-ada", "from-bob"]);
        assert_eq!(pages(&on_adas), pages(&on_bobs), "each sees what the other kept");
    }

    phase the_record_is_published_as_the_workspaces;
    {
        let report = tilo_app::framework::headless::surface_report::<BrowserApp>();
        let declared = report["resources"]
            .as_array()
            .expect("resources")
            .iter()
            .find(|resource| resource["kind"] == Bookmarks::KIND)
            .cloned()
            .expect("the app declares the record");
        assert_ne!(
            declared["person"],
            serde_json::json!(true),
            "the workspace keeps it, so it is not the person's own"
        );
    }

    phase what_a_person_kept_moves_into_the_workspace_they_own;
    {
        let held = kept_before(&ada(), "kept-before").unwrap();
        let mut adas = TestApp::<BrowserApp>::for_person_shown_by(&ada(), &held).unwrap();
        let record = adas.shared_person::<Bookmarks>().unwrap();
        assert_eq!(pages(&record), ["kept-before"], "taken into the workspace's record");
        assert_eq!(
            record.record().route().unwrap().cluster_id.to_string(),
            tilo_app::framework::headless::TEST_WORKSPACE
        );
        // Taken in once: a later edit is the workspace's own.
        record.invoke(&Bookmarks::KEEP.with("kept-after")).unwrap();
        drop(record);
        let mut adas = adas.reopen().unwrap();
        assert_eq!(
            pages(&adas.shared_person::<Bookmarks>().unwrap()),
            ["kept-after", "kept-before"]
        );
    }

    phase a_workspace_they_are_only_a_member_of_takes_in_nothing_of_theirs;
    {
        let held = kept_before(&bob(), "bobs-own").unwrap();
        let mut bobs =
            TestApp::<BrowserApp>::for_person_with_role_shown_by(&bob(), Role::Editor, &held)
                .unwrap();
        let record = bobs.shared_person::<Bookmarks>().unwrap();
        assert!(pages(&record).is_empty(), "nothing of Bob's reaches a workspace he does not own");
    }
}

use super::*;
use tilo_actions::policy::{MemberAccess, Role, WorkspacePolicy};
use tilo_member_types::{Acting, Actor, MemberId};
use tilo_resource::Model;
use tilo_ui_base::permissions::PermissionContext;
use ui_core::resource::ResourceOwners;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct History {
    watched: Vec<String>,
}

#[tilo_resource::resource(kind = "messages.drafts", catalogue = "tests/locales")]
impl History {}

#[test]
fn a_known_reader_can_read_their_record_at_any_role() {
    const APP: &str = "so.tilo.messages";
    const WORKSPACE: &str = "00000000-0000-4000-8000-0000000000ad";
    let ada = "ad".repeat(32);
    let bob = "b0".repeat(32);
    let mut policy = WorkspacePolicy::new(WORKSPACE, bob.clone());
    policy.members.insert(ada.clone(), Role::Viewer);
    let policy = policy.with_current_roles();
    let route = tilo_actions::policy::roles::member_record(History::KIND, &ada)
        .expect("Ada's record has a verifiable owner");
    let owners = ResourceOwners::in_memory();
    let open = |person: &str, id| {
        let author = MemberId::person(person);
        let records =
            WorkspaceResources::in_workspace(owners.clone(), WORKSPACE.to_owned(), author.clone());
        let actor = Acting::from(Actor::Member(author));
        let permissions = LivePermissions::fixed(PermissionContext::new(
            actor.clone(),
            Vec::new(),
            MemberAccess::new(policy.clone(), actor),
        ));
        WorkspaceResource::<History>::open_at(
            &records,
            APP,
            id,
            (APP, "Messages"),
            Some(permissions),
            None,
        )
        .expect("the workspace record")
    };
    assert!(open(&ada, &route).may_read(), "Ada is a Viewer");
    assert!(!open(&bob, &route).may_read(), "Bob owns the workspace");
    for person in [&ada, &bob] {
        assert!(
            !open(
                person,
                collab_types::EARLIER_RECORDS
                    .iter()
                    .find_map(|earlier| earlier.fixed.filter(|_| earlier.app == APP))
                    .unwrap()
            )
            .may_read(),
            "the earlier drafts have no verifiable owner"
        );
    }
}

#[test]
fn a_record_whose_store_was_busy_opens_once_the_next_arrival_answers_it() {
    const APP: &str = "so.tilo.messages";
    const WORKSPACE: &str = "00000000-0000-4000-8000-0000000000ad";
    let ada = "ad".repeat(32);
    let store = ResourceOwners::in_memory()
        .open_beside(APP, WORKSPACE)
        .expect("the app's store");
    let kept = store.clone();
    let records = WorkspaceResources::new(MemberId::person(&ada), move |_| Ok(kept.clone()));
    let state = WorkspaceState::new(records, APP, "Messages", None, None);
    let id = tilo_actions::policy::roles::member_record(History::KIND, &ada)
        .expect("Ada's record has a verifiable owner");
    assert!(
        state
            .open_with::<History>(APP, &id, |_| Err(Unopened::InUse))
            .is_none(),
        "the store is busy when the view first asks"
    );
    // One arrival: the workspace answers that it holds none of the record.
    store.note_kept();
    assert!(
        state.open_at::<History>(APP, &id).is_some(),
        "the view that asks again after the answer gets the record"
    );
}

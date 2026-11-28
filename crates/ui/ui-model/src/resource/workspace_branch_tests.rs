//! A resource of the workspace's own, branched, is the one an app of the branch makes: it needs the
//! workspace's own resources, which the toolkit keeps.

use super::workspace::WorkspaceResources;
use super::*;
use crate::resource::transfer::branch;
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use tilo_automerge::ApplyIncoming as _;

const FROM: &str = "3f2b1c00-0000-4000-8000-00000000000a";
const TO: &str = "3f2b1c00-0000-4000-8000-00000000000b";

/// A workspace's emoji type, as the record's app declares it: the emoji are kept in a map.
fn emoji_type() -> tilo_actions::surface::Surface {
    serde_json::from_value(serde_json::json!({
        "kind": "tilo.workspace.emoji",
        "label": "Workspace emoji",
        "version": 1,
        "fields": [{ "path": "emoji", "type": { "map": "any" } }],
    }))
    .expect("a workspace emoji's type")
}

tilo_test_env::scenario! {
    fn a_branched_workspace_resource;

    phase a_workspace_record_branched_is_the_one_an_app_of_the_branch_makes;

    // A resource of the workspace's own that any app makes from its kind alone where it holds none
    // (the workspace's emoji) is, in a branch, the very resource an app of the branch makes when it
    // first opens it there: the two meet as one document, scoped to the branch, holding what the
    // source held. Before, the branch's copy kept the source's genesis, the app's was the
    // branch's, and the two met as a resource of no identity, its history blocked.
    {

        const CHAT: &str = tilo_app_id::apps::CHAT;
        const EMOJI: &str = "tilo.workspace.emoji";
        let initialize = |doc: &mut AutoCommit| {
            doc.put_object(ROOT, "emoji", ObjType::Map)
                .map(drop)
                .map_err(|error| error.to_string())
        };
        let resources = |workspace: &str| {
            let records = WorkspaceResources::in_workspace(
                ResourceOwners::in_memory(),
                workspace.into(),
                tilo_ui_base::MemberId::person("ada"),
            );
            // The app keeps its record of the workspace's emoji by its declared type: the type
            // founds its container alike on every device.
            records
                .store_of(CHAT)
                .expect("the chat app's store in the workspace")
                .declare_type(EMOJI, emoji_type);
            records
        };
        let emoji = |doc: &AutoCommit| {
            let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(ROOT, "emoji") else {
                return None;
            };
            match doc.get(&map, "parrot") {
                Ok(Some((Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
                _ => None,
            }
        };
        let source = resources(FROM).open_resource(CHAT, EMOJI, &initialize).unwrap();
        source
            .edit(true, |doc| {
                let Ok(Some((_, map))) = doc.get(ROOT, "emoji") else {
                    return Err("no emoji".to_owned());
                };
                doc.put(&map, "parrot", "parrot.png")
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        let held = source.read(AutoCommit::clone).unwrap();
        // A content-only record names no identity in its document: the store's checkout does.
        let want = source.checkout().clone();

        let copy = branch(&held, &want, FROM, TO).unwrap();
        assert!(
            validate_identity(&copy, &want, FROM).is_err(),
            "the copy is founded in the branch, not in the source"
        );
        assert_eq!(emoji(&copy).as_deref(), Some("parrot.png"));
        let made = resources(TO)
            .open_resource(CHAT, EMOJI, &initialize)
            .unwrap()
            .read(AutoCommit::clone)
            .unwrap();
        for (mut one, mut other) in [(made.clone(), copy.clone()), (copy.clone(), made.clone())] {
            one.merge_incoming(&mut other).unwrap();
            assert!(
                validate_identity(&one, &want, TO).is_ok(),
                "one resource of the branch: {:?} in {:?}",
                identity(&one),
                workspace_scope(&one)
            );
            assert_eq!(emoji(&one).as_deref(), Some("parrot.png"));
        }
        assert_eq!(
            emoji(&held).as_deref(),
            Some("parrot.png"),
            "the source keeps its own"
        );
        assert!(
            validate_identity(&held, &want, FROM).is_ok(),
            "the source is still the record of the workspace it was made in"
        );
    }
}

// Actual bound owners and cached directory resolution, not a DTO equality test.
fn directory_sampled_permission(
    time: std::sync::Arc<std::sync::atomic::AtomicU64>,
    fail: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> crate::permissions::PermissionContext {
    let now = time.load(std::sync::atomic::Ordering::Acquire);
    crate::permissions::PermissionContext::granted(
        "fixture-user",
        &crate::permissions::TreePath::root(),
        crate::permissions::policy::KindSet::ALL,
        Some(110),
    )
    .at_unix_time(now)
    .with_policy_time_sampler(std::sync::Arc::new(move || {
        if fail.load(std::sync::atomic::Ordering::Acquire) {
            Err("actual retained clock retired".into())
        } else {
            Ok(time.load(std::sync::atomic::Ordering::Acquire))
        }
    }))
}

#[test]
fn directory_permission_fresh_sampler_and_seconds_preserve_actual_bound_revision() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    let time = Arc::new(AtomicU64::new(100));
    let fail = Arc::new(AtomicBool::new(false));
    let mut fixture = ActionFixture::broker();
    *fixture.permissions.borrow_mut() = directory_sampled_permission(time.clone(), fail.clone());
    let revision = capture_bound_directory(&mut fixture);
    time.store(101, Ordering::Release);
    *fixture.permissions.borrow_mut() = directory_sampled_permission(time, fail);
    assert_eq!(
        fixture.app.action_directory_revision(),
        revision,
        "fresh host sampler with unchanged live policy must not invalidate revision"
    );
    let alias = fixture
        .app
        .actions
        .bound_publication
        .alias(&fixture.bindings[1], "bound:probe")
        .unwrap();
    assert!(matches!(
        fixture
            .app
            .resolve_action_directory(&revision, &alias, "follow-up"),
        runtime_collab::ActionDirectoryResolution::Found { .. }
    ));
}

#[test]
fn directory_permission_retained_clock_expiry_invalidates_actual_bound_revision() {
    use crate::permissions::{Answer, KindSet, TreePath, policy};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    for restrict in [false, true] {
        let time = Arc::new(AtomicU64::new(100));
        let fail = Arc::new(AtomicBool::new(false));
        let mut fixture = ActionFixture::broker();
        let grant = policy::Grant::new(
            "other",
            policy::Subject::person("fixture-user"),
            TreePath::room(crate::permissions::LOCAL_ROOM).app("other"),
            KindSet::UNDOABLE,
            if restrict {
                Answer::Deny
            } else {
                Answer::Allow
            },
            policy::Subject::person("owner"),
        )
        .with_lifetime(policy::Lifetime::Until(105_000));
        *fixture.permissions.borrow_mut() =
            directory_sampled_permission(time.clone(), fail).with_grant(grant);
        let revision = capture_bound_directory(&mut fixture);
        time.store(105, Ordering::Release);
        assert_ne!(
            fixture.app.action_directory_revision(),
            revision,
            "retained clock must invalidate at Grant/Restrict expiry"
        );
    }
}

#[test]
fn directory_permission_clock_failure_refuses_cached_resolution() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    let time = Arc::new(AtomicU64::new(100));
    let fail = Arc::new(AtomicBool::new(false));
    let mut fixture = ActionFixture::broker();
    *fixture.permissions.borrow_mut() = directory_sampled_permission(time, fail.clone());
    let revision = capture_bound_directory(&mut fixture);
    let alias = fixture
        .app
        .actions
        .bound_publication
        .alias(&fixture.bindings[1], "bound:probe")
        .unwrap();
    fail.store(true, Ordering::Release);
    assert!(
        matches!(
            fixture
                .app
                .resolve_action_directory(&revision, &alias, "follow-up"),
            runtime_collab::ActionDirectoryResolution::Unavailable
        ),
        "failed current sampler must refuse, never reuse cached Found"
    );
    assert!(fixture.app.actions.action_directory_snapshot.is_none());
    let page = fixture
        .app
        .action_directory_page(runtime_collab::ActionDirectoryQuery {
            peer_id: 0,
            revision: revision.clone(),
            cursor: vec![],
            needle: String::new(),
            parent_resource: String::new(),
            max_results: 1,
            max_bytes: 8192,
            max_work: 10,
            max_depth: 1,
        });
    assert!(page.rows.is_empty() && page.revision.is_empty() && page.budget_exhausted);
    fail.store(false, Ordering::Release);
    let recovered = capture_bound_directory(&mut fixture);
    assert_ne!(
        recovered, revision,
        "retirement cannot reuse the old snapshot identity"
    );
    assert!(matches!(
        fixture
            .app
            .resolve_action_directory(&recovered, &alias, "follow-up"),
        runtime_collab::ActionDirectoryResolution::Found { .. }
    ));
}

#[test]
fn directory_permission_actor_and_capability_semantics_invalidate_revision() {
    use crate::permissions::{
        Access, Actor, CapOverride, PermissionContext, Scope, Standing, Subject,
    };
    for actor_change in [false, true] {
        let mut fixture = ActionFixture::broker();
        let revision = capture_bound_directory(&mut fixture);
        let old = fixture.permissions.borrow().clone();
        *fixture.permissions.borrow_mut() = if actor_change {
            PermissionContext::new(
                Actor::person("other-user"),
                old.capabilities().to_vec(),
                Standing::person(old.standing().policy.clone(), "other-user"),
            )
            .at_unix_time(100)
        } else {
            old.granting(CapOverride::grant(
                Subject::person("fixture-user"),
                Scope::FsPath("/new".into()),
                Access::Read,
            ))
        };
        assert_ne!(fixture.app.action_directory_revision(), revision);
    }
}

fn republish_directory(fixture: &mut ActionFixture) -> (String, Vec<tilo_actions::Resource>) {
    let resources = fixture.app.publication_resources();
    let descriptors = tilo_actions::door::survey(&resources);
    fixture
        .app
        .capture_action_directory_snapshot(&resources, &descriptors);
    (fixture.app.action_directory_revision(), resources)
}

#[test]
fn directory_filtered_cache_expired_sole_grant_cannot_republish_old_rows() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    let time = Arc::new(AtomicU64::new(100));
    let mut fixture = ActionFixture::broker();
    *fixture.permissions.borrow_mut() =
        directory_sampled_permission(time.clone(), Arc::new(AtomicBool::new(false)));
    let old = capture_bound_directory(&mut fixture);
    let alias = fixture
        .app
        .actions
        .bound_publication
        .alias(&fixture.bindings[1], "bound:probe")
        .unwrap();
    let address = tilo_actions::ActionAddress::new(&alias, "follow-up");
    time.store(110, Ordering::Release);
    assert_ne!(fixture.app.action_directory_revision(), old);
    let (revision, resources) = republish_directory(&mut fixture);
    assert!(
        tilo_actions::door::resolve(&resources, &address).is_none(),
        "normal publication reused rows filtered under the expired sole grant"
    );
    assert!(!matches!(
        fixture
            .app
            .resolve_action_directory(&revision, &alias, "follow-up"),
        runtime_collab::ActionDirectoryResolution::Found { .. }
    ));
}

#[test]
fn directory_filtered_cache_expired_restriction_restores_republished_action() {
    use crate::permissions::{Answer, KindSet, TreePath, policy};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    let time = Arc::new(AtomicU64::new(100));
    let restriction = policy::Grant::new(
        "restriction",
        policy::Subject::person("fixture-user"),
        TreePath::room(crate::permissions::LOCAL_ROOM),
        KindSet::ALL,
        Answer::Deny,
        policy::Subject::person("owner"),
    )
    .with_lifetime(policy::Lifetime::Until(105_000));
    let mut fixture = ActionFixture::broker();
    *fixture.permissions.borrow_mut() =
        directory_sampled_permission(time.clone(), Arc::new(AtomicBool::new(false)))
            .with_grant(restriction);
    let (old, resources) = republish_directory(&mut fixture);
    let alias = fixture
        .app
        .actions
        .bound_publication
        .alias(&fixture.bindings[1], "bound:probe")
        .unwrap();
    let address = tilo_actions::ActionAddress::new(&alias, "follow-up");
    assert!(tilo_actions::door::resolve(&resources, &address).is_none());
    time.store(105, Ordering::Release);
    assert_ne!(fixture.app.action_directory_revision(), old);
    let (revision, resources) = republish_directory(&mut fixture);
    assert!(
        tilo_actions::door::resolve(&resources, &address).is_some(),
        "expired restriction must regenerate filtered rows, not permanently hide the action"
    );
    assert!(matches!(
        fixture
            .app
            .resolve_action_directory(&revision, &alias, "follow-up"),
        runtime_collab::ActionDirectoryResolution::Found { .. }
    ));
}

#[test]
fn hosted_directory_unknown_owner_domain_never_restores_revision() {
    let mut fixture = ActionFixture::broker();
    let old = capture_bound_directory(&mut fixture);
    let before = crate::bound_dispatch::directory_witness(
        fixture.app.root_widget.as_ref(),
        &fixture.app.state.doc,
    )
    .unwrap();
    assert!(
        before.iter().any(|owner| !owner.supports_model_refresh()),
        "the legacy fixture supplies no complete domain promise"
    );
    fixture.app.pump_hosted_model();
    assert_ne!(fixture.app.action_directory_revision(), old);
}

#[test]
fn hosted_directory_expiry_failure_and_owner_close_never_restore_old_revision() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };
    for case in 0..3 {
        let time = Arc::new(AtomicU64::new(100));
        let fail = Arc::new(AtomicBool::new(false));
        let mut fixture = ActionFixture::broker();
        *fixture.permissions.borrow_mut() =
            directory_sampled_permission(time.clone(), fail.clone());
        let old = capture_bound_directory(&mut fixture);
        let alias = fixture
            .app
            .actions
            .bound_publication
            .alias(&fixture.bindings[1], "bound:probe")
            .unwrap();
        match case {
            0 => time.store(110, Ordering::Release),
            1 => fail.store(true, Ordering::Release),
            _ => {
                fixture.app.close_components();
            }
        }
        fixture.app.pump_hosted_model();
        assert_ne!(fixture.app.action_directory_revision(), old);
        assert!(!matches!(
            fixture
                .app
                .resolve_action_directory(&old, &alias, "follow-up"),
            runtime_collab::ActionDirectoryResolution::Found { .. }
        ));
    }
}

#[test]
fn directory_permission_action_authority_revoke_invalidates_actual_bound_revision() {
    use crate::permissions::{PermissionContext, Role};
    use std::sync::{Arc, Mutex};
    let current = Arc::new(Mutex::new(Some(Role::Owner)));
    let source = current.clone();
    let mut fixture = ActionFixture::broker();
    *fixture.permissions.borrow_mut() =
        crate::permissions::PermissionContext::member("fixture-user", Role::Editor)
            .with_action_authority(
                "so.tilo.binding-probe".into(),
                Arc::new(move || {
                    Ok(match *source.lock().unwrap() {
                        Some(role) => PermissionContext::member("fixture-user", role),
                        None => PermissionContext::stranger("fixture-user"),
                    })
                }),
            );
    let revision = capture_bound_directory(&mut fixture);
    let alias = fixture
        .app
        .actions
        .bound_publication
        .alias(&fixture.bindings[1], "bound:probe")
        .unwrap();
    assert!(matches!(
        fixture
            .app
            .resolve_action_directory(&revision, &alias, "follow-up"),
        runtime_collab::ActionDirectoryResolution::Found { .. }
    ));
    *current.lock().unwrap() = None;
    assert!(matches!(
        fixture
            .app
            .resolve_action_directory(&revision, &alias, "follow-up"),
        runtime_collab::ActionDirectoryResolution::StaleRevision
    ));
    assert!(fixture.app.actions.action_directory_snapshot.is_none());
}

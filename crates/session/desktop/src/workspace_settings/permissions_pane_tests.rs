//! The Permissions pane lists [`Workspace::capabilities`] and its
//! composer writes one through [`Workspace::grant_as`].

use super::*;
/// Whoever is looking at the pane; never a grantor in these fixtures.
const VIEWER: &str = "viewer-user";
use collab_types::caps::ResourceRef;
use tilo_desktop_model::{Capability, GRANT_ALWAYS, GRANT_SESSION, WorkspaceId};

const OWNER: &str = "owner-user";

fn app_actor(ws: &Workspace, app: &str) -> collab_types::caps::Actor {
    collab_types::caps::Actor::app(app).in_room(ws.room().to_string())
}

fn ws_with_grants() -> Workspace {
    let mut ws = Workspace::new(WorkspaceId::from(3), "perm-test");
    ws.set_owner(OWNER);
    ws.grant_app(
        "firefox",
        &Capability::Camera {
            scope: GRANT_ALWAYS,
        },
    );
    ws.grant_app(
        "firefox",
        &Capability::Network {
            hosts: None,
            scope: GRANT_SESSION,
        },
    );
    ws
}

fn pane() -> Rect {
    Rect::new(0.0, 0.0, 660.0, 480.0)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn permission_rows_list_each_grant_with_its_real_scope_and_subject() {
    {
        let record = "b0d4e23f-c7cb-4ded-a427-b32cf9da9341";
        assert_eq!(
            scope_label(&Scope::FsRecord(record.to_string()), &NoAppDirectory),
            "one shared file (b0d4e23f-c…)"
        );
        assert_eq!(
            capability_allows(
                &CapOverride::grant(
                    Subject::Principal(Principal::App("files".to_string())),
                    Scope::FsRecord(record.to_string()),
                    Access::Read,
                ),
                &NoAppDirectory
            ),
            "View one shared file (b0d4e23f-c…)"
        );
    }

    {
        let ws = ws_with_grants();
        let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
        assert_eq!(layout.rows.len(), 2);
        for row in &layout.rows {
            assert_eq!(row.subject.kind, SubjectKind::App);
            assert_eq!(row.subject.id, "firefox");
            assert!(row.revoke_rect.x1 <= row.row_rect.x1);
            assert!(row.revoke_rect.x0 > row.row_rect.x0);
            assert!(!row.cap_id.is_empty(), "rows are keyed by capability id");
        }
        assert_ne!(layout.rows[0].cap_id, layout.rows[1].cap_id);
        assert_eq!(layout.overflow, 0);
    }

    {
        let ws = ws_with_grants();
        let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
        assert!(
            layout
                .rows
                .iter()
                .all(|r| r.subject.kind == SubjectKind::App),
            "only non-membership grants belong in the Permissions list"
        );
    }

    {
        let ws = ws_with_grants();
        let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
        assert!(
            layout
                .rows
                .iter()
                .all(|r| r.subject.kind == SubjectKind::App),
            "no person membership rows in the Permissions list"
        );
        assert!(
            !members_pane_rows(&ws, pane()).is_empty(),
            "but Members has one"
        );
    }

    {
        let mut ws = ws_with_grants();
        ws.grant_as(
            &OWNER.to_string(),
            CapOverride::grant(
                Subject::person("peer-user"),
                Scope::FsPath("/var/workspace".into()),
                Access::Write,
            ),
        )
        .expect("owner grants");
        ws.govern_as_at(
            OWNER,
            tilo_action_policy::Change::Grant(tilo_action_policy::Grant::new(
                "editors-camera-app",
                tilo_action_policy::Subject::Role(CapRole::Editor),
                ws.node().app("so.tilo.camera"),
                tilo_action_policy::KindSet::READ,
                tilo_action_policy::Answer::Allow,
                tilo_action_policy::Subject::person(OWNER),
            )),
            tilo_clock::unix_now(),
        )
        .expect("owner grants");
        // This assertion is about grouping every subject kind, so give
        // the viewport room for every seeded row after the explanatory
        // header and composer.
        let layout = permissions_pane_layout(
            &ws,
            Rect::new(0.0, 0.0, 660.0, 620.0),
            &NoAppDirectory,
            VIEWER,
        );
        let kinds: Vec<SubjectKind> = layout.rows.iter().map(|r| r.subject.kind).collect();
        assert!(kinds.contains(&SubjectKind::App));
        assert!(kinds.contains(&SubjectKind::Member));
        assert!(kinds.contains(&SubjectKind::Role));
        let mut sorted = kinds.clone();
        sorted.compact_sort();
        assert_eq!(kinds, sorted, "rows must be grouped by subject kind");
    }

    // Phase `grantors_and_holders_are_named_as_the_people_pane_names_them`.
    // A 64-hex person key is what the model records; the row names the
    // collaborator, and the viewer's own grant reads "you".
    {
        let ada = "ad".repeat(32);
        let bea = "be".repeat(32);
        let mut ws = Workspace::new(WorkspaceId::from(5), "names");
        ws.set_owner(&ada);
        ws.collaborators.push(tilo_desktop_model::Collaborator::new(
            ada.clone(),
            "Ada Lovelace",
        ));
        ws.collaborators
            .push(tilo_desktop_model::Collaborator::new(bea.clone(), "Bea"));
        ws.grant_as(
            &ada,
            CapOverride::grant(
                Subject::person(bea.clone()),
                Scope::FsPath("/var/workspace".into()),
                Access::Write,
            ),
        )
        .expect("owner grants");
        let as_bea = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
        let row = &as_bea.rows[0];
        assert_eq!(row.title, "Bea");
        assert_eq!(row.granted_by.as_deref(), Some("granted by Ada Lovelace"));
        let as_ada = permissions_pane_layout(&ws, pane(), &NoAppDirectory, &ada);
        assert_eq!(as_ada.rows[0].granted_by.as_deref(), Some("granted by you"));
        let as_bea = permissions_pane_layout(&ws, pane(), &NoAppDirectory, &bea);
        assert_eq!(
            as_bea.rows[0].title, "Bea",
            "the viewer is named, never \"You\""
        );
        for row in as_bea.rows.iter().chain(&as_ada.rows) {
            let shown = format!("{} {}", row.title, row.granted_by.as_deref().unwrap_or(""));
            assert!(
                !shown.contains("adad") && !shown.contains("bebe"),
                "{shown}"
            );
        }
    }

    {
        let mut ws = Workspace::new(WorkspaceId::from(4), "labels");
        ws.set_owner(OWNER);
        ws.grant_as(
            &OWNER.to_string(),
            CapOverride::grant(
                Subject::app("terminal"),
                Scope::FsPath("/var/workspace".into()),
                Access::Write,
            )
            .expiring(Expiry::EndOfSession),
        )
        .expect("owner grants");
        let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
        let row = &layout.rows[0];
        assert_eq!(row.allows, "Edit workspace files");
        assert_eq!(row.expiry, "Session");
        assert_eq!(
            row.granted_by.as_deref(),
            Some("granted by owner-user"),
            "grant_as records the grantor; the row shows it"
        );
    }

    // Phase `no_capability_line_leaks_the_workspace_mount_point`.
    // No capability line may name the mount point workspace files are
    // projected at (HIG §4). Swept over every scope kind, not just the
    // two the panel shows today, so a new scope can't reintroduce the leak.
    {
        let scopes = vec![
            Scope::FsPath(WORKSPACE_FILES_PATH.into()),
            Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/")),
            Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design")),
            Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design/drafts")),
            Scope::Device("camera".into()),
            Scope::NetHost("*".into()),
            Scope::NetHost("example.com".into()),
            Scope::FsRecord("b0d4e23f-c7cb-4ded-a427-b32cf9da9341".into()),
        ];
        let verbs = [Access::Read, Access::Write];
        for scope in &scopes {
            for verb in verbs {
                let cap = CapOverride::grant(Subject::app("a"), scope.clone(), verb);
                let line = capability_allows(&cap, &NoAppDirectory);
                assert!(
                    !line.contains(WORKSPACE_FILES_PATH),
                    "the permission line {line:?} names {WORKSPACE_FILES_PATH} \
                     — where the OS keeps workspace files is not what the user \
                     is being asked to agree to",
                );
                assert!(
                    !line.contains("/var/"),
                    "the permission line {line:?} exposes a system path",
                );
                assert!(
                    !line.is_empty(),
                    "every scope must still describe itself; {scope:?} produced \
                     an empty line",
                );
            }
        }
    }

    // Phase `workspace_scopes_distinguish_the_root_from_a_folder_inside_it`.
    // The projection root and a path inside it must read as different
    // things, so a row never understates what is being granted.
    {
        let root = CapOverride::grant(
            Subject::app("a"),
            Scope::FsPath(WORKSPACE_FILES_PATH.into()),
            Access::Write,
        );
        let sub = CapOverride::grant(
            Subject::app("a"),
            Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design")),
            Access::Write,
        );
        assert_eq!(
            capability_allows(&root, &NoAppDirectory),
            "Edit workspace files"
        );
        assert_eq!(
            capability_allows(&sub, &NoAppDirectory),
            "Edit \u{201c}design\u{201d} in this workspace",
        );
        assert_ne!(
            capability_allows(&root, &NoAppDirectory),
            capability_allows(&sub, &NoAppDirectory),
            "granting every workspace file must not read the same as granting \
             one folder in it",
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn granting_and_revoking_permissions_is_owner_only_and_enforced() {
    {
        {
            let ws = ws_with_grants();
            let subjects = grant_subjects(&ws);
            assert_eq!(
                subjects[0],
                GrantSubject {
                    kind: SubjectKind::Member,
                    id: OWNER.to_string()
                },
                "people come first"
            );
            assert!(subjects.contains(&GrantSubject {
                kind: SubjectKind::App,
                id: "firefox".to_string()
            }));
        }

        {
            let mut ws = ws_with_grants();
            let subjects = grant_subjects(&ws);
            let firefox = subjects
                .iter()
                .position(|s| s.id == "firefox")
                .expect("firefox is a candidate subject");
            let mut draft = GrantDraft {
                subject_index: firefox,
                choice_index: 0,
                expiry: Expiry::Never,
            };
            while draft.choice().label() != "Microphone" {
                draft.cycle_choice();
            }
            assert!(!ws.allows(
                &app_actor(&ws, "firefox"),
                &ResourceRef::Device("microphone".into()),
                Access::Read
            ));

            let edit = apply_grant_draft(&mut ws, &OWNER.to_string(), &draft);
            assert_eq!(edit.sandbox_granted.len(), 1);
            assert!(
                ws.allows(
                    &app_actor(&ws, "firefox"),
                    &ResourceRef::Device("microphone".into()),
                    Access::Read
                ),
                "the granted capability must start working, not merely start rendering"
            );
        }

        {
            let mut ws = ws_with_grants();
            let subjects = grant_subjects(&ws);
            let firefox = subjects.iter().position(|s| s.id == "firefox").unwrap();
            // Re-granting an existing capability merges expiries, so pick
            // something the app doesn't already hold to observe this one.
            let mut draft = GrantDraft {
                subject_index: firefox,
                choice_index: 0,
                expiry: Expiry::AfterUse,
            };
            while draft.choice().label() != "Microphone" {
                draft.cycle_choice();
            }
            let edit = apply_grant_draft(&mut ws, &OWNER.to_string(), &draft);
            let id = &edit.sandbox_granted[0];
            let cap = ws
                .sandbox
                .iter()
                .find(|c| &c.id == id)
                .expect("stored capability");
            assert_eq!(cap.expiry, Expiry::AfterUse);
            assert!(
                !cap.persists_across_restart(),
                "an after-use grant must not be written down as permanent"
            );
        }

        {
            // `grant_capability` keeps the longer expiry for the same
            // (subject, scope, effect, verb).
            let mut ws = ws_with_grants();
            let subjects = grant_subjects(&ws);
            let firefox = subjects.iter().position(|s| s.id == "firefox").unwrap();
            let mut draft = GrantDraft {
                subject_index: firefox,
                choice_index: 0,
                expiry: Expiry::AfterUse,
            };
            while draft.choice().label() != "Camera" {
                draft.cycle_choice();
            }
            apply_grant_draft(&mut ws, &OWNER.to_string(), &draft);
            let camera = ws
                .sandbox
                .iter()
                .filter(|c| matches!(&c.scope, Scope::Device(d) if d == "camera"))
                .collect::<Vec<_>>();
            assert_eq!(
                camera.len(),
                1,
                "the re-grant merges rather than duplicating"
            );
            assert_eq!(
                camera[0].expiry,
                Expiry::Never,
                "the existing permanent grant is not demoted to once-only"
            );
            let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
            let row = layout
                .rows
                .iter()
                .find(|r| r.allows.contains("camera"))
                .expect("camera row");
            assert_eq!(
                row.expiry, "Always",
                "the row must report the expiry that is stored, not the one that was asked for"
            );
        }

        {
            let mut ws = ws_with_grants();
            let subjects = grant_subjects(&ws);
            let firefox = subjects.iter().position(|s| s.id == "firefox").unwrap();
            let draft = GrantDraft {
                subject_index: firefox,
                choice_index: 0,
                expiry: Expiry::Never,
            };
            let before = ws.sandbox.len();
            let edit = apply_grant_draft(&mut ws, &"someone-else".to_string(), &draft);
            assert!(edit.is_empty(), "grant_as must refuse a non-administrator");
            assert_eq!(ws.sandbox.len(), before);
        }

        {
            let ws = ws_with_grants();
            let subjects = grant_subjects(&ws);
            let firefox = subjects.iter().position(|s| s.id == "firefox").unwrap();
            for (i, choice) in GRANT_CHOICES.iter().enumerate() {
                let draft = GrantDraft {
                    subject_index: firefox,
                    choice_index: i,
                    expiry: Expiry::Never,
                };
                let mut probe = ws.clone();
                match draft_capability(&ws, &draft).expect("every choice resolves") {
                    DraftGrant::Sandbox(cap) => {
                        assert_eq!(cap.effect, Effect::Grant);
                        let resource = match &cap.scope {
                            Scope::Device(d) => ResourceRef::Device(d.clone()),
                            Scope::NetHost(h) => ResourceRef::NetHost(h.clone()),
                            Scope::FsPath(p) => ResourceRef::FsPath(p.clone()),
                            Scope::FsRecord(record) => {
                                panic!("unexpected scope in the grant table: {record:?}")
                            }
                        };
                        probe
                            .grant_as(&OWNER.to_string(), cap.clone())
                            .expect("owner grants");
                        assert!(
                            probe.allows(&app_actor(&probe, "firefox"), &resource, cap.access),
                            "granting {:?} must make the decision function allow it",
                            choice.label()
                        );
                    }
                    DraftGrant::Grant(grant) => {
                        let node = grant.node.clone();
                        probe
                            .govern_as_at(
                                OWNER,
                                tilo_action_policy::Change::Grant(grant),
                                tilo_clock::unix_now(),
                            )
                            .expect("owner grants");
                        assert_eq!(
                            probe
                                .subject_authority(tilo_action_policy::Subject::App(
                                    "firefox".into()
                                ))
                                .decide(
                                    &node,
                                    tilo_action_policy::roles::GOVERNANCE_KIND,
                                    &tilo_desktop_model::context_at(tilo_clock::unix_now()),
                                )
                                .answer,
                            tilo_action_policy::Answer::Allow,
                            "granting {:?} must make the decision function allow it",
                            choice.label()
                        );
                    }
                }
            }
        }
    }

    {
        {
            let mut ws = ws_with_grants();
            let now = tilo_clock::unix_now();
            let grant = tilo_action_policy::roles::app_authorship(
                &ws.policy.room,
                "firefox",
                tilo_action_policy::Subject::person(OWNER),
            );
            let id = grant.id.as_str().to_owned();
            ws.govern_as_at(OWNER, tilo_action_policy::Change::Grant(grant.clone()), now)
                .expect("owner grants");
            assert!(ws.app_is_author_at("firefox", now));
            let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
            let row = layout
                .rows
                .iter()
                .find(|r| r.cap_id == id)
                .expect("the workspace-scoped app grant has a row");
            assert_eq!(row.subject.kind, SubjectKind::App);
            assert_eq!(row.allows, grant_allows(&grant, &NoAppDirectory));
            assert_eq!(
                apply_revoke(&mut ws, &OWNER.to_string(), &row.cap_id)
                    .revoked
                    .len(),
                1
            );
            assert!(!ws.app_is_author_at("firefox", now));
        }

        {
            let mut ws = ws_with_grants();
            let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
            let camera_row = layout
                .rows
                .iter()
                .find(|r| r.allows.contains("camera"))
                .expect("camera row");
            let edit = apply_revoke(&mut ws, &OWNER.to_string(), &camera_row.cap_id);
            assert_eq!(edit.sandbox_revoked, vec![camera_row.cap_id.clone()]);
            assert!(
                !ws.allows(
                    &app_actor(&ws, "firefox"),
                    &ResourceRef::Device("camera".into()),
                    Access::Read
                ),
                "the revoked capability must stop working, not merely stop rendering"
            );
            let after = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
            assert_eq!(after.rows.len(), 1);
        }

        {
            let mut ws = ws_with_grants();
            let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
            let target = layout.rows[0].cap_id.clone();
            let edit = apply_revoke(&mut ws, &"someone-else".to_string(), &target);
            assert!(edit.is_empty());
            assert_eq!(
                permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER)
                    .rows
                    .len(),
                2
            );
        }

        {
            let mut ws = ws_with_grants();
            let edit = apply_revoke(&mut ws, &OWNER.to_string(), "cap-does-not-exist");
            assert!(edit.is_empty());
            assert_eq!(ws.sandbox.len(), 2, "the two grants");
        }
    }
}

/// What the governance actions' typed parameters pick from: the
/// workspace's subjects as subjects, its nodes as paths and its grants by
/// id, each read by name.
#[test]
fn grant_and_revoke_parameters_pick_from_the_workspace() {
    use tilo_actions::Pick;
    let mut ws = ws_with_grants();
    let now = tilo_clock::unix_now();
    let grant = tilo_action_policy::roles::app_authorship(
        &ws.policy.room,
        "firefox",
        tilo_action_policy::Subject::person(OWNER),
    );
    ws.govern_as_at(OWNER, tilo_action_policy::Change::Grant(grant.clone()), now)
        .expect("owner grants");
    let governance = governance_resource_with_access(&ws, true);
    let param = |action: &str, name: &str| {
        governance
            .actions
            .iter()
            .find(|spec| spec.id == action)
            .and_then(|spec| spec.params().iter().find(|param| param.name == name))
            .and_then(tilo_actions::ActionParam::pick)
    };
    assert_eq!(param(GRANT_ACTION_ID, "subject"), Some(Pick::Subject));
    assert_eq!(param(GRANT_ACTION_ID, "node"), Some(Pick::Node));
    assert_eq!(param(REVOKE_ACTION_ID, "grant"), Some(Pick::Grant));

    let subjects = picks(&ws, VIEWER, &NoAppDirectory, Pick::Subject);
    assert!(!subjects.is_empty());
    for subject in &subjects {
        serde_json::from_value::<tilo_action_policy::Subject>(subject.value.clone())
            .expect("a subject binds as one");
    }
    let nodes = picks(&ws, VIEWER, &NoAppDirectory, Pick::Node);
    assert_eq!(nodes[0].value, "", "the whole workspace first");
    assert!(nodes.iter().any(|node| node.value == "app:firefox"));
    let grants = picks(&ws, VIEWER, &NoAppDirectory, Pick::Grant);
    let firefox = grants
        .iter()
        .find(|picked| picked.value == grant.id.as_str())
        .expect("the grant is offered by name");
    assert!(
        !firefox.label.contains(grant.id.as_str()),
        "{}",
        firefox.label
    );
    // Who holds it is the row; what it allows is the line under it.
    let allows = firefox.detail.as_deref().expect("what the grant allows");
    assert!(!allows.is_empty());
    assert!(!firefox.label.contains(allows), "{}", firefox.label);
}

/// A grant to the work on a resource is listed with every other grant,
/// the resource named as it publishes itself, and revoked like any.
#[test]
fn a_grant_naming_a_resource_is_listed_by_that_resources_name() {
    struct Titles;
    impl AppDirectory for Titles {
        fn app_display_name(&self, _app_id: &str) -> Option<String> {
            None
        }
        fn resource_label(&self, address: &str) -> Option<String> {
            (address == "tasks.tracker/issues/1").then(|| "Prepare release notes".into())
        }
    }
    let mut ws = ws_with_grants();
    let now = tilo_clock::unix_now();
    let grant = tilo_action_policy::Grant::new(
        "issue-grant",
        tilo_action_policy::Subject::task("tasks.tracker/issues/1"),
        tilo_action_policy::TreePath::room(ws.policy.room.clone())
            .app("so.tilo.notes")
            .any(tilo_action_policy::Level::Kind)
            .resource("tasks.tracker/issues/1"),
        tilo_action_policy::KindSet::of(tilo_action_policy::ActionKind::Undoable),
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Subject::person(OWNER),
    );
    ws.govern_as_at(OWNER, tilo_action_policy::Change::Grant(grant), now)
        .expect("owner grants");
    let tall = Rect::new(0.0, 0.0, 660.0, 1200.0);
    let layout = permissions_pane_layout(&ws, tall, &Titles, VIEWER);
    let row = layout
        .rows
        .iter()
        .find(|row| row.cap_id == "issue-grant")
        .expect("the grant has a row");
    assert_eq!(row.title, "Prepare release notes");
    assert!(
        row.allows.contains("Prepare release notes"),
        "{}",
        row.allows
    );
    assert_eq!(
        apply_revoke(&mut ws, &OWNER.to_string(), &row.cap_id)
            .revoked
            .len(),
        1
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn the_permissions_pane_draws_every_state_and_fits_its_rows() {
    {
        {
            let ws = Workspace::new(WorkspaceId::default(), "empty");
            let mut scene = Scene::new();
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            draw_permissions_pane(
                &mut scene,
                Affine::IDENTITY,
                pane(),
                (&ws, &NoAppDirectory, VIEWER),
                &GrantDraft::default(),
                true,
                &mut tr,
                &theme,
            );
            assert!(
                !scene.encoding().draw_tags.is_empty(),
                "the composer and the empty state must both paint"
            );
        }

        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let draft = GrantDraft::default();

            let mut empty = Scene::new();
            draw_permissions_pane(
                &mut empty,
                Affine::IDENTITY,
                pane(),
                (
                    &Workspace::new(WorkspaceId::default(), "empty"),
                    &NoAppDirectory,
                    VIEWER,
                ),
                &draft,
                true,
                &mut tr,
                &theme,
            );
            let mut full = Scene::new();
            draw_permissions_pane(
                &mut full,
                Affine::IDENTITY,
                pane(),
                (&ws_with_grants(), &NoAppDirectory, VIEWER),
                &draft,
                true,
                &mut tr,
                &theme,
            );
            assert!(full.encoding().draw_tags.len() > empty.encoding().draw_tags.len());
        }

        {
            let ws = ws_with_grants();
            let mut scene = Scene::new();
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            draw_permissions_pane(
                &mut scene,
                Affine::IDENTITY,
                Rect::new(100.0, 100.0, 100.0, 100.0),
                (&ws, &NoAppDirectory, VIEWER),
                &GrantDraft::default(),
                true,
                &mut tr,
                &theme,
            );
            assert!(scene.encoding().draw_tags.is_empty());
        }
    }

    {
        {
            let mut ws = Workspace::new(WorkspaceId::from(9), "many");
            ws.set_owner(OWNER);
            for i in 0..12 {
                ws.grant_as(
                    &OWNER.to_string(),
                    CapOverride::grant(
                        Subject::app(format!("app-{i}")),
                        Scope::Device("camera".into()),
                        Access::Read,
                    ),
                )
                .expect("owner grants");
            }
            let short_pane = Rect::new(0.0, 0.0, 660.0, 300.0);
            let layout = permissions_pane_layout(&ws, short_pane, &NoAppDirectory, VIEWER);
            assert!(layout.rows.len() < 12);
            assert_eq!(layout.rows.len() + layout.overflow, 12);
            for row in &layout.rows {
                assert!(
                    row.row_rect.y1 <= short_pane.y1,
                    "a laid-out row must fit inside the pane"
                );
            }
        }

        {
            let ws = ws_with_grants();
            let layout = permissions_pane_layout(&ws, pane(), &NoAppDirectory, VIEWER);
            for i in 1..3 {
                assert!(layout.expiry_rects[i].x0 >= layout.expiry_rects[i - 1].x1 - f64::EPSILON);
            }
            assert!(layout.expiry_rects[2].x1 <= layout.submit_rect.x0);
            assert!(layout.subject_rect.x1 <= layout.choice_rect.x0);
            assert!(layout.choice_rect.x1 <= layout.expiry_rects[0].x0);
        }

        {
            let ws = ws_with_grants();
            let pane = Rect::new(0.0, 0.0, 326.0, 620.0);
            let layout = permissions_pane_layout(&ws, pane, &NoAppDirectory, VIEWER);
            for rect in std::iter::once(layout.subject_rect)
                .chain(std::iter::once(layout.choice_rect))
                .chain(layout.expiry_rects)
                .chain(std::iter::once(layout.submit_rect))
            {
                assert!(
                    rect.x0 >= pane.x0 && rect.x1 <= pane.x1,
                    "compact permission control must remain inside its pane: {rect:?} in {pane:?}"
                );
            }
            assert!(
                layout.subject_rect.y1 < layout.expiry_rects[0].y0,
                "narrow composer promotes expiry/grant controls to a second row"
            );

            let candidates = [desktop_policy::AppCandidate {
                package_id: "so.tilo.viewer".into(),
                display_name: "Viewer".into(),
                resources: [desktop_policy::ResourceKind::extension("pdf").unwrap()].into(),
                secondary: false,
            }];
            let rows = default_app_rows(pane, &candidates);
            let row = rows.first().expect("a narrow policy row fits");
            assert!(row.workspace_rect.x0 >= row.row_rect.x0 + DEFAULT_APP_ROW_INSET);
            assert!(row.user_rect.x1 <= row.row_rect.x1 - DEFAULT_APP_ROW_INSET);
            assert!(
                row.workspace_rect.y0 > row.row_rect.y0 + 20.0,
                "the resource label keeps its own line above both choices"
            );
            assert!(
                row.workspace_rect.x1 <= row.user_rect.x0,
                "the two explicit choices never overlap at phone width"
            );

            let wide = Rect::new(0.0, 0.0, 670.0, 800.0);
            let rows = default_app_rows(wide, &candidates);
            let row = rows.first().expect("a wide policy row fits");
            assert!(
                row.workspace_rect.x0
                    >= row.row_rect.x0 + DEFAULT_APP_ROW_INSET + DEFAULT_APP_LABEL_MIN,
                "a wide row keeps its resource label clear of the cyclers: {row:?}"
            );
            assert!(
                row.user_rect.x1 <= row.row_rect.x1 - DEFAULT_APP_ROW_INSET,
                "the personal cycler stays inside its row card: {row:?}"
            );
            assert!(row.workspace_rect.x1 <= row.user_rect.x0);
            assert!(
                (row.workspace_rect.center().y - row.row_rect.center().y).abs() < 0.5,
                "wide-row cyclers are centred on the row"
            );
        }

        {
            let long = "a".repeat(64);
            let shortened = short_id(&long);
            assert!(shortened.chars().count() <= 11);
            assert!(shortened.ends_with('…'));
            assert_eq!(short_id("quinten"), "quinten");
        }
    }
}

/// A grant set the Permissions pane shows is the set of records the room
/// holds, each read as its own sentence: the exception on the deep node
/// first, then the broad grant it narrows, named in the words people know the
/// room's parts by.
#[test]
fn a_grant_set_reads_back_as_the_grants_the_room_holds() {
    use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath};

    let ws = ws_with_grants();
    let room = TreePath::room(ws.policy.room.clone());
    let grant = |id: &str, node: TreePath, kinds: KindSet, answer: Answer| {
        Grant::new(
            id,
            Subject::person("bob"),
            node,
            kinds,
            answer,
            Subject::person(OWNER),
        )
    };
    let grants = [
        grant("room", room.clone(), KindSet::READ_UNDOABLE, Answer::Allow),
        grant(
            "mail",
            room.clone().app("mail"),
            KindSet::ALL,
            Answer::Allow,
        ),
        grant(
            "mail-send",
            room.clone().app("mail").rest().action("send"),
            KindSet::OUTSIDE,
            Answer::Ask,
        ),
    ];
    let sentence = grant_set_sentence(&grants, &NoAppDirectory);
    assert_eq!(
        sentence,
        format!(
            "room {} › mail › anywhere › send: outside effects ask first. \
             room {} › mail: everything allowed. room {}: reading and undoable \
             changes allowed.",
            ws.policy.room, ws.policy.room, ws.policy.room
        ),
        "an exception reads beside the grant it narrows"
    );
    assert_eq!(
        grant_set_sentence(&[], &NoAppDirectory),
        "Nothing is granted."
    );

    // The one-grant view the Permissions rows show is the same sentence.
    let single = grant("mail", room.app("mail"), KindSet::READ, Answer::Deny);
    assert_eq!(
        grant_allows(&single, &NoAppDirectory),
        format!("room {} › mail: reading not allowed.", ws.policy.room)
    );
}

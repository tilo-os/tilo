//! Every kind a write path names is declared in the store a normal start builds.
//!
//! A workspace's store refuses a change that writes a record kind no declared surface names
//! ([`tilo_resource_store::admit::Refusal::Undeclared`]). A kind the system writes is therefore
//! declared from its type where the service starts, so every device that judges the record knows
//! it, whether or not the app that keeps it is installed there. This walks the kinds the
//! system's own write paths name, and the kinds every app of the workspace ships, against the
//! store a start builds: a write whose kind is not declared fails here, where the test says
//! which, rather than in a person's session. Then it makes the platform's own writes — a queued
//! outside effect handed over, and each verb no app declares — on a record that store declares.
//!
//! A new system write names its kind here by the same constant its write site names.

use std::collections::{BTreeMap, BTreeSet};

use collab_types::app_surface::Surfaces;
use collab_types::store::wire::Command;
use collab_types::store::{Cell, Resource, Value};
use tilo_actions::surface::FieldType;
use tilo_resource::surface::surface_of;
use tilo_resource_store::{Schema, Stamp, Store, reference, wire};

/// The kind of the record an app's action queues when it reaches outside the workspace, which
/// every app's surface declares.
const EFFECT: &str = "tilolang.effect";

/// A record kind and the app that keeps it, as its write site names them.
struct Record {
    app: String,
    kind: String,
}

/// `app`'s record of `kind`.
fn record(app: impl Into<String>, kind: impl Into<String>) -> Record {
    Record {
        app: app.into(),
        kind: kind.into(),
    }
}

/// The person's own records: the kinds a typed action of the person's workspace founds and
/// changes, each by its type (`person_workspace::apply`).
fn person_records() -> Vec<Record> {
    use crate::workspaces::person_workspace::devices;
    use crate::workspaces::{blocked, membership, region, servers};
    vec![
        record(devices::DEVICES.0, surface_of::<devices::Devices>().kind),
        record(blocked::BLOCKED.0, surface_of::<blocked::Blocked>().kind),
        record(region::REGION.0, surface_of::<region::Region>().kind),
        record(servers::SERVERS.0, surface_of::<servers::Servers>().kind),
        record(
            servers::SERVERS.0,
            surface_of::<servers::floor::Floor>().kind,
        ),
        record(
            membership::OWNERSHIP.0,
            surface_of::<membership::Ownership>().kind,
        ),
        record(
            tilo_user_identity::workspace_list::APP,
            surface_of::<tilo_user_identity::workspace_list::WorkspaceList>().kind,
        ),
    ]
}

/// The workspace records the system keeps for apps, each written by the module that names it.
fn system_records() -> Vec<Record> {
    #[cfg_attr(
        target_arch = "wasm32",
        allow(unused_mut, reason = "the vault is not held on wasm")
    )]
    let mut records = vec![
        // The VM session's settings, which a placed device's host keeps for it
        // (`environment::keep_setting`).
        record(
            crate::environment::VM_APP,
            surface_of::<crate::environment::VmEnvironment>().kind,
        ),
        // The workspace's emoji and the people it knows, kept for every app of the workspace.
        record(
            tilo_workspace_emoji::OWNER,
            surface_of::<tilo_workspace_emoji::WorkspaceEmoji>().kind,
        ),
        record(
            tilo_app_id::apps::CONTACTS,
            surface_of::<tilo_contact_directory::ContactDirectory>().kind,
        ),
        // The workspace's list of its agents, which the host writes as its own when they act.
        record(tilo_workspace_agent::OWNER, tilo_workspace_agent::KIND),
        // The calls that wait for their person's yes (`asked_kept`).
        record(
            tilo_action_services::workspace_invocations::PROVIDER,
            collab_types::surface::SurfaceFile::compiled(
                crate::workspaces::asked_kept::surface().as_bytes(),
            )
            .expect("the asked calls' surface")
            .resources
            .into_iter()
            .next()
            .expect("the asked record")
            .kind,
        ),
    ];
    // The workspace's vault (`workspaces::vault`).
    #[cfg(not(target_arch = "wasm32"))]
    records.push(record(
        crate::workspaces::vault::APP,
        surface_of::<crate::workspaces::vault_entries::WorkspaceVaultEntries>().kind,
    ));
    records
}

#[test]
fn every_kind_a_write_path_names_is_declared_in_the_store_a_start_builds() {
    let surfaces = Surfaces::default();
    crate::service::ServiceSessions::declare_start_surfaces(&surfaces)
        .expect("the surfaces a start declares write as the files their records are judged by");
    // What every app of a workspace ships, as a server that runs its programs names it.
    for (app, _, surface) in fleet_catalogue::surfaced() {
        surfaces.register_package(app, surface.as_bytes());
    }
    // The store `Workspaces::store_with_surfaces` builds: the reference kind, then every
    // resource of every surface the start named.
    let mut store = Store::new("workspace");
    store
        .declare(reference::schema())
        .expect("the reference type is declared");
    for (app, surface) in surfaces.resources() {
        store.declare(Schema::of_app(&app, &surface)).unwrap();
    }

    platform_writes_land(&mut store, &surfaces);

    let mut named = person_records();
    named.extend(system_records());
    // Every kind an app's shipped surface declares: registered as the app ships it, no kind is
    // left out of the store that judges its records.
    for (app, _, surface) in fleet_catalogue::surfaced() {
        let file = collab_types::surface::SurfaceFile::compiled(surface.as_bytes())
            .unwrap_or_else(|error| panic!("{app}'s surface does not read: {error}"));
        for resource in file.resources {
            named.push(record(app, resource.kind));
        }
    }
    let mut missing: BTreeSet<(String, String)> = BTreeSet::new();
    for Record { app, kind } in named {
        if store.schema().get(&kind).is_none() {
            missing.insert((app, kind));
        }
    }
    assert!(
        missing.is_empty(),
        "the store a start builds declares no type for:\n{}\n\nA kind the system writes is \
         declared from its type where the service starts (`person_workspace::declare_kinds` \
         through `Surfaces::declare_system` for a workspace record, `keep_surface` for an app's \
         own), and listed in `collab_types::WORKSPACE_DOCUMENTS` when the system keeps it for \
         every workspace.",
        missing
            .iter()
            .map(|(app, kind)| format!("  {app}: {kind}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// A command an app asks under a verb the platform writes under is refused: the wider write set
/// is the platform's alone. The same record under the app's own verb is written, so the refusal
/// is the verb's, not the record's.
#[cfg(feature = "apps")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_apps_command_under_the_platforms_verb_is_refused() {
    const APP: &str = "test.platform-command";
    const KIND: &str = "test.platform-command.kind";
    let surface = tilo_surface_compile::compile(
        "[[resources]]\nkind = \"test.platform-command.kind\"\nlabel = \"Own\"\nversion = 1\nfields = [{ path = \"title\", type = \"text\" }]\n\n[[resources.verbs]]\nid = \"type\"\nkind = \"undoable\"\n",
    )
    .expect("a surface");
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = super::hosting_device();
    let server = super::test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    workspaces.set_member_device(std::sync::Arc::new(crate::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: std::sync::Arc::new(key.clone()),
    }));
    workspaces.declare_package_surface(APP, surface.as_bytes());
    let workspace = workspaces.create("Platform").await.unwrap().workspace;
    let put = |verb: &str| Command::Put {
        kind: KIND.to_owned(),
        id: format!("{verb}-record"),
        resource: Resource {
            readers: "*".to_owned(),
            parent: None,
            order: None,
            fields: BTreeMap::from([("title".to_owned(), Cell::One(Value::Text("t".into())))]),
        },
        stamp: Stamp {
            verb: verb.to_owned(),
            invoker: "ada".to_owned(),
            invocation: format!("{verb}-invocation"),
            ..Stamp::default()
        },
    };
    for verb in tilo_resource_store::PLATFORM_VERBS {
        let refused = workspaces
            .own_resource_command(&workspace, APP, put(verb), Vec::new())
            .await
            .expect_err("no app writes under a reserved verb");
        assert!(refused.contains("platform's own verb"), "{verb}: {refused}");
    }
    workspaces
        .own_resource_command(&workspace, APP, put("type"), Vec::new())
        .await
        .expect("the app's own verb writes its record");
    server.shutdown().await;
}

/// The platform's own writes on a record the store declares: a queued outside effect handed
/// over and each verb no app declares, which every schema has as it has the move, on the record
/// an outside effect waits in; and the origin's write on a kind an outside service keeps, never
/// on the effect's.
fn platform_writes_land(store: &mut Store, surfaces: &Surfaces) {
    // A record of `kind` with its first text field, as the platform's write `verb` puts it.
    let text_field = |surface: &tilo_actions::surface::Surface| {
        surface
            .fields
            .iter()
            .find(|field| matches!(&field.ty, FieldType::Text) && !field.path.contains('.'))
            .map(|field| field.path.clone())
    };
    let effect = surfaces
        .resources()
        .into_iter()
        .map(|(_, surface)| surface)
        .find(|surface| surface.kind == EFFECT)
        .expect("every app's surface declares the record an outside effect waits in");
    let effect_field = text_field(&effect).expect("a queued effect holds text");
    // A kind an outside service keeps in step (`imap.message`): the only kind the origin's
    // write lands in.
    let (origin_kind, origin_field) = surfaces
        .resources()
        .into_iter()
        .map(|(_, surface)| surface)
        .filter_map(|surface| store.schema().get(&surface.kind).cloned())
        .filter(|ty| ty.origin.is_some())
        .find_map(|ty| {
            ty.fields
                .iter()
                .find(|field| {
                    field.value == tilo_resource_store::Expected::Text
                        && field.kind == tilo_resource_store::FieldKind::Scalar
                })
                .map(|field| (ty.name.clone(), field.name.clone()))
        })
        .expect("an app the workspace ships keeps a kind an outside service keeps in step");
    let author = [7; 32];
    let check = |store: &mut Store, kind: &str, field: &str, verb: &str, id: String| {
        let written = wire::draft(
            store,
            Command::Put {
                kind: kind.to_owned(),
                id: id.clone(),
                resource: Resource {
                    readers: String::new(),
                    parent: None,
                    order: None,
                    fields: BTreeMap::from([(
                        field.to_owned(),
                        Cell::One(Value::Text("walk".into())),
                    )]),
                },
                stamp: Stamp {
                    verb: verb.to_owned(),
                    invoker: "ada".to_owned(),
                    invocation: id,
                    ..Stamp::default()
                },
            },
        )
        .unwrap_or_else(|refused| panic!("{verb} writes a declared record: {refused}"));
        store.check_batch(&written, author)
    };
    let carries = |store: &mut Store, kind: &str, field: &str, verb: &str, id: String| {
        check(store, kind, field, verb, id)
            .unwrap_or_else(|refused| panic!("{verb} on {kind} is refused: {refused:?}"));
    };
    carries(store, EFFECT, &effect_field, "$send", "queued-1".to_owned());
    carries(
        store,
        &origin_kind,
        &origin_field,
        tilo_resource_store::ORIGIN_VERB,
        "origin-1".to_owned(),
    );
    assert!(
        check(
            store,
            EFFECT,
            &effect_field,
            tilo_resource_store::ORIGIN_VERB,
            "origin-2".to_owned(),
        )
        .is_err(),
        "the origin's write never lands in a kind no outside service keeps"
    );
    for (index, verb) in tilo_resource_store::PLATFORM_VERBS.iter().enumerate() {
        carries(
            store,
            EFFECT,
            &effect_field,
            verb,
            format!("platform-{index}"),
        );
    }
}

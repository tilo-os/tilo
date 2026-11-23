//! A window's own state lives in its entry of the workspace's layout, sealed to its readers: the
//! person's other devices ask the device that opened the window to set it and read it opened,
//! another member sees the window and not its state, and the guard refuses anyone else's write
//! of it. Asserted through the functions a session and the tilolang host call, on both shapes a
//! workspace's layout record has.

use std::collections::BTreeMap;

use automerge::{ActorId, AutoCommit};
use collab_types::DocumentMutation;
use key_group::{ContentKeys, KeyGroup, MemberKey};
use tilo_action_policy::Role;
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::commands::policy::may_set_window_state;
use tilo_desktop_model::commands::{DesktopCommand, Refusal, Requester, may_command};
use tilo_desktop_model::ownership::SharedDesktop;
use tilo_desktop_model::shared_resource::device::{self, Writer};
use tilo_desktop_model::shared_resource::guard::{self, DesktopRefusal};
use tilo_desktop_model::window_state::{self, StateError, WindowState};
use tilo_desktop_model::{
    Layout, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
    desktop_doc, shared_resource,
};
use tilolang_runtime::{EnumValue, Value};

/// Ada and Bob by their ids, their root keys in hex.
fn ada() -> String {
    hex::encode([0xa0; 32])
}
fn bob() -> String {
    hex::encode([0xb0; 32])
}

fn identity() -> WorkspaceId {
    WorkspaceId::parse("00000000-0000-4000-8000-000000000042").expect("a workspace identity")
}

/// Ada's window 100 and Bob's window 200, each a group of its own on one strip.
fn shared() -> SharedDesktop {
    let mut workspace = Workspace::new(identity(), "Studio");
    let mut strip = Strip::new(StripId(1), "Main");
    strip.window_groups = [(10, 100), (20, 200)]
        .into_iter()
        .map(|(group, id)| WindowGroup {
            id: group,
            tabs: vec![Tab {
                id,
                title: "Calculator".into(),
                tile: TileContent::LocalApp {
                    app_id: "so.tilo.calculator".into(),
                    label: "Calculator".into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
            active_tab: id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        })
        .collect();
    workspace.strips = vec![strip];
    SharedDesktop {
        tree: Layout {
            active: identity(),
            workspaces: vec![workspace],
            ..Layout::default()
        },
        records: Vec::new(),
        items: Vec::new(),
        authors: [(100, ada()), (200, bob())].into(),
        opened_by: BTreeMap::new(),
    }
}

/// The two shapes of a workspace's layout record.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// Every workspace, strip and window an entry of its own.
    Entries,
    /// The layout kept as one document.
    Document,
}

/// The workspace's layout as Ada's laptop holds it.
fn laptop(shape: Shape) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(vec![1; 16]));
    let desktop = shared();
    match shape {
        Shape::Entries => {
            shared_resource::write(&mut doc, &desktop).expect("written");
        }
        Shape::Document => {
            desktop_doc::write_shared(
                &mut doc,
                &desktop.tree,
                &desktop.records,
                &desktop.items,
                &desktop.authors,
            );
        }
    }
    doc.commit();
    doc
}

/// Another device's copy of `doc`, as it was.
fn copy(doc: &AutoCommit, actor: u8) -> AutoCommit {
    doc.clone().with_actor(ActorId::from(vec![actor; 16]))
}

/// Take what `from` holds beyond `to`'s heads into `to`, as a peer's changes arrive; what
/// arrived.
fn sync(to: &mut AutoCommit, from: &mut AutoCommit) -> Vec<u8> {
    let changes = from.save_after(&to.get_heads());
    to.load_incoming(&changes).expect("a peer's changes");
    changes
}

/// The keys of the group `readers` read under in the workspace, as the first device of theirs
/// founds it.
fn group_keys(readers: &str) -> ContentKeys {
    let group = collab_types::reader_group(identity(), readers).expect("a reader set");
    KeyGroup::create(group, MemberKey::generate("device").expect("a member key"))
        .expect("a group")
        .content_keys()
        .expect("its keys")
}

/// The same keys on another device of the group's.
fn held(keys: &ContentKeys) -> ContentKeys {
    ContentKeys::from_bytes(&keys.to_bytes().expect("encoded")).expect("decoded")
}

/// The device of `person` that made window 100 (`mine`), or another device of theirs.
fn writer<'a>(person: &'a str, mine: &'a dyn Fn(u64) -> bool) -> Writer<'a> {
    Writer {
        person: Some(person),
        mine,
        records: Writer::NOTHING_NOTED,
        items: Writer::NOTHING_ITEMED,
    }
}

/// Apply `command` on the device of `person` that `mine` says opened which windows.
fn apply(
    doc: &mut AutoCommit,
    (person, mine): (&str, &dyn Fn(u64) -> bool),
    keys: &ContentKeys,
    command: &DesktopCommand,
) -> Result<bool, StateError> {
    window_state::apply(doc, &writer(person, mine), identity(), keys, command)
}

/// Bob's state for window `tab`, sealed to himself.
fn bobs_state(keys: &ContentKeys, tab: u64) -> window_state::SealedState {
    let state = state(&[("d:mode", serde_json::json!("basic"))]);
    window_state::seal(keys, identity(), tab, &bob(), &state).unwrap()
}

fn editor(person: &str) -> Requester<'_> {
    Requester {
        person,
        role: Some(Role::Editor),
        members: None,
    }
}

fn set(state: &str, value: &serde_json::Value) -> DesktopCommand {
    DesktopCommand::SetWindowState {
        tab: 100,
        state: state.into(),
        value: value.to_string(),
    }
}

fn state(fields: &[(&str, serde_json::Value)]) -> WindowState {
    WindowState(
        fields
            .iter()
            .map(|(id, value)| ((*id).to_owned(), value.clone()))
            .collect(),
    )
}

/// The guard of the workspace's layout, holding `doc` as it was before `write` arrived,
/// deciding it as `person`'s, who here may moderate everything.
fn guarded(before: &AutoCommit, write: Vec<u8>, person: &str) -> Result<bool, DesktopRefusal> {
    let mut held = before.clone();
    guard::admit(
        &mut held,
        &DocumentMutation::Incremental(write),
        &guard::Writer {
            person,
            members: None,
            allows: &|_| true,
        },
    )
}

tilo_test_env::scenario! {
    fn a_windows_state_is_its_persons_and_follows_it_to_their_devices;

    phase the_phone_asks_the_laptop_and_reads_the_state_it_set;
    // Ada's phone holds the window but did not open it: it asks the laptop, which seals the
    // state to Ada and writes it; the phone reads it back opened with its copy of her keys.
    {
        for shape in [Shape::Entries, Shape::Document] {
            let ada = ada();
            let mut laptop = laptop(shape);
            let mut phone = copy(&laptop, 2);
            let laptop_keys = group_keys(&ada);
            let phone_keys = held(&laptop_keys);
            let asked = set("d:mode", &serde_json::json!("scientific"));
            let desktop = shared();
            let workspace = &desktop.tree.workspaces[0];
            assert_eq!(
                may_command(&editor(&ada), &desktop.authors, workspace, &asked),
                Ok(()),
                "{shape:?}: Ada asks for her own window"
            );
            assert!(asked.is_placement_independent());
            let not_mine = |_: u64| false;
            assert!(
                matches!(
                    apply(&mut phone, (&ada, &not_mine), &phone_keys, &asked),
                    Err(StateError::NotTheMaker(100))
                ),
                "{shape:?}: the phone did not open the window, so it asks"
            );
            assert_eq!(
                window_state::group(&laptop, identity(), 100, &ada).unwrap(),
                collab_types::reader_group(identity(), &ada).unwrap(),
                "{shape:?}: sealed to Ada's own group until it is shared"
            );
            let mine = |tab: u64| tab == 100;
            let before = laptop.clone();
            assert!(apply(&mut laptop, (&ada, &mine), &laptop_keys, &asked).unwrap());
            laptop.commit();
            assert!(
                !apply(&mut laptop, (&ada, &mine), &laptop_keys, &asked).unwrap(),
                "{shape:?}: the value it holds writes nothing"
            );
            let wrote = sync(&mut phone, &mut laptop);
            assert_eq!(
                window_state::read(&phone, &phone_keys, 100).unwrap(),
                Some(state(&[("d:mode", serde_json::json!("scientific"))])),
                "{shape:?}"
            );
            assert_eq!(guarded(&before, wrote, &ada), Ok(true), "{shape:?}: Ada's own window");

            // A second field keeps the first; the window's place is untouched by either.
            let filter = set("d:filter", &serde_json::json!({"some": "open"}));
            assert!(apply(&mut laptop, (&ada, &mine), &laptop_keys, &filter).unwrap());
            laptop.commit();
            sync(&mut phone, &mut laptop);
            assert_eq!(
                window_state::read(&phone, &phone_keys, 100).unwrap(),
                Some(state(&[
                    ("d:filter", serde_json::json!({"some": "open"})),
                    ("d:mode", serde_json::json!("scientific")),
                ])),
                "{shape:?}"
            );
            let windows: Vec<Vec<u64>> = device::layout_of(&phone, identity())
                .unwrap()
                .strips[0]
                .window_groups
                .iter()
                .map(|group| group.tabs.iter().map(|tab| tab.id).collect())
                .collect();
            assert_eq!(windows, vec![vec![100], vec![200]], "{shape:?}");
            assert!(
                window_state::sealed(&phone, 200).is_none(),
                "{shape:?}: Bob's window has no state of its own yet"
            );
        }
    }

    phase another_member_sees_the_window_but_not_its_state;
    {
        for shape in [Shape::Entries, Shape::Document] {
            let (ada, bob) = (ada(), bob());
            let mut laptop = laptop(shape);
            let mut bobs = copy(&laptop, 3);
            let ada_keys = group_keys(&ada);
            let bob_keys = group_keys(&bob);
            let mine = |tab: u64| tab == 100;
            window_state::apply(
                &mut laptop,
                &writer(&ada, &mine),
                identity(),
                &ada_keys,
                &set("d:mode", &serde_json::json!("scientific")),
            )
            .unwrap();
            laptop.commit();
            sync(&mut bobs, &mut laptop);
            let layout = device::layout_of(&bobs, identity()).unwrap();
            assert!(layout.strips[0].window_groups.iter().any(|group| group.tabs[0].id == 100));
            let sealed = window_state::sealed(&bobs, 100).expect("the window's state is in the layout");
            assert_eq!(sealed.readers, ada);
            assert!(!sealed.sealed.contains("scientific"));
            assert!(
                matches!(window_state::read(&bobs, &bob_keys, 100), Err(StateError::Keys(_))),
                "{shape:?}: Bob's keys open nothing of Ada's"
            );
            let desktop = shared();
            let asked = set("d:mode", &serde_json::json!("basic"));
            assert_eq!(
                may_command(&editor(&bob), &desktop.authors, &desktop.tree.workspaces[0], &asked),
                Err(Refusal::Others { id: 100, owner: Some(ada.clone()) }),
                "{shape:?}: Bob may not ask for Ada's window's state"
            );
            assert!(
                may_set_window_state(&editor(&bob), &desktop.authors, 100, Some(&sealed.readers)).is_err(),
                "{shape:?}: nor as one of its readers, which he is not"
            );
            let mut both = [ada.clone(), bob.clone()];
            both.sort();
            assert_eq!(
                may_set_window_state(&editor(&bob), &desktop.authors, 100, Some(&both.join(","))),
                Ok(()),
                "{shape:?}: a reader may"
            );
        }
    }

    phase the_guard_refuses_anyone_elses_write_of_a_windows_state;
    // Bob's device writes Ada's window's state anyway, sealed to himself: the layout's guard
    // refuses it, whatever else Bob may do to the window, and so it does his clearing it.
    {
        for shape in [Shape::Entries, Shape::Document] {
            let (ada, bob) = (ada(), bob());
            let mut laptop = laptop(shape);
            let mine = |tab: u64| tab == 100;
            window_state::apply(
                &mut laptop,
                &writer(&ada, &mine),
                identity(),
                &group_keys(&ada),
                &set("d:mode", &serde_json::json!("scientific")),
            )
            .unwrap();
            laptop.commit();
            let before = laptop.clone();
            let heads = laptop.get_heads();
            let anything = |_: u64| true;
            let bob_keys = group_keys(&bob);
            let forged = bobs_state(&bob_keys, 100);
            let mut bobs = copy(&laptop, 3);
            assert!(window_state::write(&mut bobs, &writer(&bob, &anything), 100, Some(&forged)).unwrap());
            bobs.commit();
            assert_eq!(
                guarded(&before, bobs.save_after(&heads), &bob),
                Err(DesktopRefusal::WindowState { writer: bob.clone(), window: 100 }),
                "{shape:?}"
            );
            let mut clears = copy(&laptop, 4);
            assert!(window_state::write(&mut clears, &writer(&bob, &anything), 100, None).unwrap());
            clears.commit();
            assert_eq!(
                guarded(&before, clears.save_after(&heads), &bob),
                Err(DesktopRefusal::WindowState { writer: bob.clone(), window: 100 }),
                "{shape:?}"
            );
            // His own window's state is his to write.
            let mut own = copy(&laptop, 5);
            let theirs = bobs_state(&bob_keys, 200);
            assert!(window_state::write(&mut own, &writer(&bob, &anything), 200, Some(&theirs)).unwrap());
            own.commit();
            assert_eq!(guarded(&before, own.save_after(&heads), &bob), Ok(true), "{shape:?}");
            assert!(matches!(
                window_state::write(&mut own, &writer(&bob, &anything), 300, None),
                Err(StateError::NoWindow(300))
            ));
        }
    }

    phase a_typed_value_round_trips_and_a_share_gives_it_readers;
    // A view's typed value goes in as the tilolang runtime's JSON form and comes out as the
    // same value; giving the state every member as readers lets Bob open it.
    {
        let (ada, bob) = (ada(), bob());
        let mut laptop = laptop(Shape::Entries);
        let mine = |tab: u64| tab == 100;
        let typed = Value::List(vec![
            Value::Tuple(vec![Value::Int(3), Value::Float(0.5), Value::Bool(true)]),
            Value::Opt(Some(Box::new(Value::Text("Ünïcode".into())))),
            Value::Enum(EnumValue {
                ty: "d:Sort".into(),
                variant: "v:by-date".into(),
                fields: [("f:descending".to_owned(), Value::Bool(true))].into(),
            }),
        ]);
        let ada_keys = group_keys(&ada);
        let asked = set("d:panels", &tilolang_runtime::value::to_json(&typed).unwrap());
        apply(&mut laptop, (&ada, &mine), &ada_keys, &asked).unwrap();
        laptop.commit();
        let opened = window_state::read(&laptop, &ada_keys, 100).unwrap().unwrap();
        assert_eq!(
            tilolang_runtime::value::from_json(&opened.0["d:panels"]).unwrap(),
            typed
        );
        let everyone = group_keys(collab_types::EVERY_MEMBER);
        let mut bobs = copy(&laptop, 3);
        assert!(window_state::reseal(
            &mut laptop,
            &writer(&ada, &mine),
            identity(),
            100,
            (&ada_keys, &everyone),
            &collab_types::Readers::EveryMember,
        )
        .unwrap());
        laptop.commit();
        sync(&mut bobs, &mut laptop);
        assert_eq!(
            window_state::group(&bobs, identity(), 100, &bob).unwrap(),
            identity(),
            "every member reads under the workspace's own group"
        );
        assert_eq!(window_state::read(&bobs, &held(&everyone), 100).unwrap(), Some(opened));
        assert!(matches!(
            window_state::seal(&ada_keys, identity(), 100, collab_types::EVERY_MEMBER, &WindowState::default()),
            Err(StateError::OtherGroup)
        ));
    }
}

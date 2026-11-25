//! The settings over a real resource store, checked as every holder checks a change.

use std::sync::atomic::AtomicUsize;

use collab_types::store::wire::{Command, Edit};
use serde_json::json;

use super::*;
use crate::in_memory::Workspaces;

const PERSON: &str = "\"me\"";
const AGENT: &str = "\"agent\"";

fn object(value: Value) -> Map<String, Value> {
    let Value::Object(map) = value else {
        panic!("{value} is no object")
    };
    map
}

fn by(actor: &str) -> ChangeOrigin {
    ChangeOrigin::action("settings.appearance/set-mode").by(actor)
}

fn mode(settings: &WorkspaceSettings) -> Option<Value> {
    settings.load().get("appearance")?.get("mode").cloned()
}

tilo_test_env::scenario! {
    fn the_settings_are_resources_of_the_workspace;

    phase a_write_shows_at_once_and_reaches_the_store_by_its_own_verb;
    {
        let workspaces = Workspaces::new();
        let settings = WorkspaceSettings::new();
        settings.attach(workspaces.clone());
        settings
            .update_section_by("appearance", object(json!({"mode": "dark"})), &by(PERSON))
            .unwrap();
        settings
            .update_section_by("display", object(json!({"scale": 1.5})), &by(PERSON))
            .unwrap();
        assert_eq!(mode(&settings), Some(json!("dark")));
        assert!(workspaces.woken() > 0);
        assert_eq!(
            workspaces.content(Holder::Person, "settings/appearance", "settings.appearance"),
            None,
            "a write waits for the session's pass"
        );
        settings.sync();
        assert_eq!(
            workspaces.content(Holder::Person, "settings/appearance", "settings.appearance"),
            Some(json!({"mode": "dark"}))
        );
        let device = format!("devices/{}/settings/display", "ab".repeat(32));
        assert_eq!(
            workspaces.content(Holder::Device, &device, "settings.display"),
            Some(json!({"scale": 1.5})),
            "a device's section is kept in its owner's workspace, under the device"
        );
        let history = settings.history("appearance").unwrap();
        let stamp = history.last().and_then(|version| version.stamp.clone()).unwrap();
        assert_eq!(stamp.verb, "set-mode");
        assert_eq!(stamp.invoker, PERSON);
        assert!(settings.take_deferred_failures().is_empty());
    }

    phase writes_before_a_workspace_wait_on_the_device_across_a_restart_and_are_written_on_attach;
    {
        let device = tempfile::tempdir().unwrap();
        let file = device.path().join("settings-waiting.json");
        let settings = WorkspaceSettings::kept_in(file.clone());
        assert!(settings.load().is_empty(), "nobody chose anything: every default holds");
        settings
            .update_section("locale", object(json!({"language": "de-DE"})))
            .unwrap();
        settings
            .update_section("power", object(json!({"lock_minutes": 3})))
            .unwrap();
        assert_eq!(settings.load()["locale"]["language"], json!("de-DE"));
        // Another run reads the file, not this one: what waits is on the disk before it goes.
        settings.flush_kept();
        drop(settings);

        // The device restarts before it holds a workspace: what was set is still set.
        let settings = WorkspaceSettings::kept_in(file.clone());
        assert_eq!(settings.load()["locale"]["language"], json!("de-DE"));
        assert_eq!(settings.load()["power"]["lock_minutes"], json!(3));

        let workspaces = Workspaces::new();
        settings.attach(workspaces.clone());
        assert_eq!(
            workspaces.content(Holder::Person, "settings/locale", "settings.locale"),
            Some(json!({"language": "de-DE"}))
        );
        assert_eq!(
            workspaces.content(Holder::Device, &format!("devices/{}/settings/power", "ab".repeat(32)), "settings.power"),
            Some(json!({"lock_minutes": 3}))
        );
        assert_eq!(settings.load()["locale"]["language"], json!("de-DE"));
        // The file is written by a worker: what it no longer holds is gone once it says so.
        settings.flush_kept();
        assert!(!file.exists(), "what the workspace took waits no longer");
    }

    phase another_devices_change_arrives_and_rings;
    {
        let workspaces = Workspaces::new();
        let settings = WorkspaceSettings::new();
        settings.attach(workspaces.clone());
        settings
            .update_section_by("appearance", object(json!({"mode": "light"})), &by(PERSON))
            .unwrap();
        settings.sync();
        let rung = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&rung);
        let listener: Arc<settings_store::Listener> = Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        settings.changes().listen(&listener);
        assert!(!settings.refresh(), "nothing moved since this device wrote");
        workspaces
            .command(
                Holder::Person,
                Command::Edit {
                    kind: "settings.appearance".to_owned(),
                    id: "settings/appearance".to_owned(),
                    edits: vec![Edit::Set {
                        field: "mode".to_owned(),
                        value: json!("dark"),
                    }],
                    stamp: stamp("mode", &by(PERSON), "elsewhere"),
                },
            )
            .unwrap();
        assert!(settings.refresh());
        assert_eq!(mode(&settings), Some(json!("dark")));
        assert!(rung.load(Ordering::Relaxed) > 0);
    }

    phase undo_takes_back_only_the_invokers_own_step_and_redo_brings_it_back;
    {
        let workspaces = Workspaces::new();
        let settings = WorkspaceSettings::new();
        settings.attach(workspaces);
        settings
            .update_section_by("appearance", object(json!({"mode": "dark"})), &by(PERSON))
            .unwrap();
        settings
            .update_section_by("appearance", object(json!({"accent": "teal"})), &by(AGENT))
            .unwrap();
        settings.sync();
        let me = [PERSON.to_owned()];
        assert!(settings.step("appearance", None, true, &me).unwrap());
        assert_eq!(mode(&settings), None, "the person's choice is taken back");
        assert_eq!(
            settings.load()["appearance"]["accent"],
            json!("teal"),
            "what the agent did stays"
        );
        assert!(!settings.step("appearance", None, true, &me).unwrap(), "nothing more of theirs");
        assert!(settings.step("appearance", None, false, &me).unwrap());
        assert_eq!(mode(&settings), Some(json!("dark")));
        assert!(!settings.step("appearance", None, false, &me).unwrap());
        assert!(settings.step("appearance", None, true, &me).unwrap(), "a redo is undone in turn");
        assert_eq!(mode(&settings), None);
    }

    phase a_conditional_change_over_a_newer_one_is_superseded;
    {
        let settings = WorkspaceSettings::new();
        settings.attach(Workspaces::new());
        let read = object(json!({}));
        let write = || vec![("display_transaction".to_owned(), object(json!({"id": 1})))];
        assert!(matches!(
            settings
                .compare_update_sections(&[("display_transaction", read.clone())], write(), &by(PERSON))
                .unwrap(),
            ConditionalUpdate::Stored
        ));
        assert!(matches!(
            settings
                .compare_update_sections(&[("display_transaction", read)], write(), &by(PERSON))
                .unwrap(),
            ConditionalUpdate::Superseded
        ));
        settings.sync();
        assert!(settings.take_deferred_failures().is_empty());
    }

    phase an_apps_preferences_are_its_own_resource;
    {
        let workspaces = Workspaces::new();
        let settings = WorkspaceSettings::new();
        settings.attach(workspaces.clone());
        settings
            .update_section(
                placement::APP_PERSONAL_SECTION,
                object(json!({"so.tilo.notes": {"wrap": true, "font": "mono"}})),
            )
            .unwrap();
        settings.sync();
        settings
            .update_section(
                placement::APP_PERSONAL_SECTION,
                object(json!({"so.tilo.notes": {"wrap": false}})),
            )
            .unwrap();
        settings.sync();
        assert_eq!(
            workspaces.content(Holder::Person, "settings/apps/so.tilo.notes", placement::APP_KIND),
            Some(json!({"wrap": false})),
            "preferences are written whole"
        );
        assert_eq!(
            settings.load()[placement::APP_PERSONAL_SECTION],
            json!({"so.tilo.notes": {"wrap": false}})
        );
    }

    phase another_devices_setting_is_set_in_its_own_resource_and_this_devices_stays;
    {
        let workspaces = Workspaces::new();
        let settings = WorkspaceSettings::new();
        settings.attach(workspaces.clone());
        settings
            .update_section_by("power", object(json!({"lock_minutes": 5})), &by(PERSON))
            .unwrap();
        settings.sync();
        let other = "cd".repeat(32);
        set_on(
            workspaces.as_ref(),
            &other,
            "power",
            "lock_minutes",
            &json!(30),
            &ChangeOrigin::action(format!("devices/{other}/settings/power#set-lock-minutes"))
                .by(PERSON),
        )
        .unwrap();
        let mine = format!("devices/{}/settings/power", "ab".repeat(32));
        assert_eq!(
            workspaces.content(Holder::Device, &mine, "settings.power"),
            Some(json!({"lock_minutes": 5})),
            "this device's own section is left as it is"
        );
        assert_eq!(
            workspaces.content(Holder::Device, &format!("devices/{other}/settings/power"), "settings.power"),
            Some(json!({"lock_minutes": 30})),
            "the other device's resource holds what was set for it"
        );
        settings.refresh();
        assert_eq!(settings.load()["power"]["lock_minutes"], json!(5));
        assert!(
            set_on(workspaces.as_ref(), &other, "appearance", "mode", &json!("dark"), &by(PERSON))
                .is_err(),
            "a section of the person's is no device's"
        );
    }

    phase each_device_places_its_own_displays_and_keeps_the_others_places;
    {
        use settings_store::person_displays::{PLACEMENTS_LEAF, PLACEMENTS_SECTION, PlacedDisplay};
        let placed = |device: &str, x: i32, y: i32| PlacedDisplay {
            device: device.to_owned(),
            output: "panel".to_owned(),
            order: 0,
            placed: true,
            x,
            y,
        };
        // What this device's section holds now, with its own display placed: the whole value a
        // section's setter writes, so the entries a device has not read are never sent back.
        let place = |settings: &WorkspaceSettings, display: PlacedDisplay| {
            let key = format!("{}/{}", display.device, display.output);
            let mut placement = settings
                .load()
                .get(PLACEMENTS_SECTION)
                .and_then(|section| section.get(PLACEMENTS_LEAF))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            placement.insert(key, serde_json::to_value(&display).unwrap());
            settings
                .update_section_by(
                    PLACEMENTS_SECTION,
                    object(json!({ PLACEMENTS_LEAF: placement })),
                    &by(PERSON),
                )
                .unwrap();
        };
        let at = |settings: &WorkspaceSettings, device: &str| {
            let document = settings.load();
            let entry = document
                .get(PLACEMENTS_SECTION)?
                .get(PLACEMENTS_LEAF)?
                .get(format!("{device}/panel"))?;
            Some((
                i32::try_from(entry.get("x")?.as_i64()?).ok()?,
                i32::try_from(entry.get("y")?.as_i64()?).ok()?,
            ))
        };
        let workspaces = Workspaces::new();
        let laptop = WorkspaceSettings::new();
        let pi = WorkspaceSettings::new();
        laptop.attach(workspaces.clone());
        pi.attach(workspaces.clone());

        // Both place their own display, neither having read the other's.
        place(&laptop, placed("laptop", 0, 0));
        place(&pi, placed("pi", 1440, 0));
        laptop.sync();
        pi.sync();
        laptop.refresh();
        pi.refresh();
        for settings in [&laptop, &pi] {
            assert_eq!(at(settings, "laptop"), Some((0, 0)));
            assert_eq!(at(settings, "pi"), Some((1440, 0)), "the two are placed side by side");
        }

        // Each moves its own again at the same time: both moves stay.
        place(&laptop, placed("laptop", -200, 0));
        place(&pi, placed("pi", 1440, 300));
        laptop.sync();
        pi.sync();
        laptop.refresh();
        pi.refresh();
        for settings in [&laptop, &pi] {
            assert_eq!(at(settings, "laptop"), Some((-200, 0)));
            assert_eq!(
                at(settings, "pi"),
                Some((1440, 300)),
                "the laptop's move keeps where the pi placed itself"
            );
        }

        // A session that starts again reads the arrangement the store kept.
        let restarted = WorkspaceSettings::new();
        restarted.attach(workspaces);
        assert_eq!(at(&restarted, "laptop"), Some((-200, 0)));
        assert_eq!(at(&restarted, "pi"), Some((1440, 300)));
    }
}

/// A section's own verb, not only its fields' setters, is a verb of the settings' workspace
/// resources: the change it makes is written to the store, not refused for naming an undeclared
/// verb. The shell's control hints write the person's record through `learn-control-hint`.
#[test]
fn a_sections_own_verb_is_a_change_the_store_admits() {
    let workspaces = Workspaces::new();
    let settings = WorkspaceSettings::new();
    settings.attach(workspaces.clone());
    settings
        .update_section_by(
            "control_hints",
            object(json!({
                "learned": {"enabled": true, "known": ["open-from-search"], "shown": {}}
            })),
            &ChangeOrigin::action("settings.control-hints/learn-control-hint").by(PERSON),
        )
        .expect("the section is written");
    settings.sync();
    let failures = settings.take_deferred_failures();
    assert!(
        failures.is_empty(),
        "the store admits the section's own verb: {failures:?}"
    );
    let stored = workspaces.content(
        Holder::Person,
        "settings/control-hints",
        "settings.control-hints",
    );
    assert!(
        stored
            .as_ref()
            .and_then(|stored| stored.pointer("/learned/known"))
            .is_some(),
        "the hint is in the workspace's record: {stored:?}"
    );
}

/// A setting kept on the device while no workspace holds it yet is a step of whoever made it:
/// the person takes it back there, before any store has seen it, and someone else's is not
/// theirs to take back.
#[test]
fn a_setting_kept_before_a_workspace_is_attached_is_its_makers_to_undo() {
    let settings = WorkspaceSettings::new();
    let person = ChangeOrigin::action("settings.appearance/set-mode").by(PERSON);
    settings
        .update_section_by("appearance", object(json!({ "mode": "dark" })), &person)
        .expect("kept");
    settings
        .update_section_by(
            "appearance",
            object(json!({ "high_contrast": true })),
            &person,
        )
        .expect("kept");
    assert_eq!(mode(&settings), Some(json!("dark")));
    assert!(
        !settings
            .step("appearance", None, true, &[AGENT.to_owned()])
            .expect("no store is no failure"),
        "nothing of the agent's is there to take back"
    );
    assert_eq!(mode(&settings), Some(json!("dark")));
    assert!(
        settings
            .step("appearance", None, true, &[PERSON.to_owned()])
            .expect("taken back"),
        "the person's last kept step is theirs"
    );
    assert_eq!(
        settings
            .load()
            .get("appearance")
            .and_then(|held| held.get("high_contrast")),
        None,
        "the step taken back wrote the contrast, not the mode"
    );
    assert_eq!(mode(&settings), Some(json!("dark")));
    assert!(
        settings
            .step("appearance", None, true, &[PERSON.to_owned()])
            .expect("taken back"),
        "their step before it is theirs too"
    );
    assert_eq!(mode(&settings), None, "and the mode it wrote is gone");
    assert!(
        !settings
            .step("appearance", None, true, &[PERSON.to_owned()])
            .expect("nothing more"),
        "nothing more of theirs"
    );
}

/// A change a session writes names nobody, and it stays when the person takes back their step
/// before it: undo takes their newest step of the section, not the last change made.
#[test]
fn a_later_change_naming_nobody_stays_when_a_kept_step_is_undone() {
    let settings = WorkspaceSettings::new();
    let person = ChangeOrigin::action("settings.appearance/set-mode").by(PERSON);
    settings
        .update_section_by("appearance", object(json!({ "mode": "dark" })), &person)
        .expect("kept");
    settings
        .update_section_by(
            "appearance",
            object(json!({ "high_contrast": true })),
            &ChangeOrigin::default(),
        )
        .expect("kept");
    assert!(
        settings
            .step("appearance", None, true, &[PERSON.to_owned()])
            .expect("taken back"),
        "their step is taken back though a later change came"
    );
    assert_eq!(mode(&settings), None);
    assert_eq!(
        settings
            .load()
            .get("appearance")
            .and_then(|held| held.get("high_contrast")),
        Some(&json!(true)),
        "the later change stays"
    );
}

/// A change kept for a workspace keeps its step across a restart: the next run reads who made
/// it and what it wrote, so the person takes it back before any workspace holds it.
#[test]
fn a_kept_change_is_undone_after_a_restart() {
    let dir = tempfile::tempdir().expect("a directory");
    let file = dir.path().join("settings-waiting.json");
    let person = ChangeOrigin::action("settings.appearance/set-mode").by(PERSON);
    {
        let settings = WorkspaceSettings::kept_in(file.clone());
        settings
            .update_section_by("appearance", object(json!({ "mode": "dark" })), &person)
            .expect("kept");
        settings
            .update_section_by(
                "appearance",
                object(json!({ "high_contrast": true })),
                &person,
            )
            .expect("kept");
    }
    let restarted = WorkspaceSettings::kept_in(file.clone());
    assert_eq!(mode(&restarted), Some(json!("dark")), "the values waited");
    assert!(
        !restarted
            .step("appearance", None, true, &[AGENT.to_owned()])
            .expect("no store is no failure"),
        "the step kept who made it, and it is not the agent's"
    );
    assert!(
        restarted
            .step("appearance", None, true, &[PERSON.to_owned()])
            .expect("taken back"),
        "the person's step survived the restart"
    );
    assert_eq!(
        restarted
            .load()
            .get("appearance")
            .and_then(|held| held.get("high_contrast")),
        None,
        "the step taken back wrote the contrast"
    );
    assert_eq!(mode(&restarted), Some(json!("dark")));
    assert!(
        restarted
            .step("appearance", None, true, &[PERSON.to_owned()])
            .expect("taken back"),
        "and the step before it"
    );
    assert_eq!(mode(&restarted), None);
    restarted.flush_kept();
    {
        // What the third run reads is what waits now: nothing.
        let again = WorkspaceSettings::kept_in(file);
        assert_eq!(again.load().get("appearance"), None);
    }
}

/// The settings channel's write of a read-only field is refused where it is admitted, and the
/// section keeps its value: the appearance's `theme_package` is written by the action that
/// applies a theme, never by an app's record change naming it.
#[test]
fn a_read_only_field_refuses_the_settings_channels_write() {
    let workspaces = Workspaces::new();
    let settings = WorkspaceSettings::new();
    settings.attach(workspaces);
    let write = object(json!({ "theme_package": "so.tilo.theme.triangle" }));
    let refused = settings
        .readonly_refusal("appearance", &write)
        .expect("the channel cannot write the package");
    assert!(
        refused.contains("read only"),
        "the refusal says the field is read only: {refused}"
    );
    assert_eq!(
        settings
            .load()
            .get("appearance")
            .and_then(|held| held.get("theme_package")),
        None,
        "the field keeps its value"
    );
    assert!(
        settings
            .readonly_refusal("appearance", &object(json!({ "mode": "dark" })))
            .is_none(),
        "an ordinary field is the channel's to write"
    );
}

//! This device's server settings through the actions an app invokes: the
//! reading moves with each change, the configuration they change is saved where
//! the daemon reads it, and a caller that is not Settings is refused.

use super::resource::{SERVER, ServerSettings};
use super::*;
use std::path::Path;

/// Settings' connection to this device's broker.
fn settings() -> Caller {
    Caller {
        app_id: SETTINGS_APP.to_owned(),
        workspace: "home".to_owned(),
    }
}

/// Any other app on this device.
fn other() -> Caller {
    Caller {
        app_id: "so.tilo.notes".to_owned(),
        workspace: "home".to_owned(),
    }
}

/// A configuration a device already has at `path`: one that takes no work yet.
fn existing(path: &Path) {
    tilo_server_protocol::config::ServerConfig {
        work: tilo_server_protocol::WorkSetting::none(),
        ..tilo_server_protocol::config::ServerConfig::default()
    }
    .save(path)
    .unwrap();
}

/// The reading the provider answers a snapshot with, decoded.
fn reading(published: &Published) -> ServerSettings {
    let (revision, reading) = published
        .snapshot(&settings(), SERVER)
        .blocking_recv()
        .unwrap()
        .unwrap();
    assert!(revision > 0, "a reading is published at a revision");
    serde_json::from_slice(&reading.expect("the resource has a reading")).unwrap()
}

/// Invoke `action` with `arguments`, as Settings does.
fn invoke(
    published: &Published,
    caller: &Caller,
    action: &str,
    arguments: &Value,
) -> runtime_collab::wire::Receipt {
    let arguments = serde_json::to_vec(arguments).unwrap();
    published
        .invoke(caller, SERVER, action, arguments)
        .blocking_recv()
        .unwrap()
}

/// The configuration saved at `path`.
fn saved(path: &Path) -> tilo_server_protocol::config::ServerConfig {
    tilo_server_protocol::config::ServerConfig::load(path).unwrap()
}

tilo_test_env::scenario! {
    fn an_action_changes_the_reading_and_the_configuration_it_is_kept_in;

    phase each_change_applies_and_saves;
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.toml");
        existing(&path);
        let published = Published::new(Served::at(path.clone()));
        assert_eq!(reading(&published).work.from, WorkFrom::Nobody);

        assert!(matches!(
            invoke(&published, &settings(), "set-work", &serde_json::json!({ "work": "own-devices" })),
            runtime_collab::wire::Receipt::Done(_)
        ));
        let shown = reading(&published);
        assert_eq!(shown.work.from, WorkFrom::OwnDevices);
        let config = saved(&path);
        assert_eq!(config.work.from, tilo_server_protocol::WorkFrom::OwnDevices);

        invoke(
            &published,
            &settings(),
            "set-service",
            &serde_json::json!({ "service": "app_serving", "on": true }),
        );
        invoke(
            &published,
            &settings(),
            "set-work-room",
            &serde_json::json!({ "part": "memory", "percent": 50 }),
        );
        invoke(
            &published,
            &settings(),
            "set-work-while-charging",
            &serde_json::json!({ "on": true }),
        );
        let shown = reading(&published);
        assert!(shown.services.app_serving);
        assert_eq!(shown.work.when.free_memory_percent, 50);
        assert!(shown.work.when.charging_only);
        let config = saved(&path);
        assert!(config.app_serving);
        assert_eq!(config.work.when.free_memory_percent, 50);
        assert!(config.work.when.charging_only);

        // Taking no work again leaves the service the person set as it was.
        invoke(
            &published,
            &settings(),
            "set-work",
            &serde_json::json!({ "work": "nobody" }),
        );
        let config = saved(&path);
        assert!(
            config.app_serving,
            "the service the person set stays as it was"
        );
    }

    phase a_setting_the_action_cannot_name_is_refused_and_saves_nothing;
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.toml");
        existing(&path);
        let published = Published::new(Served::at(path.clone()));
        let before = std::fs::read_to_string(&path).unwrap();
        for (action, arguments) in [
            ("set-work", serde_json::json!({ "work": "everyone" })),
            (
                "set-service",
                serde_json::json!({ "service": "telepathy", "on": true }),
            ),
            (
                "set-work-room",
                serde_json::json!({ "part": "gpu", "percent": 20 }),
            ),
        ] {
            assert!(
                matches!(
                    invoke(&published, &settings(), action, &arguments),
                    runtime_collab::wire::Receipt::Refused { .. }
                ),
                "{action} names no setting"
            );
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    phase a_device_without_a_configuration_writes_one_when_its_person_changes_a_setting;
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.toml");
        let published = Published::new(Served::at(path.clone()));
        let shown = reading(&published);
        assert!(!shown.configured);
        assert_eq!(shown.work.from, WorkFrom::Nobody);
        assert!(!path.exists());
        invoke(
            &published,
            &settings(),
            "set-work",
            &serde_json::json!({ "work": "own-devices" }),
        );
        assert!(path.exists());
        assert!(reading(&published).configured);
    }

    phase an_invoker_without_the_grant_is_refused;
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.toml");
        existing(&path);
        let published = Published::new(Served::at(path.clone()));
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(
            published
                .snapshot(&other(), SERVER)
                .blocking_recv()
                .unwrap()
                .is_err(),
            "another app reads no server settings"
        );
        let (sink, _readings) = mpsc::unbounded_channel();
        assert!(published.watch(&other(), SERVER, sink).is_err());
        assert!(matches!(
            invoke(
                &published,
                &other(),
                "set-work",
                &serde_json::json!({ "work": "anyone" })
            ),
            runtime_collab::wire::Receipt::Refused { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            before,
            "a refused caller changes nothing"
        );
    }

    phase every_view_following_the_resource_reads_the_change;
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.toml");
        existing(&path);
        let published = Published::new(Served::at(path));
        let (first_sink, mut first) = mpsc::unbounded_channel();
        let (second_sink, mut second) = mpsc::unbounded_channel();
        let first_watch = published.watch(&settings(), SERVER, first_sink).unwrap();
        published.watch(&settings(), SERVER, second_sink).unwrap();
        for readings in [&mut first, &mut second] {
            let event = readings.blocking_recv().unwrap();
            assert!(matches!(event, runtime_collab::wire::Event::Reading { .. }));
        }
        invoke(
            &published,
            &settings(),
            "set-work",
            &serde_json::json!({ "work": "anyone" }),
        );
        for readings in [&mut first, &mut second] {
            let runtime_collab::wire::Event::Reading {
                revision, reading, ..
            } = readings.blocking_recv().unwrap()
            else {
                panic!("a change is a reading");
            };
            assert!(revision > 1);
            let shown: ServerSettings =
                serde_json::from_slice(&reading.expect("the resource has a reading")).unwrap();
            assert_eq!(shown.work.from, WorkFrom::Anyone);
        }
        published.unwatch(first_watch);
        assert!(
            published
                .watches
                .lock_recover()
                .iter()
                .all(|watch| watch.id != first_watch),
            "an unwatched view is forgotten"
        );
    }
}

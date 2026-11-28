//! A person new to Tilo, whose identity and workspace this computer founded as it was set up
//! (as `tilo login --new-identity` founds them), brings their things over from the Windows left
//! on its disk: with nothing of Windows running, the browser's bookmarks and history it kept
//! reach their Browser, through the device's migrations as `tilo migrate` steps them.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use tilo_migrate::old_system::fixture::write_windows_disk;
use tilo_migrate::provider::caller::Steps;

use crate::language::host;
use crate::language::migrate_walk::{device_of, listed_in, step};
use crate::language::migrate_window::engine;

/// The Migrations app, whose own records a step's review is kept in.
const MIGRATE: &str = "so.tilo.migrate";
/// Where the browser data lands.
const BROWSER: &str = "so.tilo.browser";

#[test]
fn a_newcomer_brings_their_browser_over_from_the_windows_left_on_the_disk() {
    host::run_with(&["migrate", "browser"], |host| async move {
        let home = PathBuf::from(std::env::var_os("HOME").expect("the test's home"));
        let disk = home.join("windows");
        write_windows_disk(&disk);
        host.install_app(MIGRATE).await;
        host.install_app(BROWSER).await;
        host.daemon().serve_migrations();
        let device = device_of(&engine(host.as_ref()));
        let (own, caller) = host
            .daemon()
            .own_connection_in(host.workspace())
            .expect("the person's own connection reaches the workspace");
        let steps = Arc::new(Steps::new(
            own,
            caller,
            &device,
            host.workspace().to_owned(),
            None,
        ));

        // The review names the disk's one person and offers what their browser kept.
        let reviewed = step(
            &steps,
            "review",
            json!({"source": "disk", "profile": disk.display().to_string()}),
        )
        .await;
        let browser_parts: Vec<String> = reviewed.answer["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["key"].as_str())
            .filter(|key| {
                key.starts_with("browsers:")
                    && (key.ends_with("/bookmarks") || key.ends_with("/history"))
            })
            .map(str::to_owned)
            .collect();
        assert_eq!(
            browser_parts.len(),
            2,
            "Chrome's bookmarks and history are offered: {:#}",
            reviewed.answer
        );
        step(
            &steps,
            "choose",
            json!({"source": "disk", "items": browser_parts, "only": true}),
        )
        .await;

        let landed = step(&steps, "start", json!({"source": "disk"})).await;
        assert_eq!(landed.answer["state"], "complete", "{:#}", landed.answer);
        // Browser lists what it keeps: the bookmark ("Kept") and the page visited ("Visited").
        let listed = listed_in(host.as_ref(), host.workspace(), "browser").await;
        assert!(
            listed.contains("Kept") && listed.contains("Visited"),
            "the bookmark and the visited page are in Browser: {listed}"
        );
    });
}

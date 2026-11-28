//! A person new to Tilo, whose identity and workspace this computer founded as it was set up
//! (as `tilo login --new-identity` founds them), brings their things over from the Windows left
//! on its disk: with nothing of Windows running, the browser's bookmarks and history it kept
//! reach their Browser, and the sign-ins Firefox saved reach their Passwords, through the
//! device's migrations as `tilo migrate` steps them.

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use tilo_migrate::old_system::fixture::{
    SAVED_SITE, SAVED_USER, write_firefox_profile, write_windows_disk,
};
use tilo_migrate::provider::caller::Steps;

use crate::language::host;
use crate::language::migrate_walk::{device_of, listed_in, step};
use crate::language::migrate_window::{agent_does, answer, engine};

/// The Migrations app, whose own records a step's review is kept in.
const MIGRATE: &str = "so.tilo.migrate";

/// The Windows disk of the test's home, the app `lands_in` installed beside Migrations, and the
/// device's migrations as the person's own `tilo migrate` steps them: the disk reviewed, the
/// parts of what it holds `wanted` picks chosen alone, and the run started, which completes.
async fn brought_over(host: &host::Host, lands_in: &str, wanted: impl Fn(&str) -> bool) -> Value {
    let home = PathBuf::from(std::env::var_os("HOME").expect("the test's home"));
    let disk = home.join("windows");
    write_windows_disk(&disk);
    write_firefox_profile(&disk);
    host.install_app(MIGRATE).await;
    host.install_app(lands_in).await;
    host.daemon().serve_migrations();
    let device = device_of(&engine(host));
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
    // The review names the disk's one person and offers what their browsers kept.
    let reviewed = step(
        &steps,
        "review",
        json!({"source": "disk", "profile": disk.display().to_string()}),
    )
    .await;
    let parts: Vec<String> = reviewed.answer["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["key"].as_str())
        .filter(|key| wanted(key))
        .map(str::to_owned)
        .collect();
    assert!(
        !parts.is_empty(),
        "the review offers what is wanted: {:#}",
        reviewed.answer
    );
    step(
        &steps,
        "choose",
        json!({"source": "disk", "items": parts, "only": true}),
    )
    .await;
    // A step the workspace asks its person about (keeping a password in their vault) waits for
    // their yes in their shell; once they gave it, they start the migration again, as the run
    // tells them, and it goes on where it stopped.
    let mut landed = step(&steps, "start", json!({"source": "disk"})).await;
    for _ in 0..3 {
        if landed.answer["state"] != "failed" {
            break;
        }
        let questions = steps.questions().await.expect("the questions read");
        assert!(
            !questions.is_empty(),
            "a run fails only on what its person is asked: {:#}",
            landed.answer
        );
        for asked in questions {
            let record = format!("{}/{}", asked.record.workspace, asked.record.record);
            answer(host, &record, "approve").await;
        }
        landed = step(&steps, "start", json!({"source": "disk"})).await;
    }
    assert_eq!(landed.answer["state"], "complete", "{:#}", landed.answer);
    landed.answer
}

#[test]
fn a_newcomer_brings_their_browser_over_from_the_windows_left_on_the_disk() {
    host::run_with(&["migrate", "browser"], |host| async move {
        brought_over(host.as_ref(), "so.tilo.browser", |key| {
            key.starts_with("browsers:google-chrome")
                && (key.ends_with("/bookmarks") || key.ends_with("/history"))
        })
        .await;
        // Browser lists what it keeps: the bookmark ("Kept") and the page visited ("Visited").
        let listed = listed_in(host.as_ref(), host.workspace(), "browser").await;
        assert!(
            listed.contains("Kept") && listed.contains("Visited"),
            "the bookmark and the visited page are in Browser: {listed}"
        );
    });
}

#[test]
fn a_newcomer_s_saved_sign_ins_reach_their_passwords_from_the_disk() {
    host::run_with(&["migrate", "passwords"], |host| async move {
        brought_over(host.as_ref(), "so.tilo.passwords", |key| {
            key.starts_with("browsers:") && key.contains("firefox") && key.ends_with("/passwords")
        })
        .await;
        // The login went into the workspace's vault, its password never asked of the person;
        // Passwords lists it by its site once it settles with the vault, as it does every few
        // seconds while open.
        agent_does(&host.agent(), "passwords", "settle", &json!({}))
            .await
            .unwrap_or_else(|why| panic!("Passwords settles with the vault: {why}"));
        let logins = agent_does(&host.agent(), "passwords", "list_logins", &json!({}))
            .await
            .unwrap_or_else(|why| panic!("Passwords lists its logins: {why}"))
            .to_string();
        assert!(
            logins.contains("shop.example.org"),
            "the sign-in Firefox saved for {SAVED_SITE} as {SAVED_USER} is in Passwords: {logins}"
        );
    });
}

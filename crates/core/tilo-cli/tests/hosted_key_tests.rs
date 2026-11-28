//! `tilo do account/keys#create` from a backend's terminal: a computer that is no device of the
//! person's, with no daemon and no identity, asks their site, and gets the key they approve there.

use std::process::Command;
use test_http::{Reply, Responder};
use tilo_test_env::SpawnUnderEnvLock as _;

#[test]
fn a_computer_that_is_no_device_of_the_persons_asks_the_site_for_its_key() {
    let api = Responder::handle(|request| {
        Some(match request.target.as_str() {
            "/keys/new" => Reply::status(201).body(
                r#"{"approve_url":"http://site.test/keys/approve/one","poll":"p1","interval":1,"expires_in":900}"#,
            ),
            "/keys/poll" => Reply::status(200).body(
                r#"{"key":"tilo_sk_test","grant":"g1","name":"Backend","scope":"all","expires_at_ms":0}"#,
            ),
            _ => Reply::status(404),
        })
    });
    let entry = format!(r#"{{"api":"{}"}}"#, api.url());
    let site = Responder::handle(move |request| {
        (request.target == "/entry.json").then(|| Reply::status(200).body(entry.clone()))
    });
    let home = tempfile::tempdir().unwrap();
    let run = home.path().join("run");
    std::fs::create_dir_all(&run).unwrap();
    let out = Command::new(tilo_test_env::package_binary!("tilo"))
        .args(["do", "account/keys#create", "name=Backend", "--json"])
        .env_clear()
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_RUNTIME_DIR", &run)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TILO_SITE", site.url())
        .output_locked()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    let printed = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{said}");
    assert!(
        said.contains("approve it at http://site.test/keys/approve/one"),
        "the terminal printed no link: {said}"
    );
    let answer: serde_json::Value = serde_json::from_str(&printed).unwrap();
    assert_eq!(
        answer
            .pointer("/result/key")
            .and_then(serde_json::Value::as_str),
        Some("tilo_sk_test"),
        "{printed}"
    );
    let asked = api.drain_requests();
    assert!(
        asked
            .iter()
            .any(|request| request.target == "/keys/new" && request.body_text().contains("Backend")),
        "the site was asked for no key: {asked:?}"
    );
}

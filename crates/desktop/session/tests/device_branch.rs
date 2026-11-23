//! A machine copied from one of a person's devices, in a workspace a site
//! hosts: the copy re-keys at boot through the production re-key
//! (`tilo_device_identity::branch::settle`), its person certifies the new key,
//! and both run as real sessions. What either opens on the workspace's shared
//! shell — a record sealed under the workspace's channel keys — the other
//! shows, before and after both restart, and the source stays the member it
//! was. Before any copy, the same record between two of a person's devices
//! that were never the same machine.
//!
//! The shell's windows are the edits here rather than an app's records: a
//! window is a change of the workspace's sealed shell record, and opening one
//! needs no app sandbox, which the machines these tests run on do not give.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live \
              until their sessions end"
)]

use std::time::Instant;

use crate::common::people::{
    Device, Person, Site, at, joined_within, new_workspace, shared_invitation, show,
    two_devices_of_one_person, wait_shown,
};
use serde_json::json;
use tilo_server_protocol::workspaces::Role;

/// The app whose window is opened on the sessions here.
const NOTES: &str = "so.tilo.notes";

fn start(device: &mut Device, site: &Site) {
    device.start();
    device.wait_holding(&site.workspace);
    show(device, site);
}

/// Ada's laptop in a workspace her site hosts, with a window on its shared
/// shell the site keeps; then its disk copied while it is off, the copy
/// re-keyed at boot and certified by Ada, and both started.
fn laptop_and_its_copy() -> (Site, Person, Device, Device) {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    // The workspace's host lets its shell author its records.
    let mut laptop = ada.device("Ada's laptop");
    let hers = site.invite(ada.public, Role::Editor);
    laptop.join(&hers);
    laptop.wait_holding(&site.workspace);
    show(&laptop, &site);
    {
        let _before = laptop.window(NOTES, "Before the copy");
        wait_shown(&site, &[&laptop], "Before the copy");
    }

    // The laptop's disk is copied while it is off; the copy boots on a
    // machine of its own and re-keys.
    laptop.stop();
    let (mut copy, branched) = ada.branch(&laptop);
    assert_ne!(copy.cert.device_public_key, laptop.cert.device_public_key);
    assert_eq!(branched.previous, laptop.cert.device_public_key);
    assert!(
        copy.cert.device_label.starts_with("Ada's laptop branch "),
        "{}",
        copy.cert.device_label
    );
    assert_ne!(
        std::fs::read(copy.identity().device_privkey).unwrap(),
        std::fs::read(laptop.identity().device_privkey).unwrap(),
        "the copy holds no key of its source's"
    );
    start(&mut laptop, &site);
    start(&mut copy, &site);
    (site, ada, laptop, copy)
}

/// What either of a person's two devices writes to the workspace's shell the other reads: two
/// sessions of their own, on the route the network actor publishes a record on.
#[test]
fn an_edit_on_one_device_of_a_person_reaches_the_other() {
    let (site, _ada, laptop, phone) = two_devices_of_one_person();
    show(&laptop, &site);
    show(&phone, &site);

    let _from_laptop = laptop.window(NOTES, "From the laptop");
    wait_shown(&site, &[&laptop, &phone], "From the laptop");

    let _from_phone = phone.window(NOTES, "From the phone");
    wait_shown(&site, &[&laptop, &phone], "From the phone");
}

/// A workspace the person makes on their phone arrives on their laptop's shell and leaves the
/// laptop where it is: the person stays in the workspace they went to, and what they open next is a
/// window of it. A workspace their own join brings is the one they land in; one their list brings
/// from another device is on the shell for when they go there.
#[test]
fn a_workspace_made_on_another_device_arrives_without_taking_the_front() {
    let (site, _ada, laptop, phone) = two_devices_of_one_person();
    show(&laptop, &site);
    laptop.wait_showing(&site.workspace);

    // Made on the phone while the laptop shows the site's workspace: the phone hosts it, the
    // person's list names it, and the laptop enters it there as their further device.
    let before = phone.on_shell();
    let made = phone.act_wait(&new_workspace(), &json!({}));
    assert_eq!(
        made["update"]["state"],
        json!("succeeded"),
        "{} makes a workspace: {made}\n{}",
        phone.label,
        phone.diagnostic()
    );
    tilo_test_env::wait_until("the phone's session holds the workspace it made", || {
        phone.on_shell().len() > before.len()
    });
    let made = phone
        .on_shell()
        .into_iter()
        .find(|workspace| !before.contains(workspace))
        .expect("the workspace the phone made");

    let until = tilo_test_env::hang_deadline();
    while !laptop.on_shell().contains(&made) {
        assert!(
            Instant::now() < until,
            "{} never held {made}, made on the phone\n--- {} ---\n{}\n--- {} ---\n{}",
            laptop.label,
            laptop.label,
            syncing(&laptop),
            phone.label,
            syncing(&phone),
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert_eq!(
        laptop.showing().as_deref(),
        Some(site.workspace.as_str()),
        "{} stays in the workspace its person went to when {made} arrives from their phone; \
         its session holds {:?}\n{}",
        laptop.label,
        laptop.on_shell(),
        laptop.join_flow()
    );

    // What the person opens next is a window of the workspace they are in, which its site keeps.
    let _after = laptop.window(NOTES, "After the arrival");
    wait_shown(&site, &[&laptop], "After the arrival");
    assert_eq!(
        laptop.showing().as_deref(),
        Some(site.workspace.as_str()),
        "{} is still in the workspace it opened the window in",
        laptop.label
    );
}

/// The copy takes the workspace up again as Ada's own further device: the
/// group state it was copied with names its source, and is dropped once the
/// copy is admitted; the site admits the copy as a device of Ada's without an
/// invitation; the copy keeps the workspace on its shell throughout; and Ada
/// stays the member she was, her laptop holding the workspace too.
#[test]
fn a_machine_copied_from_a_device_rejoins_as_a_device_of_its_own() {
    let (site, ada, laptop, copy) = laptop_and_its_copy();
    // The group state on the copied disk names the device it was made for: once the workspace
    // admits the copy, the copy drops it and joins the workspace's key group as itself.
    let dropped = "group state named the device this machine was copied from";
    let admitted = format!(
        "the host admitted this device and the workspace is held here workspace={}",
        site.workspace
    );
    // The copy joins as it starts, and the product gives that join its own patience before it says
    // the join did not happen: the wait spans that and the suite's hang bound after it, and a wait
    // that runs out reports what the copy's session said instead of only that it did not.
    let rejoined = || {
        let log = copy.log();
        (log.contains(dropped)
            && log
                .lines()
                .any(|line| line.contains(&admitted) && line.contains("member=true")))
        .then_some(())
    };
    let deadline = Instant::now()
        .checked_add(joined_within())
        .expect("the wait's bound fits in an Instant");
    while rejoined().is_none() {
        assert!(
            Instant::now() < deadline,
            "the copy never rejoined {} as its own device; its session says:\n{}",
            site.workspace,
            copy.diagnostic(),
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    copy.wait_holding(&site.workspace);
    laptop.wait_holding(&site.workspace);
    // Nobody was removed, and a member is a person: Ada, the owner, is the one member, and the
    // copy adds none.
    let members = site.members();
    assert!(members.values().all(|mark| !mark.removed), "{members:?}");
    assert!(!members.contains_key(&hex::encode(copy.cert.device_public_key)));
    assert_eq!(
        members
            .get(&hex::encode(ada.public))
            .map(|mark| mark.role.as_str()),
        Some("owner"),
        "{members:?}"
    );
    assert_eq!(members.len(), 1, "{members:?}");
}

/// What either opens on the workspace's sealed shell the other shows, before
/// and after both restart.
#[test]
fn a_machine_copied_from_a_device_and_its_source_read_each_others_edits() {
    let (site, _ada, mut laptop, mut copy) = laptop_and_its_copy();
    // What the copy opens the source shows, and what the source opens the
    // copy shows: each reads the other's writes to the sealed shell.
    let from_copy = copy.window(NOTES, "From the copy");
    wait_shown(&site, &[&copy, &laptop], "From the copy");
    let from_laptop = laptop.window(NOTES, "From the laptop");
    wait_shown(&site, &[&laptop, &copy], "From the laptop");
    drop((from_copy, from_laptop));

    // Both come back as themselves.
    laptop.stop();
    copy.stop();
    start(&mut laptop, &site);
    start(&mut copy, &site);
    let _copy_again = copy.window(NOTES, "The copy again");
    wait_shown(&site, &[&copy, &laptop], "The copy again");
    let _laptop_again = laptop.window(NOTES, "The laptop again");
    wait_shown(&site, &[&laptop, &copy], "The laptop again");
}

/// One device, no copy: what it opens on the workspace's shell after a
/// restart continues what it wrote before, and the site keeps it.
#[test]
fn a_device_restarted_keeps_writing_the_workspace_shell() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    let mut laptop = ada.device("Ada's laptop");
    let hers = site.invite(ada.public, Role::Editor);
    laptop.join(&hers);
    laptop.wait_holding(&site.workspace);
    show(&laptop, &site);
    {
        let _before = laptop.window(NOTES, "Before the restart");
        wait_shown(&site, &[&laptop], "Before the restart");
    }
    laptop.stop();
    start(&mut laptop, &site);
    let _after = laptop.window(NOTES, "After the restart");
    wait_shown(&site, &[&laptop], "After the restart");
}

/// A copy started while the workspace's host is away is not the workspace's member yet: only the
/// host admits a device, and it is away. The copy's apps take part in nothing there until then, so
/// an action asked of one is refused at once, saying why, rather than waiting on an app that never
/// answers; and the shell tells the person the workspace waits for its host (`e2e-branch` S07-b,
/// where the branch booted while its source, the host, was stopped).
#[test]
fn a_copy_started_while_the_host_is_away_refuses_its_apps_actions_saying_why() {
    let ada = Person::new("Ada");
    let mut site = Site::start(&ada, "Studio");
    let mut laptop = ada.device("Ada's laptop");
    laptop.install_passwords();
    let hers = site.invite(ada.public, Role::Editor);
    laptop.join(&hers);
    laptop.wait_holding(&site.workspace);
    show(&laptop, &site);
    laptop.open(PASSWORDS);
    laptop.wait_for_window(PASSWORDS);
    // Admitted, Passwords runs what it is asked.
    let answer = laptop.app_act(PASSWORDS, LIST_LOGINS, &serde_json::json!({}));
    let state = answer["update"]["state"].as_str();
    assert!(
        matches!(state, Some("succeeded" | "failed")),
        "the laptop's Passwords did not run its action: {answer}"
    );

    laptop.stop();
    let (mut copy, _) = ada.branch(&laptop);
    site.stop();
    copy.start();
    copy.wait_holding(&site.workspace);
    show(&copy, &site);

    // The window the laptop left open waits for the host, and says so where it stands.
    tilo_test_env::wait_until(
        format!("{} holding Passwords for its host", copy.label),
        || copy.state().to_string().contains("to take this device in"),
    );
    // The copy says it holds the workspace but waits for its host, which is not this device: what
    // `tilo-vm vm branch` reads to know a branch was taken in.
    let row = held_row(&copy, &site);
    assert_eq!(row["awaiting_host"], true, "{row}");
    assert_eq!(row["hosted_here"], false, "{row}");
    // Opening an app meanwhile is refused at once, saying why.
    let asked = std::time::Instant::now();
    let opened = copy.act(&copy.open_target(PASSWORDS), &serde_json::json!({}));
    assert!(
        opened.to_string().contains("hasn't been admitted to"),
        "an open while the host is away is refused with the reason: {opened}"
    );
    assert!(asked.elapsed() < std::time::Duration::from_secs(10));
    // What the copy's shell offers of Passwords, whose window the laptop left open, is refused at
    // once, with the reason.
    let passwords = copy.app_action(PASSWORDS, LIST_LOGINS);
    let started = std::time::Instant::now();
    let refused = copy.act(&passwords, &serde_json::json!({}));
    let said = refused["err"]["message"].as_str().unwrap_or_default();
    assert_eq!(
        refused["err"]["code"], "remote_actions_unavailable",
        "{refused}"
    );
    assert!(
        said.contains("hasn't been admitted to") && said.contains("must be reachable once"),
        "the refusal says why: {refused}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "refused at once, not after waiting on the app: {:?}",
        started.elapsed()
    );
    // The shell tells the person, once, that the workspace waits for its host.
    let told = "the person is told this device waits for the workspace's host to take it in";
    tilo_test_env::wait_until(
        format!(
            "{} telling the person the workspace waits for its host",
            copy.label
        ),
        || copy.log().contains(told),
    );
}

/// Passwords' action that lists the logins of the vault.
const LIST_LOGINS: &str = "list-logins";
/// Passwords, as the person's devices install it.
const PASSWORDS: &str = "so.tilo.passwords";
/// Model Providers, as the person's devices install it.
const MODEL_PROVIDERS: &str = "so.tilo.model-providers";
/// The login Ada saves before her laptop is copied.
const LOGIN: &str = "Branch mail";

/// The logins Passwords on `device` lists, by name, as its own `list-logins` answers.
/// Passwords refuses the list while its vault is still opening, so this asks until it lists.
fn logins(device: &Device) -> Vec<String> {
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    let answer = loop {
        let answer = device.app_act(PASSWORDS, LIST_LOGINS, &serde_json::json!({}));
        if answer["update"]["state"] == "succeeded" {
            break answer;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{} Passwords never listed its logins: {answer}\n{}",
            device.label,
            device.warnings()
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    };
    answer["update"]["output"]["value"]["logins"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|login| login["name"].as_str().map(str::to_owned))
        .collect()
}

/// What a person keeps in their vault on one device is theirs on a machine copied from it: Ada
/// saves a login through Passwords' own action with the host up, her laptop is copied and the copy
/// re-keyed, and the copy, started with the host up and the laptop booted beside it as a branch
/// is, lists that login and runs Model Providers' actions (`e2e-branch` S07).
#[test]
fn a_copy_lists_the_logins_its_source_saved_and_runs_its_providers_actions() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    let mut laptop = ada.device("Ada's laptop");
    laptop.install_passwords();
    laptop.install_model_providers();
    let hers = site.invite(ada.public, Role::Editor);
    laptop.join(&hers);
    laptop.wait_holding(&site.workspace);
    show(&laptop, &site);
    laptop.open(PASSWORDS);
    laptop.wait_for_window(PASSWORDS);
    // Saved once the window shows the vault, as a person sees it open before saving into it.
    assert!(
        logins(&laptop).is_empty(),
        "a new person's vault starts empty"
    );
    let saved = laptop.app_act(
        PASSWORDS,
        "save-login",
        &serde_json::json!({
            "name": LOGIN,
            "origin": "https://branch.example",
            "username": "ada@branch.example",
            "password": "kept-on-the-laptop",
        }),
    );
    assert_eq!(
        saved["update"]["state"],
        "succeeded",
        "save-login: {saved}\n{}",
        laptop.warnings()
    );
    assert!(logins(&laptop).contains(&LOGIN.to_owned()));
    laptop.open(MODEL_PROVIDERS);
    laptop.wait_for_window(MODEL_PROVIDERS);
    // Model Providers on a device whose providers no server keeps shows the person's own models
    // and answers a read of them; adding a provider is the panel a server's gateway answers.
    let providers = laptop.app_act(MODEL_PROVIDERS, "read-providers", &serde_json::json!({}));
    assert!(
        providers["update"]["state"].is_string(),
        "the laptop's Model Providers ran its action: {providers}"
    );

    laptop.stop();
    let (mut copy, _) = ada.branch(&laptop);
    start(&mut copy, &site);
    // Opened once the host took the copy in: Passwords joins the workspace as the copy's own.
    wait_admitted(&copy, &site);
    // The vault's keys are its readers', never the site's: the copy, a further device of Ada's,
    // is let into them by the laptop, booted beside it as a branch's source boots beside it.
    laptop.start();
    copy.open(PASSWORDS);
    copy.wait_for_window(PASSWORDS);
    let listed = logins(&copy);
    assert!(
        listed.contains(&LOGIN.to_owned()),
        "the copy's Passwords lists {listed:?}, not the login its source saved\n{}\nthe laptop warned:\n{}",
        copy.warnings(),
        laptop.warnings()
    );
    copy.open(MODEL_PROVIDERS);
    copy.wait_for_window(MODEL_PROVIDERS);
    let providers = copy.app_act(MODEL_PROVIDERS, "read-providers", &serde_json::json!({}));
    assert!(
        providers["update"]["state"].is_string(),
        "the copy's Model Providers ran its action: {providers}\n{}",
        copy.warnings()
    );
}

/// What `device` says of the workspace `site` serves (`keeping_devices`): who hosts it, and
/// whether its host took `device` in yet. `Null` while `device` does not hold it.
fn held_row(device: &Device, site: &Site) -> serde_json::Value {
    device.ask(&serde_json::json!({"op": "keeping_devices"}))["answer"]["workspaces"]
        .as_array()
        .and_then(|held| {
            held.iter()
                .find(|row| row["workspace"].as_str() == Some(site.workspace.as_str()))
                .cloned()
        })
        .unwrap_or_default()
}

/// Wait until `site` took `device` in as a device of its person's own: the workspace's record names
/// this device, and its apps join there as it.
fn wait_admitted(device: &Device, site: &Site) {
    let admitted = format!(
        "the host admitted this device and the workspace is held here workspace={}",
        site.workspace
    );
    let until = tilo_test_env::hang_deadline();
    loop {
        let log = device.log();
        if log
            .lines()
            .any(|line| line.contains(&admitted) && line.contains("member=true"))
            && held_row(device, site)["awaiting_host"] == false
        {
            return;
        }
        assert!(
            std::time::Instant::now() < until,
            "{} was not admitted to {}; what it said of joining:\n{}",
            device.label,
            site.workspace,
            log.lines()
                .filter(|line| line.contains("join_flow") || line.contains("join:"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// A copy whose windows come back before its host took it in holds them, and starts each once the
/// host admitted the copy: its app joins the workspace as the copy's own. Ada leaves Passwords open
/// on her laptop; the copy boots beside the site, as a branch boots beside its host, and restores
/// it; Passwords lists the login the laptop, booted beside it again, saved. Started at once,
/// before the host took the copy in, an app was refused the workspace for good and published
/// nothing (`e2e-branch` S07-a, S07-b). Model Providers opened once admitted is
/// [`a_copy_lists_the_logins_its_source_saved_and_runs_its_providers_actions`]'s.
#[test]
fn a_copy_opens_the_windows_its_source_left_once_its_host_admits_it() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    let mut laptop = ada.device("Ada's laptop");
    laptop.install_passwords();
    let hers = site.invite(ada.public, Role::Editor);
    laptop.join(&hers);
    laptop.wait_holding(&site.workspace);
    show(&laptop, &site);
    laptop.open(PASSWORDS);
    laptop.wait_for_window(PASSWORDS);
    // Saved once the window shows the vault, as a person sees it open before saving into it.
    assert!(
        logins(&laptop).is_empty(),
        "a new person's vault starts empty"
    );
    let saved = laptop.app_act(
        PASSWORDS,
        "save-login",
        &serde_json::json!({
            "name": LOGIN,
            "origin": "https://branch.example",
            "username": "ada@branch.example",
            "password": "kept-on-the-laptop",
        }),
    );
    assert_eq!(
        saved["update"]["state"],
        "succeeded",
        "save-login: {saved}\n{}",
        laptop.warnings()
    );

    laptop.stop();
    let (mut copy, _) = ada.branch(&laptop);
    copy.start();
    wait_admitted(&copy, &site);
    copy.wait_for_window(PASSWORDS);
    // The vault's keys are its readers', never the site's: the laptop lets the copy into them.
    laptop.start();
    let listed = logins(&copy);
    assert!(
        listed.contains(&LOGIN.to_owned()),
        "the copy's Passwords lists {listed:?}, not the login its source saved\n{}\nthe laptop warned:\n{}",
        copy.warnings(),
        laptop.warnings()
    );
}

/// Bring the workspace `workspace` (its id) to the front on `device`'s shell.
fn show_held(device: &Device, workspace: &str) {
    let held = || device.held().iter().any(|held| held == workspace);
    tilo_test_env::wait_until(format!("{} holds {workspace}", device.label), held);
    let shown = device.act(
        &at("so.tilo.session", "desktop:chrome", "switch-workspace"),
        &json!({"workspace": workspace}),
    );
    assert!(
        shown.get("err").is_none(),
        "{} shows {workspace}: {shown}",
        device.label
    );
    tilo_test_env::wait_until(format!("{} shows {workspace}", device.label), || {
        device.showing().as_deref() == Some(workspace)
    });
}

/// The window titles `device` 's shell shows in the workspace `workspace` (its id).
fn titles_in(device: &Device, workspace: &str) -> Vec<String> {
    let state = device.state();
    let tree: tilo_desktop_model::Layout =
        serde_json::from_value(state["state"]["workspace_tree"].clone()).unwrap_or_default();
    tree.workspaces
        .iter()
        .filter(|held| held.id().to_string() == workspace)
        .flat_map(|held| &held.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| tab.title.clone())
        .collect()
}

/// What `device` holds of its workspaces' hosts and logged of joining and syncing them.
fn syncing(device: &Device) -> String {
    let hosts = device.ask(&json!({"op": "keeping_devices"}));
    let log = device.log();
    let said: Vec<&str> = log
        .lines()
        .filter(|line| {
            [
                "join",
                "admit",
                "direct link",
                "link's",
                "frame",
                "history",
                "workspace_sync",
                "refus",
                "ownership",
                "take this device",
                "peer-sync",
                "dials its host",
                "device of the person",
                "own host",
                "layout",
                "ownership",
                "native",
                "link",
                "person's",
                "found",
                "WARN",
                "ERROR",
            ]
            .iter()
            .any(|word| line.contains(word))
        })
        .filter(|line| {
            !line.contains("moq_net") && !line.contains("mDNS") && !line.contains("retained=true")
        })
        .collect();
    let tail = said.len().saturating_sub(200);
    format!("hosts: {hosts}\n{}", said[tail..].join("\n"))
}

/// Start `laptop` and its copy and wait until the copy is admitted to `hosted`, the workspace the
/// laptop hosts. A restart moves the port the laptop's record names; the copy reaches it at the
/// relay the record names, which the laptop answers on while a link it handed out into the
/// workspace waits to be redeemed. On the boxes the two find each other on their network instead
/// (`e2e-branch`).
fn start_both(laptop: &mut Device, copy: &mut Device, hosted: &str) {
    laptop.start();
    copy.start();
    let until = tilo_test_env::hang_deadline();
    loop {
        let rows = copy.ask(&json!({"op": "keeping_devices"}));
        let admitted = rows["answer"]["workspaces"].as_array().is_some_and(|held| {
            held.iter().any(|row| {
                row["workspace"].as_str() == Some(hosted) && row["awaiting_host"] == false
            })
        });
        if admitted {
            break;
        }
        assert!(
            std::time::Instant::now() < until,
            "{} was never admitted to {hosted}, which {} hosts\n--- {} ---\n{}\n--- {} ---\n{}",
            copy.label,
            laptop.label,
            laptop.label,
            syncing(laptop),
            copy.label,
            syncing(copy),
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Wait until `reader` shows the window `title` that `writer` opened in `workspace`, saying on a
/// miss what each shell shows and what each session logged of syncing it.
fn wait_reaches(writer: &Device, reader: &Device, workspace: &str, title: &str) {
    let until = tilo_test_env::hang_deadline();
    while !titles_in(reader, workspace)
        .iter()
        .any(|shown| shown == title)
    {
        assert!(
            std::time::Instant::now() < until,
            "{title:?}, opened on {}, never showed on {} in {workspace}: {} shows {:?}, {} shows \
             {:?}\n--- {} ---\n{}\n--- {} ---\n{}",
            writer.label,
            reader.label,
            writer.label,
            titles_in(writer, workspace),
            reader.label,
            titles_in(reader, workspace),
            writer.label,
            syncing(writer),
            reader.label,
            syncing(reader),
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// A workspace the source made and hosts itself, and the machine copied from it, edited each way
/// after each restart of both: an edit on either always reaches the other (`e2e-branch` S09 and
/// S11, which missed one direction once in a while after a reboot). `TILO_SYNC_ROUNDS` runs more
/// rounds than the suite's three.
#[test]
fn a_workspace_its_source_hosts_and_a_copy_edit_each_other_after_every_restart() {
    let rounds: usize = std::env::var("TILO_SYNC_ROUNDS")
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(3);
    // No site: as on the boxes, every workspace the two hold is one the laptop hosts.
    let ada = Person::new("Ada");
    let mut laptop = ada.device("Ada's laptop");
    laptop.start();
    // A workspace the laptop makes and hosts before it is copied, as box hosts "Branch e2e".
    let before = laptop.on_shell();
    let made = laptop.act_wait(&new_workspace(), &json!({}));
    assert_eq!(made["update"]["state"], json!("succeeded"), "{made}");
    let until = tilo_test_env::hang_deadline();
    while laptop.on_shell().len() <= before.len() {
        assert!(
            Instant::now() < until,
            "the laptop's session never held the workspace it made\n{}",
            syncing(&laptop),
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let hosted = laptop
        .on_shell()
        .into_iter()
        .find(|workspace| !before.contains(workspace))
        .expect("the workspace the laptop made");
    show_held(&laptop, &hosted);
    {
        let _before = laptop.window(NOTES, "Before the copy");
        tilo_test_env::wait_until("the laptop shows its window", || {
            titles_in(&laptop, &hosted)
                .iter()
                .any(|title| title == "Before the copy")
        });
    }
    // A link into the workspace waits for Bob: the laptop stays reachable at its server through every
    // restart, as the boxes are on their network.
    let _out = shared_invitation(&laptop, "editor", Person::new("Bob").public);
    laptop.stop();
    let (mut copy, _) = ada.branch(&laptop);
    start_both(&mut laptop, &mut copy, &hosted);
    for round in 0..rounds {
        for device in [&laptop, &copy] {
            show_held(device, &hosted);
        }
        let from_laptop = format!("Laptop {round}");
        let _laptop_window = laptop.window(NOTES, &from_laptop);
        wait_reaches(&laptop, &copy, &hosted, &from_laptop);
        let from_copy = format!("Copy {round}");
        let _copy_window = copy.window(NOTES, &from_copy);
        wait_reaches(&copy, &laptop, &hosted, &from_copy);
        drop((_laptop_window, _copy_window));
        laptop.stop();
        copy.stop();
        start_both(&mut laptop, &mut copy, &hosted);
    }
}

/// A copy keeps reaching the workspace its source hosts however much it writes there: each window
/// it opens shows on its source, past the writes its ownership holds at once (`MAX_ENTRIES`, 64).
/// The copy's writes stayed in its ownership for good and, once that was full, a new one never left
/// it (`e2e-branch` S11).
#[test]
fn a_copy_keeps_reaching_the_workspace_its_source_hosts_past_what_its_ownership_holds() {
    let ada = Person::new("Ada");
    let mut laptop = ada.device("Ada's laptop");
    laptop.start();
    let before = laptop.on_shell();
    let made = laptop.act_wait(&new_workspace(), &json!({}));
    assert_eq!(made["update"]["state"], json!("succeeded"), "{made}");
    tilo_test_env::wait_until("the laptop's session holds the workspace it made", || {
        laptop.on_shell().len() > before.len()
    });
    let hosted = laptop
        .on_shell()
        .into_iter()
        .find(|workspace| !before.contains(workspace))
        .expect("the workspace the laptop made");
    show_held(&laptop, &hosted);
    {
        let _before = laptop.window(NOTES, "Before the copy");
        tilo_test_env::wait_until("the laptop shows its window", || {
            titles_in(&laptop, &hosted)
                .iter()
                .any(|title| title == "Before the copy")
        });
    }
    let _out = shared_invitation(&laptop, "editor", Person::new("Bob").public);
    laptop.stop();
    let (mut copy, _) = ada.branch(&laptop);
    start_both(&mut laptop, &mut copy, &hosted);
    for device in [&laptop, &copy] {
        show_held(device, &hosted);
    }
    for round in 0..80 {
        let title = format!("Copy {round}");
        let _window = copy.window(NOTES, &title);
        wait_reaches(&copy, &laptop, &hosted, &title);
    }
    // Neither device refused a write of the other's to their person's own store: a write a link's
    // catch-up already carries is not sent after it, where its sibling would refuse it as a replay.
    for device in [&laptop, &copy] {
        let log = device.log();
        assert!(
            !log.contains("a peer's write was refused"),
            "{} refused a write of its person's own store:\n{}",
            device.label,
            device.warnings()
        );
    }
}

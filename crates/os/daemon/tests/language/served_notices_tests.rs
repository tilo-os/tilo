//! A notice an app raises on a device with no desktop session: it is kept in the record of the
//! person's devices, where every device of theirs and every page raises it, and what they answer
//! there is carried out on the device it was raised on.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;

use super::mount;

use tilolang_workspace::records::{Packages, member, with_tl_record};
use tilolang_workspace::runtime::Value;

use tilo_workspace_service::apps::served_notices::ServedNotices;
use tilo_workspace_service::tl_records::{Compiled, Package};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, declare_app, hosting_device, test_host_holding,
};
use tilo_workspace_service::workspaces::person_workspace::{
    self,
    devices::{DEVICES, Devices},
    read_on,
};

/// The app that tells its person something: a notice with one action they may choose.
const APP: &str = "test.notice";
const SOURCE: &str = r#"app test.notice "Notices"

type Memo {
    done: Bool = false
}

root Memo

default view memo for Memo

view memo(m: Memo) {
    text("A memo")
}

action mark(m: Memo) (label: "Mark done", icon: .check) {
    m.done = true
}

action tell(m: Memo) uses notify {
    notify(to: invoker, title: "It is time", body: "Anything", opens: m, on: ["mark"])
}
"#;

/// Run `tell` on the app's record of `workspace`, as the person: the notice the app raises there.
async fn raise(workspaces: &Workspaces, workspace: &str, packages: &Packages, person: &str) {
    let who = member(tilo_actions::MemberId::person(person.to_owned()));
    with_tl_record(
        workspaces,
        workspace,
        packages,
        (who.clone(), person),
        move |opened| {
            let root = opened.open_root()?;
            opened
                .invoke(&who, (APP, "tell"), vec![Value::resource(&root)], None)
                .map(drop)
        },
    )
    .await
    .expect("the app tells its person");
}

/// Whether the memo the app's record holds is done, read through the app's own package.
async fn done(workspaces: &Workspaces, workspace: &str, packages: &Packages, person: &str) -> bool {
    let who = member(tilo_actions::MemberId::person(person.to_owned()));
    with_tl_record(
        workspaces,
        workspace,
        packages,
        (who.clone(), person),
        move |opened| {
            let root = opened.open_root()?;
            match opened.host().runtime().read(&root, "done") {
                Ok(Value::Bool(done)) => Ok(done),
                other => Err(format!("the memo's done does not read: {other:?}")),
            }
        },
    )
    .await
    .expect("the memo reads")
}

/// The one notice the record of the person's devices holds, by its id there.
async fn shown(
    workspaces: &Workspaces,
    own: &str,
) -> (String, person_workspace::devices::SharedNotice) {
    let record = read_on::<Devices>(workspaces, own, DEVICES)
        .await
        .expect("the record of their devices reads");
    let held: Vec<(String, _)> = record
        .notices
        .iter()
        .map(|(id, notice)| (id.as_str().to_owned(), notice.clone()))
        .collect();
    assert_eq!(held.len(), 1, "one notice is kept: {held:?}");
    held.into_iter().next().unwrap()
}

// USER REQUIREMENT: an app running in a device with no desktop session tells its person
// something, and every device of theirs and every page of theirs raises it; dismissing it on one
// of them takes it off all of them, and choosing its action runs that action of the app.
#[tokio::test]
async fn a_notice_raised_in_the_daemon_follows_the_person_and_their_answers_reach_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let desk = test_host_holding(dir.path(), &key, &cert, Arc::new(TestGroupKeys::default())).await;
    let workspaces = desk.workspaces().clone();
    mount(&workspaces);
    let own = person_workspace::found(&workspaces)
        .await
        .expect("the person's own workspace is founded");
    let person = hex::encode(cert.user_public_key);
    let workspace = workspaces
        .create("Work")
        .await
        .expect("a workspace is made")
        .workspace;
    assert!(
        workspaces.join_keys(&workspace).await,
        "the workspace's keys open what its records are kept in"
    );
    let packages = Packages::compile(&[(SOURCE, "")]).expect("the app's sources compile");
    declare_app(
        &desk,
        APP,
        &packages.surface(APP).expect("the app declares its surface"),
    );
    raise(&workspaces, &workspace, &packages, &person).await;

    let package = Package::Compiled(Compiled::new(packages.clone()));
    let mut served = ServedNotices::new(
        workspaces.clone(),
        cert.device_public_key,
        cert.user_public_key,
    );
    let shown_count = served
        .project(&workspace, APP, "Notices", Some(&package), 1_000)
        .await
        .expect("the notice is kept");
    assert_eq!(shown_count, 1);

    // The record every device of the person's reads and every page raises it from.
    let (id, notice) = shown(&workspaces, &own).await;
    assert_eq!(
        notice.from,
        hex::encode(cert.device_public_key),
        "it is from this device"
    );
    assert_eq!(
        (notice.app_id.as_str(), notice.app_name.as_str()),
        (APP, "Notices")
    );
    assert_eq!(
        (notice.title.as_str(), notice.body.as_str()),
        ("It is time", "Anything")
    );
    assert_eq!(
        notice.actions,
        vec![person_workspace::devices::NoticeAction {
            key: "mark".into(),
            label: "Mark done".into(),
        }],
        "the action the app offered, labelled as it labels it"
    );
    assert!(notice.shown_at(2_000), "it is shown while it holds");
    assert!(
        served
            .project(&workspace, APP, "Notices", Some(&package), 1_001)
            .await
            .expect("a second look keeps it once")
            == 0,
        "one notice is kept once on the device that sent it"
    );

    // A page of another of their devices dismisses it: the device it was sent on carries that
    // out, takes it off every device of theirs, and marks the app's notice seen.
    let page_device = [9; 32];
    person_workspace::apply_on(
        &workspaces,
        &own,
        DEVICES,
        Devices::answering_notice(&id, &page_device, ""),
    )
    .await
    .expect("the page's person dismisses it");
    assert!(
        served
            .follow(2_000)
            .await
            .expect("the answer is read")
            .is_empty(),
        "a dismissal chooses no action"
    );
    let record = read_on::<Devices>(&workspaces, &own, DEVICES)
        .await
        .expect("the record reads");
    assert!(
        record.notices.is_empty(),
        "an answered notice leaves every device of theirs"
    );
    assert!(
        workspaces
            .app_notices(&workspace, APP, Some(&package), &person)
            .await
            .expect("the app's record reads")
            .is_empty(),
        "the app's notice is seen, so no device raises it again"
    );

    // Another notice, and this time the person chooses the action it offers on another device:
    // that action of the app runs where the notice was sent.
    raise(&workspaces, &workspace, &packages, &person).await;
    assert!(
        served
            .project(&workspace, APP, "Notices", Some(&package), 3_000)
            .await
            .expect("the second notice is kept")
            == 1
    );
    let (id, _) = shown(&workspaces, &own).await;
    person_workspace::apply_on(
        &workspaces,
        &own,
        DEVICES,
        Devices::answering_notice(&id, &page_device, "mark"),
    )
    .await
    .expect("the page's person chooses Mark done");
    let carried = served.follow(4_000).await.expect("the answer is read");
    assert_eq!(carried.len(), 1, "one notice was carried out: {carried:?}");
    assert_eq!(
        (carried[0].app.as_str(), carried[0].action.as_str()),
        (APP, "mark")
    );
    assert!(
        done(&workspaces, &workspace, &packages, &person).await,
        "the app's action ran on what the notice opened"
    );
    let record = read_on::<Devices>(&workspaces, &own, DEVICES)
        .await
        .expect("the record reads");
    assert!(
        record.notices.is_empty(),
        "a chosen notice leaves every device of theirs too"
    );
    desk.shutdown().await;
}

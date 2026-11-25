//! An alarm set for one of the person's devices rings there from the platform at its moment,
//! with no network: the device reads the app's trigger (`at a.at on a.rings_on`) from its own copy
//! of the record, arms a wakeup with what the trigger's run will tell, moves it as the alarm
//! moves, takes it back when the alarm is off, and shows the workspace's notice no second time
//! once the device that runs the trigger writes it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::mount;

use tilolang_workspace::records::{Packages, member, with_tl_record};
use tilolang_workspace::runtime::{ResourceRef, Value};

use tilo_workspace_service::apps::served_notices::ServedNotices;
use tilo_workspace_service::apps::wakeups::{
    Notification, Notifier, NotifyError, Scheduled, SystemTime, Wakeups,
};
use tilo_workspace_service::tl_records::{Compiled, Package};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, declare_app, governor, test_host_holding,
};
use tilo_workspace_service::workspaces::person_workspace;

pub(super) const APP: &str = "so.tilo.clocks";
pub(super) const CLOCKS: (&str, &str) = (
    include_str!("../../../../../apps/tl/clocks/app.tl"),
    include_str!("../../../../../apps/tl/clocks/app.ids"),
);

/// The platform's notifications, as a phone's are: what is armed for later, and what it showed.
/// [`Platform::ring`] is the moment coming, which the platform rings with nothing else running.
#[derive(Default)]
struct Platform {
    armed: Mutex<BTreeMap<String, (Notification, SystemTime)>>,
    shown: Mutex<Vec<Notification>>,
}

impl Platform {
    fn armed(&self) -> Vec<(Notification, SystemTime)> {
        self.armed.lock().unwrap().values().cloned().collect()
    }

    /// The moment of the wakeup `tag` came: the platform shows what was armed.
    fn ring(&self, tag: &str) {
        let (notification, _) = self.armed.lock().unwrap().remove(tag).expect("it is armed");
        self.shown.lock().unwrap().push(notification);
    }
}

impl Notifier for Platform {
    fn show(&self, notification: &Notification) -> Result<(), NotifyError> {
        self.shown.lock().unwrap().push(notification.clone());
        Ok(())
    }

    fn withdraw(&self, _tag: &str) -> Result<(), NotifyError> {
        Ok(())
    }

    fn schedule(&self, notification: &Notification, at: SystemTime) -> Result<(), NotifyError> {
        self.armed
            .lock()
            .unwrap()
            .insert(notification.tag.clone(), (notification.clone(), at));
        Ok(())
    }

    fn scheduled(&self) -> Result<Vec<Scheduled>, NotifyError> {
        Ok(self
            .armed
            .lock()
            .unwrap()
            .iter()
            .map(|(tag, (_, at))| Scheduled {
                tag: tag.clone(),
                at: *at,
            })
            .collect())
    }

    fn cancel_scheduled(&self, tag: &str) -> Result<(), NotifyError> {
        self.armed.lock().unwrap().remove(tag);
        Ok(())
    }
}

/// Run Clocks' `action` on what `args` makes as the person, in the record of `workspace`: the
/// resource it answered, if it answered one. A record's values stay on the thread that opened it,
/// so the arguments are made there.
async fn clocks(
    workspaces: &Workspaces,
    workspace: &str,
    packages: &Packages,
    person: &str,
    (action, args): (&'static str, impl FnOnce() -> Vec<Value> + Send + 'static),
) -> Option<ResourceRef> {
    let who = member(tilo_actions::MemberId::person(person.to_owned()));
    with_tl_record(
        workspaces,
        workspace,
        packages,
        (who.clone(), person),
        move |opened| {
            let args = if action == "add_alarm" {
                let root = opened.open_root()?;
                std::iter::once(Value::resource(&root))
                    .chain(args())
                    .collect()
            } else {
                args()
            };
            let answered = opened.invoke(&who, (APP, action), args, None)?;
            Ok(match answered {
                Value::Ref(made) => Some(ResourceRef::clone(&made.target)),
                _ => None,
            })
        },
    )
    .await
    .unwrap_or_else(|error| panic!("{action}: {error}"))
}

/// An alarm at `hour`:`minute`, once, ringing on `rings_on`.
pub(super) async fn alarm(
    workspaces: &Workspaces,
    workspace: &str,
    packages: &Packages,
    person: &str,
    (label, hour, minute, rings_on): (&str, i64, i64, &str),
) -> ResourceRef {
    let (label, rings_on) = (label.to_owned(), rings_on.to_owned());
    clocks(
        workspaces,
        workspace,
        packages,
        person,
        ("add_alarm", move || {
            vec![
                Value::text(label),
                Value::Int(hour),
                Value::Int(minute),
                Value::List(Vec::new()),
                Value::text(rings_on),
            ]
        }),
    )
    .await
    .expect("add_alarm answers the alarm")
}

fn ms(at: SystemTime) -> i64 {
    i64::try_from(
        at.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

// USER REQUIREMENT: an alarm set for my phone rings on my phone at its time even when the phone
// is offline or the app is closed, says what the alarm says and offers its snooze, follows the
// alarm when it is moved or turned off, and does not ring a second time when the phone hears
// from the rest of the workspace again.
#[tokio::test]
async fn an_alarm_for_this_device_rings_from_its_own_wakeup_and_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let desk = test_host_holding(dir.path(), &key, &cert, Arc::new(TestGroupKeys::default())).await;
    let workspaces = desk.workspaces().clone();
    mount(&workspaces);
    person_workspace::found(&workspaces)
        .await
        .expect("the person's own workspace is founded");
    let person = hex::encode(cert.user_public_key);
    let here = hex::encode(cert.device_public_key);
    let workspace = workspaces
        .create("Home")
        .await
        .expect("a workspace is made")
        .workspace;
    assert!(workspaces.join_keys(&workspace).await);
    let packages = Packages::compile(&[CLOCKS]).expect("Clocks compiles");
    declare_app(
        &desk,
        APP,
        &packages.surface(APP).expect("Clocks declares its surface"),
    );
    // The person installs Clocks, which gives it its own records: its trigger writes the notice.
    tilo_workspace_service::workspaces::fixtures::install_app(&workspaces, &workspace, APP)
        .await
        .expect("the person installs Clocks");
    let package = Package::Compiled(Compiled::new(packages.clone()));
    let platform = Arc::new(Platform::default());
    let mut wakeups = Wakeups::new(
        workspaces.clone(),
        Arc::clone(&platform) as Arc<dyn Notifier>,
        cert.device_public_key,
        cert.user_public_key,
    );
    let now = || tilo_clock::unix_now_ms_i64();

    // An alarm for another device of theirs arms nothing here.
    let gym = alarm(
        &workspaces,
        &workspace,
        &packages,
        &person,
        ("Gym", 6, 0, &"ab".repeat(32)),
    )
    .await;
    assert_eq!(
        wakeups
            .arm(&workspace, (APP, "Clocks"), Some(&package), now())
            .await
            .unwrap(),
        0
    );
    assert!(platform.armed().is_empty(), "the gym alarm rings elsewhere");
    // Off, it rings nowhere: what the workspace runs below is this device's alarm alone.
    clocks(
        &workspaces,
        &workspace,
        &packages,
        &person,
        ("set_alarm_enabled", move || {
            vec![Value::resource(&gym), Value::Bool(false)]
        }),
    )
    .await;

    // One for this device is armed with what its ring will say, and the snooze it offers.
    let standup = alarm(
        &workspaces,
        &workspace,
        &packages,
        &person,
        ("Standup", 7, 0, &here),
    )
    .await;
    assert_eq!(
        wakeups
            .arm(&workspace, (APP, "Clocks"), Some(&package), now())
            .await
            .unwrap(),
        1
    );
    let armed = platform.armed();
    assert_eq!(armed.len(), 1, "{armed:#?}");
    let (notification, seven) = armed[0].clone();
    assert_eq!(
        (
            notification.title.as_str(),
            notification.body.as_str(),
            notification.app.as_deref()
        ),
        ("Standup", "It is 7:00", Some("Clocks"))
    );
    assert_eq!(notification.buttons.len(), 1);
    assert_eq!(notification.buttons[0].label, "Snooze 5 minutes");
    assert!(ms(seven) > now(), "its moment is still to come");
    assert_eq!(
        wakeups
            .arm(&workspace, (APP, "Clocks"), Some(&package), now())
            .await
            .unwrap(),
        0,
        "looking again arms nothing twice"
    );

    // Moved to half past, the same wakeup moves.
    clocks(
        &workspaces,
        &workspace,
        &packages,
        &person,
        ("edit_alarm", {
            let standup = standup.clone();
            move || {
                vec![
                    Value::resource(&standup),
                    Value::text("Standup"),
                    Value::Int(7),
                    Value::Int(30),
                    Value::List(Vec::new()),
                ]
            }
        }),
    )
    .await;
    wakeups
        .arm(&workspace, (APP, "Clocks"), Some(&package), now())
        .await
        .unwrap();
    let armed = platform.armed();
    assert_eq!(armed.len(), 1, "moved, not added: {armed:#?}");
    assert_eq!(armed[0].0.tag, notification.tag, "the same wakeup");
    assert_eq!(armed[0].0.body, "It is 7:30");
    let day = 24 * 60 * 60 * 1000;
    assert_eq!(
        (ms(armed[0].1) - ms(seven)).rem_euclid(day),
        30 * 60 * 1000,
        "half an hour on"
    );

    // Turned off, it is taken back; on again, armed again.
    let off = |enabled: bool| {
        let standup = standup.clone();
        ("set_alarm_enabled", move || {
            vec![Value::resource(&standup), Value::Bool(enabled)]
        })
    };
    clocks(&workspaces, &workspace, &packages, &person, off(false)).await;
    wakeups
        .arm(&workspace, (APP, "Clocks"), Some(&package), now())
        .await
        .unwrap();
    assert!(
        platform.armed().is_empty(),
        "an alarm that is off rings nowhere"
    );
    clocks(&workspaces, &workspace, &packages, &person, off(true)).await;
    wakeups
        .arm(&workspace, (APP, "Clocks"), Some(&package), now())
        .await
        .unwrap();
    let (notification, at) = platform.armed()[0].clone();

    // Its moment comes and the platform rings it, with nothing of the workspace reached.
    platform.ring(&notification.tag);
    let shown = platform.shown.lock().unwrap().clone();
    assert_eq!(shown.len(), 1);
    assert_eq!(
        (shown[0].title.as_str(), shown[0].body.as_str()),
        ("Standup", "It is 7:30")
    );
    let rang = wakeups.look();
    assert_eq!(rang.len(), 1, "the device knows it rang: {rang:?}");

    // The device that runs the trigger rings the alarm for the workspace at that moment and
    // writes the notice, as Clocks, which keeps its records since the person installed it; the
    // device that rang it already shows it no second time, where one that did not would.
    let who = member(tilo_actions::MemberId::person(person.clone()));
    let ran = with_tl_record(
        &workspaces,
        &workspace,
        &packages,
        (who, &person),
        move |opened| Ok(opened.host().run_triggers(ms(at) / 1000)),
    )
    .await
    .unwrap();
    assert_eq!(ran, 1, "the trigger runs once for the workspace");
    let raised = workspaces
        .app_notices(&workspace, APP, Some(&package), &person)
        .await
        .unwrap();
    assert_eq!(raised.len(), 1, "{raised:#?}");
    assert_eq!(
        raised[0].doc, rang[0].2,
        "the notice the device rang, by its record"
    );
    let mut served = ServedNotices::new(
        workspaces.clone(),
        cert.device_public_key,
        cert.user_public_key,
    );
    served.rang_here(rang);
    assert_eq!(
        served
            .project(&workspace, APP, "Clocks", Some(&package), now())
            .await
            .unwrap(),
        0,
        "the person is told once"
    );
    let mut elsewhere = ServedNotices::new(
        workspaces.clone(),
        cert.device_public_key,
        cert.user_public_key,
    );
    assert_eq!(
        elsewhere
            .project(&workspace, APP, "Clocks", Some(&package), now())
            .await
            .unwrap(),
        1,
        "a device that did not ring it shows it"
    );
    desk.shutdown().await;
}

// USER REQUIREMENT: pressing snooze on the alarm that rang on my phone snoozes it there, with no
// network, and it rings again five minutes on.
#[tokio::test]
async fn a_wakeups_snooze_runs_on_the_alarm_with_no_network() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let desk = test_host_holding(dir.path(), &key, &cert, Arc::new(TestGroupKeys::default())).await;
    let workspaces = desk.workspaces().clone();
    mount(&workspaces);
    person_workspace::found(&workspaces).await.unwrap();
    let person = hex::encode(cert.user_public_key);
    let here = hex::encode(cert.device_public_key);
    let workspace = workspaces.create("Home").await.unwrap().workspace;
    assert!(workspaces.join_keys(&workspace).await);
    let packages = Packages::compile(&[CLOCKS]).unwrap();
    declare_app(&desk, APP, &packages.surface(APP).unwrap());
    let package = Package::Compiled(Compiled::new(packages.clone()));
    let platform = Arc::new(Platform::default());
    let mut wakeups = Wakeups::new(
        workspaces.clone(),
        Arc::clone(&platform) as Arc<dyn Notifier>,
        cert.device_public_key,
        cert.user_public_key,
    );
    let now = tilo_clock::unix_now_ms_i64();
    alarm(
        &workspaces,
        &workspace,
        &packages,
        &person,
        ("Standup", 7, 0, &here),
    )
    .await;
    wakeups
        .arm(&workspace, (APP, "Clocks"), Some(&package), now)
        .await
        .unwrap();
    let (notification, _) = platform.armed()[0].clone();
    platform.ring(&notification.tag);
    wakeups.look();

    // A button naming no wakeup of this device's runs nothing.
    assert!(
        wakeups
            .chosen("tilo-wakeup:w00000000000000?snooze_alarm@1")
            .await
            .is_err()
    );
    wakeups
        .chosen(&notification.buttons[0].activation)
        .await
        .expect("the snooze runs");
    let before = tilo_clock::unix_now_ms_i64();
    wakeups
        .arm(&workspace, (APP, "Clocks"), Some(&package), before)
        .await
        .unwrap();
    let armed = platform.armed();
    assert_eq!(armed.len(), 1, "snoozed, it is armed again: {armed:#?}");
    let in_ms = ms(armed[0].1) - before;
    assert!(
        (4 * 60 * 1000..=5 * 60 * 1000 + 2_000).contains(&in_ms),
        "five minutes on: {in_ms} ms"
    );
    desk.shutdown().await;
}

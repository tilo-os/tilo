//! Which device keeps an app's long-lived connection ([`Turn::Connection`]): two devices of one
//! workspace, each running the trigger runner that takes connection turns, with an app host that
//! records the process it starts and stops.
#![allow(
    clippy::large_futures,
    clippy::significant_drop_tightening,
    reason = "a fixture failing is its test failing, and each test keeps both devices to the end"
)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::ActionCallError;
use tilo_automation::{AutomationInvocationId, InvocationReport};
use tilo_user_identity::DeviceCapabilities;

use super::super::Workspaces;
use super::super::fixtures::{TestHost, device_cert};
use super::tests::{member_service, shared_with};
use crate::apps::triggers::{Firing, Runner, StartRun};
use crate::work::{
    HostConnections, Ineligible, NotTaken, Order, Turn, connection_holder, takes_turn,
};

const BRIDGE: &str = "so.tilo.bridge";
const NEEDS: [&str; 2] = ["network.http", "secrets"];

/// The order both devices keep here: no device waits for another, so each look decides.
const AT_ONCE: Order = Order {
    server_grace: Duration::ZERO,
    other_grace: Duration::ZERO,
};

/// What a device has for the bridge, and the bridge processes it runs.
#[derive(Default)]
struct Device {
    has: Mutex<Has>,
    running: Mutex<BTreeSet<(String, String)>>,
    starts: Mutex<u32>,
}

#[derive(Clone, Copy, Default)]
struct Has {
    installed: bool,
    capabilities: bool,
    power: bool,
}

impl Has {
    const ALL: Self = Self {
        installed: true,
        capabilities: true,
        power: true,
    };
}

impl Device {
    fn with(has: Has) -> Arc<Self> {
        let device = Arc::new(Self::default());
        *device.has.lock().unwrap() = has;
        device
    }

    fn runs_bridge(&self) -> bool {
        !self.running.lock().unwrap().is_empty()
    }
}

impl HostConnections for Device {
    fn connections(&self) -> Vec<(String, Vec<String>)> {
        if self.has.lock().unwrap().installed {
            vec![(BRIDGE.to_owned(), NEEDS.map(str::to_owned).to_vec())]
        } else {
            Vec::new()
        }
    }
    fn installed(&self, app: &str) -> bool {
        app == BRIDGE && self.has.lock().unwrap().installed
    }
    fn gives(&self, _: &str, _: &str, capability: &str) -> bool {
        NEEDS.contains(&capability) && self.has.lock().unwrap().capabilities
    }
    fn on_power(&self) -> bool {
        self.has.lock().unwrap().power
    }
    fn start(&self, workspace: &str, app: &str) -> Result<(), String> {
        *self.starts.lock().unwrap() += 1;
        self.running
            .lock()
            .unwrap()
            .insert((workspace.to_owned(), app.to_owned()));
        Ok(())
    }
    fn runs(&self, workspace: &str, app: &str) -> bool {
        self.running
            .lock()
            .unwrap()
            .contains(&(workspace.to_owned(), app.to_owned()))
    }
    fn stop(&self, workspace: &str, app: &str) {
        self.running
            .lock()
            .unwrap()
            .remove(&(workspace.to_owned(), app.to_owned()));
    }
}

/// No shortcut fires in these tests.
struct NoRuns;

impl StartRun for NoRuns {
    fn start(&self, _: &Firing, _: AutomationInvocationId) -> Result<u64, ActionCallError> {
        Err(ActionCallError::Unavailable {
            reason: "no shortcut runs here".into(),
        })
    }
    fn ended(&self, _: &Firing, _: AutomationInvocationId) -> Option<InvocationReport> {
        None
    }
}

fn runner(workspaces: &Workspaces) -> Runner {
    Runner::new(workspaces.clone(), Arc::new(NoRuns), AT_ONCE)
}

/// The root of the person both devices are of.
fn persons_root() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

/// A desktop with a workspace and a laptop in it, each running the bridge host `hosts` names.
async fn two_devices(
    states: (&std::path::Path, &std::path::Path),
    seed: u8,
    hosts: (&Arc<Device>, &Arc<Device>),
) -> (TestHost, TestHost, String) {
    let laptop = device_cert(&persons_root(), "laptop", DeviceCapabilities::member());
    let (desktop, laptop, created, invitation) = shared_with(states.0, "Bridged", seed, laptop).await;
    let laptop = member_service(states.1, laptop).await;
    laptop.workspaces().join_member(&invitation).await.unwrap();
    desktop
        .workspaces()
        .run_connections(Arc::clone(hosts.0) as Arc<dyn HostConnections>);
    laptop
        .workspaces()
        .run_connections(Arc::clone(hosts.1) as Arc<dyn HostConnections>);
    (desktop, laptop, created.workspace)
}

/// The device the workspace, as `workspaces` reads it, says holds the bridge's lease.
async fn holder(workspaces: &Workspaces, workspace: &str) -> Option<[u8; 32]> {
    connection_holder(workspaces, workspace, BRIDGE)
        .await
        .unwrap()
}

/// Wait until `workspaces` reads `device` holding the bridge's lease.
async fn reads_holder(workspaces: &Workspaces, workspace: &str, device: [u8; 32]) {
    let (workspaces, workspace) = (workspaces.clone(), workspace.to_owned());
    tilo_test_env::within("the claim reaches the other device", async move {
        while holder(&workspaces, &workspace).await != Some(device) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// The bridge runs on the device that has it installed, gives it what it declares it needs and
/// is on power; a device that lacks any of them says why, and never claims or starts it.
#[tokio::test]
async fn a_connection_runs_only_on_a_device_that_has_what_the_app_needs() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (desktop_has, laptop_has) = (Device::with(Has::default()), Device::with(Has::ALL));
    let (desktop, laptop, workspace) =
        two_devices((one.path(), two.path()), 51, (&desktop_has, &laptop_has)).await;
    let laptop_device = laptop.workspaces().lease_device(&workspace).await;
    let turn = Turn::connection(BRIDGE, NEEDS.map(str::to_owned).to_vec());

    let mut on_desktop = runner(desktop.workspaces());
    on_desktop.pass(Vec::new()).await;
    assert!(on_desktop.connections_held().is_empty(), "nothing is installed there");
    assert_eq!(
        takes_turn(desktop.workspaces(), &workspace, &turn).await.err(),
        Some(NotTaken::Ineligible(Ineligible::NotInstalled))
    );
    desktop_has.has.lock().unwrap().installed = true;
    assert_eq!(
        takes_turn(desktop.workspaces(), &workspace, &turn).await.err(),
        Some(NotTaken::Ineligible(Ineligible::LacksCapability))
    );
    desktop_has.has.lock().unwrap().capabilities = true;
    assert_eq!(
        takes_turn(desktop.workspaces(), &workspace, &turn).await.err(),
        Some(NotTaken::Ineligible(Ineligible::OnBattery))
    );
    on_desktop.pass(Vec::new()).await;
    assert!(
        on_desktop.connections_held().is_empty() && !desktop_has.runs_bridge(),
        "a device on its battery leaves the connection to another"
    );
    assert_eq!(*desktop_has.starts.lock().unwrap(), 0);
    assert_eq!(holder(desktop.workspaces(), &workspace).await, None, "no claim was written");

    let mut on_laptop = runner(laptop.workspaces());
    on_laptop.pass(Vec::new()).await;
    assert_eq!(on_laptop.connections_held(), [(workspace.clone(), BRIDGE.to_owned())]);
    assert!(laptop_has.runs_bridge(), "the laptop runs the bridge's process");
    reads_holder(desktop.workspaces(), &workspace, laptop_device).await;

    // Plugged in, the desktop could run it too, but the laptop holds it.
    desktop_has.has.lock().unwrap().power = true;
    on_desktop.pass(Vec::new()).await;
    assert!(on_desktop.connections_held().is_empty() && !desktop_has.runs_bridge());

    // Once the laptop runs on its battery it lets the turn go at its next renewal, and the
    // desktop takes it.
    laptop_has.has.lock().unwrap().power = false;
    let wall = tilo_clock::testing::SharedWall::default();
    wall.follow();
    wall.pin(tilo_clock::wall() + Duration::from_secs(16));
    on_laptop.pass(Vec::new()).await;
    assert!(on_laptop.connections_held().is_empty() && !laptop_has.runs_bridge());
    let desktop_device = desktop.workspaces().lease_device(&workspace).await;
    tilo_test_env::within("the desktop takes the connection the laptop let go", async {
        while !desktop_has.runs_bridge() {
            on_desktop.pass(Vec::new()).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    wall.release();
    assert_eq!(holder(desktop.workspaces(), &workspace).await, Some(desktop_device));
    assert!(!laptop_has.runs_bridge(), "never on both");
}

/// Both devices can run the bridge; exactly one holds it. When that device goes silent (asleep,
/// offline), the other takes the connection over once its claim is over, and the first stops its
/// process the moment it looks again, before it could run beside the new holder.
#[tokio::test]
async fn a_silent_holder_is_taken_over_and_stops_its_connection_when_it_resumes() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (desktop_has, laptop_has) = (Device::with(Has::ALL), Device::with(Has::ALL));
    let (desktop, laptop, workspace) =
        two_devices((one.path(), two.path()), 52, (&desktop_has, &laptop_has)).await;
    let desktop_device = desktop.workspaces().lease_device(&workspace).await;
    let laptop_device = laptop.workspaces().lease_device(&workspace).await;
    let (mut on_desktop, mut on_laptop) = (runner(desktop.workspaces()), runner(laptop.workspaces()));

    on_desktop.pass(Vec::new()).await;
    assert!(desktop_has.runs_bridge(), "the first device to look takes it");
    reads_holder(laptop.workspaces(), &workspace, desktop_device).await;
    for _ in 0..3 {
        on_laptop.pass(Vec::new()).await;
        on_desktop.pass(Vec::new()).await;
        assert!(on_laptop.connections_held().is_empty() && !laptop_has.runs_bridge());
        assert!(desktop_has.runs_bridge(), "the holder keeps it");
    }

    // The desktop goes silent: it looks no more, and its process is left as it was. A minute
    // after its last renewal, its claim is over.
    let wall = tilo_clock::testing::SharedWall::default();
    wall.follow();
    let silent = tilo_clock::wall()
        + Duration::from_nanos(tilo_peer_state::CLOCK_SKEW_NS)
        + Duration::from_secs(1);
    wall.pin(silent);
    on_laptop.pass(Vec::new()).await;
    assert_eq!(on_laptop.connections_held(), [(workspace.clone(), BRIDGE.to_owned())]);
    assert!(laptop_has.runs_bridge(), "the laptop took the connection over");
    reads_holder(desktop.workspaces(), &workspace, laptop_device).await;

    // The desktop wakes: before anything else, it stops the connection it no longer holds, and
    // looking again it finds the laptop holding it.
    on_desktop.pass(Vec::new()).await;
    assert!(!desktop_has.runs_bridge(), "a device that resumes stops the connection");
    for _ in 0..3 {
        on_desktop.pass(Vec::new()).await;
        on_laptop.pass(Vec::new()).await;
        assert!(on_desktop.connections_held().is_empty() && !desktop_has.runs_bridge());
        assert!(laptop_has.runs_bridge());
    }
    wall.release();
    assert_eq!(holder(desktop.workspaces(), &workspace).await, Some(laptop_device));
    assert_eq!(*desktop_has.starts.lock().unwrap(), 1, "the desktop started it once");
}

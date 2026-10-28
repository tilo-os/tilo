//! A device hosting its workspace with nobody connected runs nothing
//! periodic: its embedded host wakes only on I/O. Measured as the lone idle
//! desktop's gate measures it (`idle_quiet_tests`), in a process and
//! network namespace of its own, by counting how often the process's
//! threads block over an idle window, with the host demonstrably open.

use std::sync::Arc;
use std::time::Instant;

use tilo_user_identity::DeviceCapabilities;

use super::super::idle_quiet_tests::{
    LONE_CPU_BUDGET, LONE_IDLE, LONE_SETTLE, LONE_WAKEUP_BUDGET, in_a_process_of_its_own,
    session_threads,
};
use super::{Device, Person, Wakes, hosted_workspace, scratch};

#[cfg(target_os = "linux")]
#[test]
#[serial_test::serial]
fn a_device_hosting_its_workspace_with_nobody_connected_wakes_nothing() {
    if in_a_process_of_its_own(concat!(
        module_path!(),
        "::a_device_hosting_its_workspace_with_nobody_connected_wakes_nothing"
    )) {
        return;
    }
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(71);
    home.env().set("TILO_USER_ID", ada.id());
    let device = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A41,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let (_, room) = hosted_workspace(&device, &[], "Home");
    assert_eq!(device.hosted_rooms(), vec![room], "its host is open");

    // Past the start's own work, then nobody touches it: its compositor is
    // parked, so not even a frame turn reaches the network. Every thread of
    // this process is the device's.
    std::thread::sleep(LONE_SETTLE);
    let before = session_threads(None);
    let started = Instant::now();
    std::thread::sleep(LONE_IDLE);
    let wakeups =
        tilo_test_env::idle::Wakeups::between(before, session_threads(None), started.elapsed());
    eprintln!("a lone hosting device's threads, {wakeups}");
    assert!(
        wakeups.blocking_per_second() <= LONE_WAKEUP_BUDGET,
        "a device hosting its workspace alone woke {:.2} times a second, over \
         {LONE_WAKEUP_BUDGET}/s:\n{wakeups}",
        wakeups.blocking_per_second(),
    );
    assert!(
        wakeups.total_cpu_percent() <= LONE_CPU_BUDGET,
        "a device hosting its workspace alone used {:.2} % of a core, over \
         {LONE_CPU_BUDGET} %:\n{wakeups}",
        wakeups.total_cpu_percent(),
    );
    assert_eq!(
        device.hosted_rooms(),
        vec![room],
        "the host stayed open throughout"
    );
    drop(device);
}

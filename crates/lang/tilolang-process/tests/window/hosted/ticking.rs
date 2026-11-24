//! A view's own clock (`every 1s: tick(c)`) runs on the device whose window shows the view, as
//! the person at that window: not on the workspace's origin, not on another device that has the
//! app open, and not while nobody can see the window.

use super::*;
use tilolang_process::testing::Bus;
use tilolang_tilo::runtime::{ResourceRef, Value as RtValue};
use ui::time::testing::with_monotonic_time;

const TICKING: &str = "so.tilo.lang.ticking";

impl Session {
    /// Who made each tick the clock took, as this device's copy of the workspace reads them.
    fn ticked_by(&self) -> Vec<String> {
        let host = self.instance_host();
        let runtime = host.runtime();
        let root = host.root_of(TICKING).expect("the clock");
        let RtValue::List(ticks) = runtime.read(&root, "ticks").expect("the ticks read") else {
            panic!("the ticks are a list");
        };
        ticks
            .into_iter()
            .map(|tick| {
                let RtValue::Ref(tick) = tick else {
                    panic!("a tick is a record");
                };
                let tick: &ResourceRef = &tick.target;
                match runtime.read(tick, "by").expect("who ticked reads") {
                    RtValue::Actor(by) => by.to_string(),
                    other => panic!("who ticked is a member, not {other:?}"),
                }
            })
            .collect()
    }
}

/// Both devices' hosts and windows at `at`, until neither owes anything.
fn settled_at(at: ui::time::Instant, devices: [&mut Session; 2]) {
    with_monotonic_time(at, || {
        for device in devices {
            device.settle();
        }
    });
}

/// Ada's device and Bo's both have the clock open; Bo presses Start. His window ticks it once a
/// second, as him; Ada's, which shows no clock running, never does, though her device runs the
/// same app over the same workspace. Bo's window hidden, his clock waits; seen again, it ticks
/// once for the time it was away.
#[test]
fn a_views_clock_ticks_on_the_device_whose_window_shows_it() {
    let bus = Rc::new(Bus::default());
    bus.member(ui::tilo_actions::MemberId::person("ada"), "Ada");
    bus.member(ui::tilo_actions::MemberId::person("bo"), "Bo");
    let route = RouteSpec::app(TICKING, uuid::Uuid::new_v4());
    let mut ada = Session::on_bus(&bus, &route, spec_of(&route, "ada"));
    let mut bo = Session::on_bus(&bus, &route, spec_of(&route, "bo"));
    let start = ui::time::now();
    let second = |n: u64| start + std::time::Duration::from_millis(n * 1000 + 50);

    settled_at(start, [&mut ada, &mut bo]);
    with_monotonic_time(start, || bo.window.press_named("Start"));
    settled_at(start, [&mut ada, &mut bo]);
    assert!(bo.ticked_by().is_empty(), "nothing ticks before its second");

    for n in 1..=3 {
        settled_at(second(n), [&mut bo, &mut ada]);
    }
    let bo_id = ui::tilo_actions::MemberId::person("bo").to_string();
    assert_eq!(
        bo.ticked_by(),
        vec![bo_id.clone(); 3],
        "Bo's window ticks the clock once a second, as him"
    );
    assert_eq!(
        ada.ticked_by(),
        vec![bo_id.clone(); 3],
        "Ada's device has the same ticks, and made none of them"
    );

    // Bo's window goes where he cannot see it: its clock waits.
    ui::widget::set_window_shown(false);
    for n in 4..=8 {
        settled_at(second(n), [&mut bo, &mut ada]);
    }
    assert_eq!(
        bo.ticked_by().len(),
        3,
        "a window nobody sees ticks nothing"
    );
    ui::widget::set_window_shown(true);
    settled_at(second(8), [&mut bo, &mut ada]);
    assert_eq!(
        ada.ticked_by(),
        vec![bo_id; 4],
        "seen again, it ticks once for the time it was away"
    );
}

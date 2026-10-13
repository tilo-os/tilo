//! What a connected accessibility bus costs a Settings click.
//!
//! The native runner hands the whole accessibility tree to AccessKit's Unix
//! adapter after every frame. The adapter ignores it until the bus reports
//! assistive technology enabled; then it diffs the tree and queues AT-SPI
//! events. This runs the hosted app against the real adapter and reports the
//! frame work with the bus absent and present.
//!
//! It needs a session bus with the registry, so it is ignored by default:
//!
//! ```sh
//! # A private runtime directory and settings backend keep the real session
//! # (its buses, its dconf) out of it.
//! export XDG_RUNTIME_DIR=$(mktemp -d) GSETTINGS_BACKEND=memory
//! dbus-run-session -- sh -c '
//!   /usr/libexec/at-spi-bus-launcher --launch-immediately & sleep 2
//!   TILO_A11Y_BUS=1 cargo test -p settings --test it a11y_bus -- --ignored --nocapture'
//! ```
#![allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::manual_midpoint,
    reason = "timing statistics over bounded sample counts: microsecond casts, percentile indices and medians of a few hundred samples"
)]

use std::time::{Duration, Instant};

use settings::SettingsShell;
use ui::accesskit::Role;
use ui::accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate,
};
use ui::framework::testing::TestApp;

struct Idle;
impl ActionHandler for Idle {
    fn do_action(&mut self, _: ActionRequest) {}
}
impl ActivationHandler for Idle {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        None
    }
}
impl DeactivationHandler for Idle {
    fn deactivate_accessibility(&mut self) {}
}

fn median(samples: &mut [u64]) -> u64 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "needs a session bus with the AT-SPI registry; see the module docs"]
fn accesskit_adapter_on_a_live_bus() {
    let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
    app.settle();
    app.harness().resize((1472, 920)).unwrap();
    app.settle();
    let mut adapter = accesskit_unix::Adapter::new(Idle, Idle, Idle);
    adapter.update_window_focus_state(true);

    // The adapter wakes on a change of the bus's `IsEnabled`, never on its
    // initial value, so flip it once the adapter is listening.
    if std::env::var_os("TILO_A11Y_BUS").is_some() {
        std::thread::sleep(Duration::from_millis(500));
        for value in ["false", "true"] {
            let status = std::process::Command::new("dbus-send")
                .args([
                    "--session",
                    "--print-reply",
                    "--dest=org.a11y.Bus",
                    "/org/a11y/bus",
                    "org.freedesktop.DBus.Properties.Set",
                    "string:org.a11y.Status",
                    "string:IsEnabled",
                    &format!("variant:boolean:{value}"),
                ])
                .stdout(std::process::Stdio::null())
                .status()
                .expect("dbus-send runs");
            assert!(status.success(), "the bus accepted IsEnabled={value}");
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    // The adapter wakes when the bus says so; feed it frames until it asks.
    let mut active = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !active {
        adapter.update_if_active(|| {
            active = true;
            app.harness().a11y_tree_update()
        });
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("perf a11y adapter active: {active}");

    let rows: Vec<(f64, f64)> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == Role::TreeItem)
        .filter_map(|(_, node)| {
            node.bounds()
                .map(|b| ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0))
        })
        .collect();
    let (mut switch, mut update, mut nodes) = (Vec::new(), Vec::new(), 0);
    for round in 0..3 {
        for (x, y) in rows.iter().skip(1).take(6) {
            let t = Instant::now();
            app.harness().click_at(*x, *y);
            app.settle();
            let frame = t.elapsed().as_micros() as u64;
            let t = Instant::now();
            adapter.update_if_active(|| {
                let tree = app.harness().a11y_tree_update();
                nodes = tree.nodes.len();
                tree
            });
            let adapter_us = t.elapsed().as_micros() as u64;
            if round > 0 {
                switch.push(frame);
                update.push(adapter_us);
            }
        }
    }
    let (frame, adapter_us) = (median(&mut switch), median(&mut update));
    eprintln!(
        "perf a11y bus (active={active}): page switch p50={frame}us, adapter update p50={adapter_us}us ({nodes} nodes)"
    );
    // Scrolling re-sends the whole tree each frame.
    let mut scroll = Vec::new();
    app.harness().click_at(1460.0, 460.0);
    for i in 0..60 {
        app.harness().scroll(0.0, if i < 30 { -1.0 } else { 1.0 });
        app.settle();
        let t = Instant::now();
        adapter.update_if_active(|| app.harness().a11y_tree_update());
        scroll.push(t.elapsed().as_micros() as u64);
    }
    eprintln!(
        "perf a11y bus (active={active}): adapter update per scroll frame p50={}us",
        median(&mut scroll)
    );
}

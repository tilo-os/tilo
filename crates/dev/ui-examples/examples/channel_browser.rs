//! Interactive channel-browser demo with seeded entries.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p ui-examples --example ui-lab -- channel_browser                       # interactive
//! cargo run -p ui-examples --example ui-lab -- channel_browser --shots o.png         # headless frame
//! TILO_SHOTS_THEME=light cargo run -p ui-examples --example ui-lab -- channel_browser --shots o.png
//! ```
//!
//! `--shots` mounts the sheet in a [`tilo_ui::headless::FrameHarness`] under the
//! `TILO_SHOTS_THEME` palette (`dark` by default), composites it on the
//! theme's background and writes `o.png` plus `o.inspection.json` — the
//! rects `scripts/ux-bench/usability.py`'s `geometry_audit` lints.
//!
//! Notes:
//! - No broker, wheel-disc, or shell integration: the widget consumes
//!   `Vec<ChannelEntry>` directly.
//! - Click a space-filter pill to narrow; arrow keys cycle through
//!   pills + rows; Enter prints the selected channel; Esc dismisses.

use std::path::Path;
use std::time::{Duration, SystemTime};

use collab_types::ChannelId;
use tilo_ui::headless::FrameHarness;
use tilo_ui::widget::channel_browser::{ChannelBrowser, ChannelEntry};
use tilo_ui::{App, State};

/// Viewport for the headless frame: the sheet's own 640 px layout width
/// plus a margin of ground on every side, so the sheet edge is visible.
const SHOT_VIEWPORT: (u32, u32) = (720, 620);

/// A fixed instant for `--shots`, so relative times do not drift between
/// runs; the live sheet uses the real clock.
const SHOT_NOW_SECS: u64 = 1_800_000_000;

fn clock(headless: bool) -> SystemTime {
    if headless {
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(SHOT_NOW_SECS))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    } else {
        tilo_ui::time::wall()
    }
}

fn ch(
    now: SystemTime,
    name: &str,
    space: &str,
    hue: f32,
    minutes_ago: u64,
    unread: u32,
    pinned: bool,
    role: Option<&str>,
) -> ChannelEntry {
    ChannelEntry {
        channel_id: ChannelId::new(),
        space_name: space.into(),
        space_color_hue: hue,
        name: name.into(),
        last_activity: now
            .checked_sub(Duration::from_secs(minutes_ago.saturating_mul(60)))
            .unwrap_or(now),
        unread_count: unread,
        is_pinned: pinned,
        role_label: role.map(str::to_string),
    }
}

pub fn run(args: &[String]) {
    let shot = args
        .iter()
        .position(|a| a == "--shots")
        .and_then(|i| args.get(i.saturating_add(1)))
        .cloned();
    let _detached = tilo_ui::wake::WakeScope::enter_detached();
    let wake_scope = shot.is_none().then(tilo_ui::wake::WakeScope::new);
    let _wake_entry = wake_scope.as_ref().map(tilo_ui::wake::WakeScope::enter);
    let now = clock(shot.is_some());
    // Mocked entries — two spaces, a mix of roles, pins, and unread
    // counts. Matches the shape the shell will produce in Phase 5
    // once the wheel-disc auto-promote algorithm runs.
    let entries = vec![
        ch(
            now,
            "engineering",
            "Company",
            220.0,
            5,
            12,
            true,
            Some("Editor"),
        ),
        ch(
            now,
            "design",
            "Company",
            220.0,
            25,
            0,
            false,
            Some("Editor"),
        ),
        ch(now, "ops", "Company", 220.0, 90, 3, false, Some("Owner")),
        ch(
            now,
            "marketing",
            "Company",
            220.0,
            240,
            0,
            false,
            Some("Viewer"),
        ),
        ch(
            now,
            "standup",
            "Company",
            220.0,
            8,
            1,
            false,
            Some("Editor"),
        ),
        ch(now, "journal", "Personal", 30.0, 60, 0, true, None),
        ch(now, "recipes", "Personal", 30.0, 720, 0, false, None),
        ch(now, "reading-list", "Personal", 30.0, 1440, 5, false, None),
        ch(
            now,
            "side-project",
            "Side hustles",
            130.0,
            180,
            2,
            false,
            Some("Owner"),
        ),
    ];

    let state = State::new();

    let mut browser = ChannelBrowser::with_entries(entries)
        .on_select(|e| {
            eprintln!(
                "selected: {} (space={}, unread={})",
                e.name, e.space_name, e.unread_count
            );
        })
        .on_dismiss(|| {
            eprintln!("dismissed");
        });

    match (shot, wake_scope) {
        (Some(out), _) => {
            browser.set_now_for_test(now);
            render_shot(browser, Path::new(&out));
        }
        (None, Some(scope)) => App::from_scoped_parts(scope, state, Box::new(browser)).run(),
        (None, None) => eprintln!("channel_browser: no wake scope for the live sheet"),
    }
}

/// One headless frame of the sheet: PNG composited on the theme background
/// plus the harness inspection beside it.
fn render_shot(browser: ChannelBrowser, out: &Path) {
    let theme = crate::registry::shot_theme();
    let harness = FrameHarness::new_with_theme(
        State::new(),
        Box::new(browser),
        SHOT_VIEWPORT,
        theme.with_reduce_motion(true),
    );
    harness.render_png(out);
    match harness.inspection_json() {
        Ok(json) => {
            let path = out.with_extension("inspection.json");
            if let Err(error) = std::fs::write(&path, json) {
                eprintln!("channel_browser: cannot write {}: {error}", path.display());
            }
        }
        Err(error) => eprintln!("channel_browser: inspection failed: {error}"),
    }
    println!("channel_browser: wrote {}", out.display());
}

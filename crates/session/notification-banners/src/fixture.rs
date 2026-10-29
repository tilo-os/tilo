//! The banners on their own: a fixed desktop's notifications for a capture,
//! a test and the app's own window, staged per scenario.

use tilo_desktop::notifications::{
    Addressed, Banner, BannerFeed, Notification, NotificationIconKind, NotificationSeverity,
    PERSISTENT_DISMISS,
};
use ui::time::Duration;
use ui::vello::kurbo::Rect;

use crate::link::{OtherDisplay, Placement};

/// The top bar over every display of the fixed desktop.
const TOP_BAR: f64 = 32.0;

/// The window a scenario is drawn in.
#[must_use]
pub fn viewport(scenario: Option<&str>) -> (u32, u32) {
    match scenario {
        Some("machine") => (2560, 800),
        Some("more") => (1280, 400),
        _ => (1280, 800),
    }
}

/// Settled on screen: the banner arrived a while ago and rests.
fn settled(notification: Notification) -> Notification {
    let shown = ui::time::now()
        .checked_sub(Duration::from_secs(2))
        .unwrap_or_else(ui::time::now);
    notification.with_shown_at(shown)
}

fn banner(notification: Notification) -> Banner {
    Banner {
        notification: settled(notification),
        actions: Vec::new(),
    }
}

fn alarm_choices() -> Vec<(String, String)> {
    vec![
        ("dismiss".into(), "Dismiss".into()),
        ("snooze".into(), "Snooze".into()),
    ]
}

/// A message from a person, with an inline reply.
fn message() -> Banner {
    let mut notification = Notification::new("Ada Lovelace", "Can you look at the review?")
        .with_app_name("Messages")
        .with_from("Ada Lovelace", "ada@example.org");
    notification.actions = vec![
        ("default".into(), "Open".into()),
        ("inline-reply".into(), "Reply".into()),
    ];
    banner(notification)
}

/// A meeting reminder with the desktop's alarm choices.
fn alarm() -> Banner {
    let mut banner = banner(
        Notification::new("Design review", "Starts in 5 minutes")
            .with_app_name("Calendar")
            .with_addressed(Addressed::Reminder)
            .with_dismiss_after(PERSISTENT_DISMISS),
    );
    banner.actions = alarm_choices();
    banner
}

fn build() -> Banner {
    banner(Notification::new("Build finished", "All 412 tests passed").with_app_name("Terminal"))
}

fn call() -> Banner {
    let mut notification = Notification::new("Jonas Weber", "Incoming video call")
        .with_app_name("Calls")
        .with_from("Jonas Weber", "jonas@example.org")
        .with_addressed(Addressed::IncomingCall)
        .with_dismiss_after(PERSISTENT_DISMISS);
    notification.actions = vec![
        ("accept".into(), "Answer".into()),
        ("decline".into(), "Decline".into()),
    ];
    banner(notification)
}

fn battery() -> Banner {
    banner(
        Notification::new("Battery critically low", "Plug in now: 4 % left")
            .with_severity(NotificationSeverity::Error)
            .with_icon_kind(NotificationIconKind::Generic)
            .with_dismiss_after(PERSISTENT_DISMISS)
            .machine_wide(),
    )
}

/// The fixed desktop's banners for `scenario`, oldest first.
#[must_use]
pub fn feed(scenario: Option<&str>) -> BannerFeed {
    let banners = match scenario {
        Some("empty") => Vec::new(),
        Some("choices") => vec![alarm()],
        Some("call") => vec![build(), call()],
        Some("machine") => vec![build(), battery(), call()],
        Some("more") => (1..=6)
            .map(|at| {
                banner(
                    Notification::new(format!("Upload {at} finished"), "photos.zip")
                        .with_app_name("Files"),
                )
            })
            .collect(),
        _ => vec![build(), alarm(), message()],
    };
    BannerFeed {
        banners,
        locked: scenario == Some("locked"),
        held: false,
    }
}

/// Where the fixed desktop puts the banners for `scenario`, in a window of
/// `size`.
#[must_use]
pub fn placement(scenario: Option<&str>, size: (f64, f64)) -> Placement {
    let (width, height) = size;
    match scenario {
        Some("machine") => {
            let half = width / 2.0;
            Placement {
                viewport: Rect::new(0.0, TOP_BAR, half, height),
                others: vec![OtherDisplay {
                    display: Rect::new(half, 0.0, width, height),
                    viewport: Rect::new(half, TOP_BAR, width, height),
                }],
                ..Placement::default()
            }
        }
        Some("bars") => Placement {
            viewport: Rect::new(0.0, TOP_BAR, width, height),
            window_bars: vec![
                Rect::new(8.0, TOP_BAR + 8.0, width / 2.0 - 4.0, TOP_BAR + 52.0),
                Rect::new(
                    width / 2.0 + 4.0,
                    TOP_BAR + 8.0,
                    width - 8.0,
                    TOP_BAR + 96.0,
                ),
            ],
            ..Placement::default()
        },
        _ => Placement {
            viewport: Rect::new(0.0, TOP_BAR, width, height),
            ..Placement::default()
        },
    }
}

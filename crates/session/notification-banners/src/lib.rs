//! The desktop's notification banners as an app: the banner stack in the
//! top-right corner of the display in use, the banners about the whole
//! machine repeated on the other displays, and a ringing call's edge.
//!
//! The banners draw what the desktop lends them (its live notifications and
//! where they go, [`link`]) and publish what can be done with them as their
//! actions ([`BannerEffect`]): close, open and choose on a banner, reply to
//! it, snooze it, move keyboard focus into it, read it in full in the
//! notification centre, answer or decline a call. The shell mounts them in
//! its own process over a feed of the notifications it keeps; on their own,
//! in a capture or a test, they show a fixed desktop's ([`fixture`]).

pub mod fixture;
pub mod link;
mod prepare;
pub mod resource;
pub mod surface;

pub use link::{Ask, BannerService, Command, Link, OtherDisplay, Placement, Shown};
pub use prepare::prepare;
pub use resource::{BannerEffect, Banners, NotificationBanners};
pub use surface::BannerSurface;

/// The banners' package id.
pub const APP_ID: &str = "so.tilo.notification-banners";

/// The freedesktop action key a sender offers to take an inline reply.
pub const INLINE_REPLY: &str = "inline-reply";

/// Whether `key` is one the banners act on while they hold the keyboard:
/// any other goes back to the person's window, and takes focus with it.
#[must_use]
pub fn banner_key(key: &ui::winit::keyboard::Key) -> bool {
    use ui::winit::keyboard::{Key, NamedKey};
    matches!(
        key,
        Key::Named(
            NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::Tab
                | NamedKey::Enter
                | NamedKey::Space
                | NamedKey::Escape
        )
    )
}

/// The banners' toolkit definition, the one every host mounts, carrying
/// their committed surface.
#[must_use]
pub fn definition() -> &'static ui::prelude::ApplicationDefinition {
    ui::built_in_definition!(NotificationBanners)
}

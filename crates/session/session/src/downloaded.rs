//! The programs the person downloaded and added to their apps: one registry
//! the session hosts ([`tilo_app_launch::downloaded`]) and publishes beside
//! its other services, and the desk's windows, which tell it when a program
//! that names no application directory shows itself.
//!
//! The registry reads, classifies and writes off the compositor's loop; all
//! the session does here is hand it each window once and publish it, so
//! search and the app sheets read one registry rather than one each.

use std::sync::Arc;

use tilo_app_launch::downloaded::{self, ObservedWindow, WindowArtwork};
use tilo_compositor::{IconArtwork, WindowIcon, WindowInfo};
use tilo_service::Service;
use tilo_surface_host::{StripPolicy, WindowObserver};

use crate::Options;

/// The desk's windows as the registry sees them: a window that named itself
/// is looked at once, and the program behind it is offered while it has no
/// entry of its own.
struct Follow(downloaded::Handle);

impl WindowObserver for Follow {
    fn opened(&self, window: &WindowInfo) {
        let Some(app_id) = window.app_id.as_deref() else {
            return;
        };
        self.0.opened(ObservedWindow {
            id: window.id,
            app_id: app_id.to_owned(),
            pid: window.pid,
            artwork: artwork(window.icon.as_ref()),
        });
    }

    fn closed(&self, id: u64) {
        self.0.closed(id);
    }
}

/// The icon the window set for itself, as the registry reads it: the name it
/// named, else the largest pixels it offered.
fn artwork(icon: Option<&Arc<WindowIcon>>) -> Option<WindowArtwork> {
    match icon?.artwork() {
        IconArtwork::Named(name) => Some(WindowArtwork::Named(name.clone())),
        IconArtwork::Pixels(images) => {
            let image = images.iter().max_by_key(|image| image.width)?;
            Some(WindowArtwork::Pixels {
                width: image.width,
                height: image.height,
                rgba: Arc::clone(&image.pixels),
            })
        }
    }
}

/// Host the person's downloaded programs: the registry takes the desk's
/// windows from now on and is published as a service of `options`, so the
/// frame after it is mounted finds it by its resource. A registry that
/// cannot start is said so in the log and the session carries on without
/// one: no program is offered, and the apps it already has are still the
/// machine's own entries.
pub fn install(options: &mut Options, policy: &mut StripPolicy) {
    let registry = match downloaded::Downloaded::start(Some(options.wake.handle())) {
        Ok(registry) => registry,
        Err(error) => {
            tracing::warn!(%error, "the downloaded programs are not kept");
            return;
        }
    };
    policy.observe_windows(Arc::new(Follow(registry.handle())));
    options
        .services
        .push(Box::new(registry) as Box<dyn Service>);
}

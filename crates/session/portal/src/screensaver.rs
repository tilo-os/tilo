//! `org.freedesktop.ScreenSaver` `Inhibit`/`UnInhibit` on the session bus.
//!
//! Browsers, video players and older SDL games keep the screen on through
//! this interface rather than the portal or the Wayland protocol. Each
//! cookie is an idle hold among the portal's own [`Inhibitions`], so it is
//! published like theirs (the person sees who keeps the screen on and may
//! end it), and is released by `UnInhibit` or by its caller leaving the
//! bus: a crashed player must not keep the session from locking.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt as _;
use tilo_lock::MutexRecover as _;
use zbus::interface;

use crate::inhibit::{IDLE, Inhibitions};

/// The name the interface is served under.
pub const BUS_NAME: &str = "org.freedesktop.ScreenSaver";
/// Where clients look for it: the specification's path, and the one older
/// clients use.
pub const OBJECT_PATHS: [&str; 2] = ["/org/freedesktop/ScreenSaver", "/ScreenSaver"];

/// How long an app's name may be, in characters.
const MAX_NAME_CHARS: usize = 64;

#[derive(Default)]
struct Cookies {
    /// The bus name that took each cookie.
    owners: HashMap<u32, String>,
    last: u32,
}

/// The `org.freedesktop.ScreenSaver` interface, holding through
/// `inhibitions`.
#[derive(Clone)]
pub struct ScreenSaver {
    inhibitions: Inhibitions,
    cookies: Arc<Mutex<Cookies>>,
}

/// The hold path of `cookie`, among the portal's.
fn hold_path(cookie: u32) -> String {
    format!("screensaver:{cookie}")
}

/// An app's name as it gave it, cleaned for display; `fallback` when it
/// gave none.
fn display_name(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    if cleaned.is_empty() {
        fallback.to_owned()
    } else {
        cleaned
    }
}

impl ScreenSaver {
    #[must_use]
    pub fn new(inhibitions: Inhibitions) -> Self {
        Self {
            inhibitions,
            cookies: Arc::default(),
        }
    }

    /// Hold the screen on for `owner`, which names itself `name`; its
    /// cookie.
    pub fn inhibit(&self, owner: &str, name: &str, reason: &str) -> u32 {
        let cookie = {
            let mut cookies = self.cookies.lock_recover();
            let mut cookie = cookies.last.wrapping_add(1).max(1);
            while cookies.owners.contains_key(&cookie) {
                cookie = cookie.wrapping_add(1).max(1);
            }
            cookies.last = cookie;
            cookies.owners.insert(cookie, owner.to_owned());
            cookie
        };
        let reason = display_name(reason, "");
        self.inhibitions.hold(
            &hold_path(cookie),
            &display_name(name, owner),
            IDLE,
            (!reason.is_empty()).then_some(reason),
        );
        cookie
    }

    /// Release `owner`'s hold `cookie`; `false` when it holds no such one.
    pub fn uninhibit(&self, owner: &str, cookie: u32) -> bool {
        let released = {
            let mut cookies = self.cookies.lock_recover();
            let held = cookies
                .owners
                .get(&cookie)
                .is_some_and(|held| held == owner);
            held && cookies.owners.remove(&cookie).is_some()
        };
        if released {
            self.inhibitions.release(&hold_path(cookie));
        }
        released
    }

    /// Release every hold of `owner`, which left the bus; how many.
    pub fn release_owner(&self, owner: &str) -> usize {
        let gone: Vec<u32> = {
            let mut cookies = self.cookies.lock_recover();
            let gone = cookies
                .owners
                .iter()
                .filter(|(_, held)| *held == owner)
                .map(|(cookie, _)| *cookie)
                .collect::<Vec<_>>();
            for cookie in &gone {
                cookies.owners.remove(cookie);
            }
            gone
        };
        for cookie in &gone {
            self.inhibitions.release(&hold_path(*cookie));
        }
        gone.len()
    }

    /// Serve at both paths on `connection` and release the holds of every
    /// client that leaves it.
    ///
    /// # Errors
    /// The object server refused a path.
    pub async fn serve_on(&self, connection: &zbus::Connection) -> zbus::Result<()> {
        for path in OBJECT_PATHS {
            connection.object_server().at(path, self.clone()).await?;
        }
        let service = self.clone();
        let watched = connection.clone();
        connection
            .executor()
            .spawn(
                async move {
                    let Ok(proxy) = zbus::fdo::DBusProxy::new(&watched).await else {
                        return;
                    };
                    let Ok(mut changes) = proxy.receive_name_owner_changed().await else {
                        return;
                    };
                    while let Some(change) = changes.next().await {
                        let Ok(args) = change.args() else { continue };
                        if args.name().starts_with(':') && args.new_owner().is_none() {
                            service.release_owner(args.name());
                        }
                    }
                },
                "screensaver holds of departed clients",
            )
            .detach();
        Ok(())
    }
}

fn sender(header: &zbus::message::Header<'_>) -> zbus::fdo::Result<String> {
    header
        .sender()
        .map(ToString::to_string)
        .ok_or_else(|| zbus::fdo::Error::AccessDenied("no sender".into()))
}

#[interface(name = "org.freedesktop.ScreenSaver")]
impl ScreenSaver {
    #[zbus(name = "Inhibit")]
    fn dbus_inhibit(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        application_name: String,
        reason_for_inhibit: String,
    ) -> zbus::fdo::Result<u32> {
        Ok(self.inhibit(&sender(&header)?, &application_name, &reason_for_inhibit))
    }

    #[zbus(name = "UnInhibit")]
    fn dbus_uninhibit(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        cookie: u32,
    ) -> zbus::fdo::Result<()> {
        if self.uninhibit(&sender(&header)?, cookie) {
            Ok(())
        } else {
            Err(zbus::fdo::Error::InvalidArgs(
                "unknown inhibit cookie".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cookie_holds_the_screen_on_until_its_owner_releases_it() {
        let inhibitions = Inhibitions::default();
        let service = ScreenSaver::new(inhibitions.clone());
        let cookie = service.inhibit(":1.42", "Google Chrome", "Playing video");
        assert_ne!(cookie, 0);
        assert!(inhibitions.registry().is_inhibited_for(IDLE));
        assert_eq!(
            inhibitions.held(),
            [(hold_path(cookie), "Google Chrome".to_owned(), IDLE)]
        );
        assert!(
            !service.uninhibit(":1.7", cookie),
            "another client must not release someone else's hold"
        );
        assert!(service.uninhibit(":1.42", cookie));
        assert!(!inhibitions.registry().is_inhibited_for(IDLE));
    }

    #[test]
    fn a_caller_leaving_the_bus_releases_every_hold_it_took() {
        let inhibitions = Inhibitions::default();
        let service = ScreenSaver::new(inhibitions.clone());
        service.inhibit(":1.42", "VLC", "");
        service.inhibit(":1.42", "VLC", "");
        let other = service.inhibit(":1.43", "", "");
        assert_eq!(service.release_owner(":1.42"), 2);
        assert_eq!(inhibitions.held().len(), 1);
        assert_eq!(inhibitions.held()[0].1, ":1.43", "named by its bus name");
        assert!(service.uninhibit(":1.43", other));
        assert!(!inhibitions.registry().is_inhibited_for(IDLE));
    }

    #[test]
    fn the_name_is_cleaned_for_display() {
        assert_eq!(
            display_name("  VLC media player \n", "x"),
            "VLC media player"
        );
        assert_eq!(
            display_name(&"x".repeat(200), "y").chars().count(),
            MAX_NAME_CHARS
        );
    }
}

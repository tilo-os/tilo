//! `org.freedesktop.ScreenSaver` `Inhibit`/`UnInhibit` on the session bus.
//!
//! Chrome, VLC, older SDL and many other native players keep the screen on
//! through this interface rather than the portal or the Wayland protocol.
//! Each cookie holds an idle hold in the session's [`InhibitRegistry`], the
//! same one those paths use, and is released by `UnInhibit` or by its caller
//! leaving the bus — a crashed player must not keep the session unlocked.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use tilo_power_mgmt::{InhibitRegistry, InhibitToken};
use zbus::interface;

use super::lifecycle::disconnected_peers;

/// Well-known name the service claims; freedesktop clients call it directly.
pub const BUS_NAME: &str = "org.freedesktop.ScreenSaver";
/// Both object paths clients use in the wild.
pub const OBJECT_PATHS: [&str; 2] = ["/org/freedesktop/ScreenSaver", "/ScreenSaver"];

/// Longest application name kept; it is shown in quick settings.
const MAX_NAME_CHARS: usize = 64;

struct Hold {
    owner: String,
    _token: InhibitToken,
}

#[derive(Default)]
struct Holds {
    by_cookie: HashMap<u32, Hold>,
    next_cookie: u32,
}

/// The cookies handed out, shared by every exported path.
#[derive(Clone)]
pub struct ScreenSaverService {
    registry: InhibitRegistry,
    holds: Arc<Mutex<Holds>>,
}

impl ScreenSaverService {
    pub fn new(registry: InhibitRegistry) -> Self {
        Self {
            registry,
            holds: Arc::new(Mutex::new(Holds::default())),
        }
    }

    /// Hold the screen on for `owner` (a unique bus name), naming the hold
    /// after the application; returns the cookie `UnInhibit` takes.
    pub fn inhibit(&self, owner: &str, application_name: &str) -> u32 {
        let name = display_name(application_name);
        let token = self
            .registry
            .acquire_for(name, tilo_power_mgmt::inhibit::INHIBIT_IDLE);
        let mut holds = self
            .holds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Cookie 0 reads as "failed" to some clients; never hand it out.
        let mut cookie = holds.next_cookie.wrapping_add(1).max(1);
        while holds.by_cookie.contains_key(&cookie) {
            cookie = cookie.wrapping_add(1).max(1);
        }
        holds.next_cookie = cookie;
        holds.by_cookie.insert(
            cookie,
            Hold {
                owner: owner.to_owned(),
                _token: token,
            },
        );
        cookie
    }

    /// Release `cookie` if `owner` is the caller that took it; another
    /// client cannot release a hold it does not own.
    pub fn uninhibit(&self, owner: &str, cookie: u32) -> bool {
        let mut holds = self
            .holds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if holds
            .by_cookie
            .get(&cookie)
            .is_some_and(|hold| hold.owner == owner)
        {
            holds.by_cookie.remove(&cookie);
            true
        } else {
            false
        }
    }

    /// Release everything `owner` holds — it left the bus.
    pub fn release_owner(&self, owner: &str) -> usize {
        let mut holds = self
            .holds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = holds.by_cookie.len();
        holds.by_cookie.retain(|_, hold| hold.owner != owner);
        before.saturating_sub(holds.by_cookie.len())
    }

    /// Drop the holds of callers as they disconnect.
    pub fn spawn_disconnect_cleanup(&self, connection: &zbus::Connection) {
        let service = self.clone();
        let task_connection = connection.clone();
        connection
            .executor()
            .spawn(
                async move {
                    let Ok(mut gone) = disconnected_peers(&task_connection).await else {
                        return;
                    };
                    while let Some(owner) = gone.next().await {
                        service.release_owner(&owner);
                    }
                },
                "screensaver inhibitor disconnect cleanup",
            )
            .detach();
    }
}

/// What the hold is called: the application's own name, trimmed and bounded,
/// with control characters removed since it is drawn in trusted chrome.
fn display_name(application_name: &str) -> String {
    let cleaned: String = application_name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    if cleaned.is_empty() {
        crate::shell::i18n::t("portal-inhibit-unnamed-app")
    } else {
        cleaned
    }
}

#[interface(name = "org.freedesktop.ScreenSaver")]
impl ScreenSaverService {
    #[zbus(name = "Inhibit")]
    fn dbus_inhibit(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        application_name: String,
        _reason_for_inhibit: String,
    ) -> zbus::fdo::Result<u32> {
        let owner = hdr
            .sender()
            .ok_or_else(|| zbus::fdo::Error::AccessDenied("no sender".into()))?;
        Ok(self.inhibit(owner.as_str(), &application_name))
    }

    #[zbus(name = "UnInhibit")]
    fn dbus_uninhibit(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        cookie: u32,
    ) -> zbus::fdo::Result<()> {
        let owner = hdr
            .sender()
            .ok_or_else(|| zbus::fdo::Error::AccessDenied("no sender".into()))?;
        if self.uninhibit(owner.as_str(), cookie) {
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

    const IDLE: u32 = tilo_power_mgmt::inhibit::INHIBIT_IDLE;

    #[test]
    fn a_cookie_holds_the_screen_on_until_its_owner_releases_it() {
        let registry = InhibitRegistry::new();
        let service = ScreenSaverService::new(registry.clone());
        let cookie = service.inhibit(":1.42", "Google Chrome");
        assert_ne!(cookie, 0);
        assert!(registry.is_inhibited_for(IDLE));
        assert_eq!(registry.active_reasons_for(IDLE), vec!["Google Chrome"]);

        assert!(
            !service.uninhibit(":1.7", cookie),
            "another client must not release someone else's hold"
        );
        assert!(registry.is_inhibited_for(IDLE));
        assert!(service.uninhibit(":1.42", cookie));
        assert!(!registry.is_inhibited_for(IDLE));
    }

    #[test]
    fn a_caller_leaving_the_bus_releases_every_hold_it_took() {
        let registry = InhibitRegistry::new();
        let service = ScreenSaverService::new(registry.clone());
        service.inhibit(":1.42", "VLC");
        service.inhibit(":1.42", "VLC");
        let other = service.inhibit(":1.43", "mpv");
        assert_eq!(service.release_owner(":1.42"), 2);
        assert_eq!(registry.active_reasons_for(IDLE), vec!["mpv"]);
        assert!(service.uninhibit(":1.43", other));
        assert!(!registry.is_inhibited_for(IDLE));
    }

    #[test]
    fn the_hold_is_named_for_display() {
        assert_eq!(display_name("  VLC media player \n"), "VLC media player");
        assert_eq!(display_name(""), "An app");
        assert_eq!(
            display_name(&"x".repeat(200)).chars().count(),
            MAX_NAME_CHARS
        );
    }
}

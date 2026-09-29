//! Appearance from the XDG Settings portal's `org.freedesktop.appearance`:
//! colour scheme, accent, contrast and reduced motion, followed live as the
//! desktop changes them. A background thread owns the D-Bus connection; a
//! desktop without the portal leaves every preference unset.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use zbus::zvariant::{OwnedValue, Value};

use super::{HostAppearance, Scheme};

const NAMESPACE: &str = "org.freedesktop.appearance";

#[zbus::proxy(
    interface = "org.freedesktop.portal.Settings",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop",
    gen_async = false
)]
trait Settings {
    fn read_all(
        &self,
        namespaces: &[&str],
    ) -> zbus::Result<HashMap<String, HashMap<String, OwnedValue>>>;

    #[zbus(signal)]
    fn setting_changed(&self, namespace: &str, key: &str, value: Value<'_>) -> zbus::Result<()>;
}

#[derive(Default)]
pub(super) struct Platform {
    reading: Arc<Mutex<HostAppearance>>,
}

impl Platform {
    pub(super) fn start() -> Self {
        let platform = Self::default();
        let reading = Arc::clone(&platform.reading);
        let wake = crate::wake::WakeHandle::new();
        let spawned = std::thread::Builder::new()
            .name("tilo-host-appearance".into())
            .spawn(move || {
                let followed = zbus::blocking::Connection::session().and_then(|connection| {
                    follow(&connection, |appearance| {
                        *reading.lock().unwrap_or_else(PoisonError::into_inner) = appearance;
                        wake.wake();
                    })
                });
                if let Err(error) = followed {
                    tracing::debug!(%error, "no appearance from the Settings portal");
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "cannot follow the desktop appearance");
        }
        platform
    }

    pub(super) fn refresh(&mut self) {}

    pub(super) fn current(&self) -> HostAppearance {
        *self.reading.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Publishes the portal's appearance, then the appearance after each change
/// to it, until the connection closes.
fn follow(
    connection: &zbus::blocking::Connection,
    mut publish: impl FnMut(HostAppearance),
) -> zbus::Result<()> {
    let settings = SettingsProxy::new(connection)?;
    // Subscribed before the read, so a change in between is not lost.
    let changes = settings.receive_setting_changed()?;
    let mut appearance = HostAppearance::default();
    if let Some(values) = settings.read_all(&[NAMESPACE])?.get(NAMESPACE) {
        for (key, value) in values {
            apply(&mut appearance, key, value);
        }
    }
    publish(appearance);
    for change in changes {
        let args = change.args()?;
        if args.namespace == NAMESPACE && apply(&mut appearance, args.key, &args.value) {
            publish(appearance);
        }
    }
    Ok(())
}

/// Folds one appearance setting into `appearance`, returning whether it is
/// one this reads. Unknown and out-of-range values mean "no preference".
fn apply(appearance: &mut HostAppearance, key: &str, value: &Value<'_>) -> bool {
    let value = unwrap_variant(value);
    let flag = || u32::try_from(value).ok();
    match key {
        "color-scheme" => {
            appearance.scheme = match flag() {
                Some(1) => Some(Scheme::Dark),
                Some(2) => Some(Scheme::Light),
                _ => None,
            };
        }
        "accent-color" => appearance.accent = accent(value),
        "contrast" => appearance.high_contrast = flag() == Some(1),
        "reduced-motion" => appearance.reduce_motion = flag() == Some(1),
        _ => return false,
    }
    true
}

/// Some portal backends answer with the value boxed in one more variant.
fn unwrap_variant<'a>(value: &'a Value<'a>) -> &'a Value<'a> {
    match value {
        Value::Value(inner) => unwrap_variant(inner),
        other => other,
    }
}

/// `(ddd)` sRGB in `0.0..=1.0`; anything else is the spec's "unset".
fn accent(value: &Value<'_>) -> Option<[u8; 3]> {
    let Value::Structure(rgb) = value else {
        return None;
    };
    let [Value::F64(r), Value::F64(g), Value::F64(b)] = rgb.fields() else {
        return None;
    };
    let channel = |c: f64| {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "range-checked to 0.0..=1.0 first, so the product fits a u8"
        )]
        let byte = (c * 255.0).round() as u8;
        (0.0..=1.0).contains(&c).then_some(byte)
    };
    Some([channel(*r)?, channel(*g)?, channel(*b)?])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    fn applied(key: &str, value: Value<'_>) -> HostAppearance {
        let mut appearance = HostAppearance::default();
        assert!(apply(&mut appearance, key, &value), "{key} is read");
        appearance
    }

    #[test]
    fn color_scheme_maps_prefer_dark_and_prefer_light() {
        assert_eq!(
            applied("color-scheme", Value::U32(1)).scheme,
            Some(Scheme::Dark)
        );
        assert_eq!(
            applied("color-scheme", Value::U32(2)).scheme,
            Some(Scheme::Light)
        );
        assert_eq!(applied("color-scheme", Value::U32(0)).scheme, None);
        assert_eq!(applied("color-scheme", Value::U32(7)).scheme, None);
        assert_eq!(
            applied("color-scheme", Value::Value(Box::new(Value::U32(1)))).scheme,
            Some(Scheme::Dark),
            "a doubly boxed value reads the same"
        );
    }

    #[test]
    fn accent_maps_unit_rgb_and_rejects_out_of_range() {
        let rgb = |r, g, b| Value::from((r, g, b));
        assert_eq!(
            applied("accent-color", rgb(1.0, 0.5, 0.0)).accent,
            Some([255, 128, 0])
        );
        assert_eq!(applied("accent-color", rgb(-1.0, 0.5, 0.0)).accent, None);
        assert_eq!(applied("accent-color", rgb(0.2, 1.5, 0.0)).accent, None);
        assert_eq!(applied("accent-color", Value::U32(3)).accent, None);
    }

    #[test]
    fn contrast_and_reduced_motion_are_on_only_at_one() {
        assert!(applied("contrast", Value::U32(1)).high_contrast);
        assert!(!applied("contrast", Value::U32(0)).high_contrast);
        assert!(applied("reduced-motion", Value::U32(1)).reduce_motion);
        assert!(!applied("reduced-motion", Value::U32(0)).reduce_motion);
        let mut appearance = HostAppearance::default();
        assert!(!apply(&mut appearance, "wallpaper", &Value::U32(1)));
        assert_eq!(appearance, HostAppearance::default());
    }

    /// A stand-in for `xdg-desktop-portal` serving one appearance.
    struct FakePortal {
        appearance: HashMap<String, OwnedValue>,
    }

    #[zbus::interface(name = "org.freedesktop.portal.Settings")]
    impl FakePortal {
        fn read_all(
            &self,
            namespaces: Vec<String>,
        ) -> HashMap<String, HashMap<String, OwnedValue>> {
            namespaces
                .into_iter()
                .filter(|namespace| namespace == NAMESPACE)
                .map(|namespace| {
                    let values = self
                        .appearance
                        .iter()
                        .map(|(key, value)| (key.clone(), value.try_clone().expect("clone")))
                        .collect();
                    (namespace, values)
                })
                .collect()
        }
    }

    fn owned(value: Value<'_>) -> OwnedValue {
        value.try_into().expect("owned value")
    }

    #[test]
    fn follows_a_fake_portal_over_dbus() {
        const GUID: &str = "0123456789abcdef0123456789abcdef";
        const PATH: &str = "/org/freedesktop/portal/desktop";
        let (server_socket, client_socket) = UnixStream::pair().expect("socket pair");
        let portal = FakePortal {
            appearance: HashMap::from([
                ("color-scheme".to_owned(), owned(Value::U32(2))),
                (
                    "accent-color".to_owned(),
                    owned(Value::from((0.0, 0.5, 1.0))),
                ),
                ("contrast".to_owned(), owned(Value::U32(0))),
            ]),
        };
        let server = std::thread::spawn(move || {
            zbus::blocking::connection::Builder::unix_stream(server_socket)
                .server(GUID)
                .expect("guid")
                .p2p()
                .serve_at(PATH, portal)
                .expect("serve")
                .build()
                .expect("server connection")
        });
        let client = zbus::blocking::connection::Builder::unix_stream(client_socket)
            .p2p()
            .build()
            .expect("client connection");
        let server = server.join().expect("server thread");

        let (published, readings) = mpsc::channel();
        let follower = std::thread::spawn(move || {
            follow(&client, |appearance| {
                let _ = published.send(appearance);
            })
        });
        let next = || readings.recv_timeout(crate::HANG_BOUND).expect("a reading");

        assert_eq!(
            next(),
            HostAppearance {
                scheme: Some(Scheme::Light),
                accent: Some([0, 128, 255]),
                high_contrast: false,
                reduce_motion: false,
            },
            "the initial read"
        );

        let emit = |namespace: &str, key: &str, value: Value<'_>| {
            server
                .emit_signal(
                    None::<zbus::names::BusName<'_>>,
                    PATH,
                    "org.freedesktop.portal.Settings",
                    "SettingChanged",
                    &(namespace, key, value),
                )
                .expect("emit");
        };
        emit("org.gnome.desktop.interface", "color-scheme", Value::U32(1));
        emit(NAMESPACE, "color-scheme", Value::U32(1));
        let dark = next();
        assert_eq!(
            dark.scheme,
            Some(Scheme::Dark),
            "another namespace is ignored"
        );
        assert_eq!(dark.accent, Some([0, 128, 255]));
        emit(NAMESPACE, "contrast", Value::U32(1));
        assert!(next().high_contrast);
        emit(NAMESPACE, "accent-color", Value::from((1.0, 0.0, 0.0)));
        assert_eq!(next().accent, Some([255, 0, 0]));

        drop(server);
        assert_eq!(
            readings.recv_timeout(crate::HANG_BOUND),
            Err(mpsc::RecvTimeoutError::Disconnected),
            "the follower ends with the connection"
        );
        let _ = follower.join().expect("follower thread");
    }
}

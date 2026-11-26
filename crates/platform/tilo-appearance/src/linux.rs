//! Appearance from the XDG Settings portal's `org.freedesktop.appearance`:
//! colour scheme, accent, contrast and reduced motion, followed live as the
//! desktop changes them. A background thread owns the D-Bus connection; a
//! desktop without the portal leaves every preference unset.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::zvariant::{OwnedValue, Value};

use crate::{Appearance, Scheme};
use tilo_lock::MutexRecover as _;

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

pub(crate) type Reading = Arc<Mutex<Appearance>>;

pub(crate) fn follow(changed: impl Fn() + Send + 'static) -> Reading {
    let reading = Reading::default();
    let shared = Arc::clone(&reading);
    let spawned = std::thread::Builder::new()
        .name("tilo-appearance".into())
        .spawn(move || {
            let followed = zbus::blocking::Connection::session().and_then(|connection| {
                watch(&connection, |appearance| {
                    *shared.lock_recover() = appearance;
                    changed();
                })
            });
            if let Err(error) = followed {
                tracing::debug!(%error, "no appearance from the Settings portal");
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "cannot follow the desktop appearance");
    }
    reading
}

pub(crate) fn current(reading: &Reading) -> Appearance {
    *reading.lock_recover()
}

/// Publishes the portal's appearance, then the appearance after each change,
/// until the connection closes.
fn watch(
    connection: &zbus::blocking::Connection,
    mut publish: impl FnMut(Appearance),
) -> zbus::Result<()> {
    let settings = SettingsProxy::new(connection)?;
    // Subscribed before the read, so a change in between is not lost.
    let changes = settings.receive_setting_changed()?;
    let mut appearance = Appearance::default();
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
fn apply(appearance: &mut Appearance, key: &str, value: &Value<'_>) -> bool {
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

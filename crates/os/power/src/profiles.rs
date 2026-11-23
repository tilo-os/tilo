//! power-profiles-daemon D-Bus integration.

use std::collections::HashMap;

use zbus::zvariant::OwnedValue;

const DEST: &str = "net.hadess.PowerProfiles";
const PATH: &str = "/net/hadess/PowerProfiles";
pub(crate) const IFACE: &str = "net.hadess.PowerProfiles";

#[derive(Clone)]
pub struct PowerProfilesMonitor {
    proxy: zbus::Proxy<'static>,
}

impl PowerProfilesMonitor {
    pub async fn with_connection(connection: &zbus::Connection) -> zbus::Result<Self> {
        let proxy = zbus::proxy::Builder::<zbus::Proxy<'static>>::new(connection)
            .destination(DEST)?
            .path(PATH)?
            .interface(IFACE)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        Ok(Self { proxy })
    }

    pub async fn active_profile(&self) -> zbus::Result<String> {
        self.proxy.get_property("ActiveProfile").await
    }

    /// Switch to `profile` (`power-saver`, `balanced` or `performance`).
    pub async fn set_active_profile(&self, profile: &str) -> zbus::Result<()> {
        Ok(self.proxy.set_property("ActiveProfile", profile).await?)
    }

    /// The profiles the daemon advertises, in its order.
    pub async fn profiles(&self) -> zbus::Result<Vec<String>> {
        let profiles: Vec<HashMap<String, OwnedValue>> =
            self.proxy.get_property("Profiles").await?;
        Ok(profile_names(&profiles))
    }

    /// Follow `org.freedesktop.DBus.Properties.PropertiesChanged` signals on
    /// `net.hadess.PowerProfiles` without maintaining a local property cache.
    pub async fn receive_properties_changed(
        &self,
    ) -> zbus::Result<zbus::fdo::PropertiesChangedStream> {
        let properties = zbus::fdo::PropertiesProxy::builder(self.proxy.connection())
            .destination(DEST)?
            .path(PATH)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        properties.receive_properties_changed().await
    }

    /// Follow bus-name owner transitions for `net.hadess.PowerProfiles`.
    pub async fn receive_owner_changed(
        &self,
    ) -> zbus::Result<zbus::proxy::OwnerChangedStream<'static>> {
        self.proxy.receive_owner_changed().await
    }

    /// Whether the underlying D-Bus connection talks to a message bus daemon
    /// (as opposed to a direct peer-to-peer socket in tests).
    pub(crate) fn is_bus(&self) -> bool {
        self.proxy.connection().is_bus()
    }

    /// Whether `net.hadess.PowerProfiles` currently has an owner on the bus.
    pub async fn has_owner(&self) -> zbus::Result<bool> {
        let dbus = zbus::fdo::DBusProxy::builder(self.proxy.connection())
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        let name = zbus::names::BusName::try_from(DEST)?;
        dbus.name_has_owner(name).await.map_err(Into::into)
    }
}

/// Extract profile names from the daemon's `Profiles` property entries, preserving order.
pub(crate) fn profile_names(profiles: &[HashMap<String, OwnedValue>]) -> Vec<String> {
    profiles
        .iter()
        .filter_map(|profile| String::try_from(profile.get("Profile")?.try_clone().ok()?).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use zbus::Guid;
    use zbus::connection::Builder;
    use zbus::zvariant::Value;

    use super::*;

    struct FakeDaemon {
        active: Arc<Mutex<String>>,
        profiles: Vec<&'static str>,
    }

    #[zbus::interface(name = "net.hadess.PowerProfiles")]
    impl FakeDaemon {
        #[zbus(property)]
        fn active_profile(&self) -> String {
            self.active.lock().expect("active").clone()
        }

        #[zbus(property)]
        fn set_active_profile(&self, profile: String) -> zbus::fdo::Result<()> {
            if !self.profiles.contains(&profile.as_str()) {
                return Err(zbus::fdo::Error::InvalidArgs(profile));
            }
            *self.active.lock().expect("active") = profile;
            Ok(())
        }

        #[zbus(property)]
        fn profiles(&self) -> Vec<HashMap<String, OwnedValue>> {
            self.profiles
                .iter()
                .map(|name| {
                    HashMap::from([
                        (
                            "Profile".to_owned(),
                            OwnedValue::try_from(Value::from(*name)).expect("profile"),
                        ),
                        (
                            "Driver".to_owned(),
                            OwnedValue::try_from(Value::from("platform_profile")).expect("driver"),
                        ),
                    ])
                })
                .collect()
        }
    }

    #[tokio::test]
    async fn profiles_are_read_and_switched_over_the_bus() {
        let active = Arc::new(Mutex::new("balanced".to_owned()));
        let (server, client) = tokio::net::UnixStream::pair().expect("socketpair");
        let server = Builder::unix_stream(server)
            .server(Guid::generate())
            .expect("server")
            .p2p()
            .serve_at(
                PATH,
                FakeDaemon {
                    active: Arc::clone(&active),
                    profiles: vec!["performance", "balanced", "power-saver"],
                },
            )
            .expect("daemon")
            .build();
        let client = Builder::unix_stream(client).p2p().build();
        let (server, client) = tokio::join!(server, client);
        let (_server, client) = (server.expect("server"), client.expect("client"));
        let monitor = PowerProfilesMonitor::with_connection(&client)
            .await
            .expect("proxy");
        assert_eq!(
            monitor.profiles().await.expect("profiles"),
            ["performance", "balanced", "power-saver"]
        );
        assert_eq!(monitor.active_profile().await.expect("active"), "balanced");
        monitor
            .set_active_profile("power-saver")
            .await
            .expect("switch");
        assert_eq!(*active.lock().expect("active"), "power-saver");
        assert!(monitor.set_active_profile("turbo").await.is_err());
    }
}

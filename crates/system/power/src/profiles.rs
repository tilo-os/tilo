//! power-profiles-daemon D-Bus integration.

const DEST: &str = "net.hadess.PowerProfiles";
const PATH: &str = "/net/hadess/PowerProfiles";
const IFACE: &str = "net.hadess.PowerProfiles";

#[derive(Clone)]
pub struct PowerProfilesMonitor {
    proxy: zbus::Proxy<'static>,
}

impl PowerProfilesMonitor {
    pub async fn with_connection(connection: &zbus::Connection) -> zbus::Result<Self> {
        let proxy = zbus::Proxy::new(connection, DEST, PATH, IFACE).await?;
        Ok(Self { proxy })
    }

    pub async fn active_profile(&self) -> zbus::Result<String> {
        self.proxy.get_property("ActiveProfile").await
    }

    pub async fn receive_active_profile_changed(&self) -> zbus::proxy::PropertyStream<'_, String> {
        self.proxy.receive_property_changed("ActiveProfile").await
    }
}

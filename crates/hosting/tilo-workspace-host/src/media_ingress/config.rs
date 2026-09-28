//! Explicit trusted-operator local installation settings.
use super::*;
use serde::Deserialize;
use std::path::{Path, PathBuf};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperatorConfig {
    delivery_version: u16,
    #[serde(default)]
    activate_delivery: bool,
    #[serde(default)]
    local_owner_enrollment: bool,
    origin: String,
    /// TCP and UDP bind address. Absent, the host binds the loopback address
    /// the origin names. Required for a public origin, which an edge forwards
    /// here (TLS passthrough on TCP, datagrams on UDP).
    #[serde(default)]
    listen: Option<std::net::SocketAddr>,
    certificate_der: PathBuf,
    key_der: PathBuf,
    assets: PathBuf,
}
impl OperatorConfig {
    fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= 16 * 1024, "operator config exceeds bound");
        let config: Self = serde_json::from_slice(bytes)?;
        ensure!(config.delivery_version == 3, "delivery_version 3 required");
        let uri: hyper::Uri = config.origin.parse()?;
        let authority = uri.authority().context("origin authority missing")?;
        let loopback = matches!(authority.host(), "localhost" | "127.0.0.1" | "[::1]");
        ensure!(
            !authority.as_str().contains('@')
                && uri.scheme_str() == Some("https")
                && config.origin == format!("https://{authority}")
                && (loopback || crate::https::is_dns_name(authority.host()))
                && authority.port_u16().unwrap_or(443) != 0,
            "exact HTTPS origin with a loopback or DNS host required"
        );
        ensure!(
            loopback || config.listen.is_some(),
            "a public origin requires an explicit listen address"
        );
        ensure!(
            !config.local_owner_enrollment
                || (loopback && config.listen.is_none_or(|listen| listen.ip().is_loopback())),
            "local owner enrollment requires a loopback origin and listener"
        );
        ensure!(
            config.listen.is_none_or(|listen| listen.port() != 0),
            "listen address requires a fixed port"
        );
        ensure!(
            config.certificate_der.is_absolute()
                && config.key_der.is_absolute()
                && config.assets.is_absolute(),
            "absolute deployment paths required"
        );
        Ok(config)
    }

    /// The configured listener, else the loopback address the origin names.
    async fn bind_address(&self) -> Result<std::net::SocketAddr> {
        if let Some(listen) = self.listen {
            return Ok(listen);
        }
        let uri: hyper::Uri = self.origin.parse()?;
        let authority = uri.authority().context("origin authority missing")?;
        let host = authority
            .host()
            .trim_start_matches('[')
            .trim_end_matches(']');
        tokio::net::lookup_host((host, authority.port_u16().unwrap_or(443)))
            .await?
            .find(|v| v.ip().is_loopback())
            .context("loopback address missing")
    }
}
pub(crate) struct PreparedIngress {
    listener: TcpListener,
    endpoint: quinn::Endpoint,
    config: IngressConfig,
    activate_delivery: bool,
    local_owner_enrollment: bool,
    assets_path: PathBuf,
    channels: Option<(Ingress, https::IngressReceiver)>,
}
impl PreparedIngress {
    pub(crate) fn host_bytes(&self) -> Result<crate::host_bytes::HostBytes> {
        Ok(self
            .channels
            .as_ref()
            .context("prepared ingress consumed")?
            .0
            .host_bytes())
    }
    pub(crate) fn host_work(&self) -> Result<crate::store::HostWorkInventory> {
        Ok(self
            .channels
            .as_ref()
            .context("prepared ingress consumed")?
            .0
            .host_work())
    }
    pub(crate) async fn from_env() -> Result<Option<Self>> {
        let Some(path) = std::env::var_os("TILO_SESSION_HTTPS_CONFIG") else {
            return Ok(None);
        };
        Ok(Some(Self::load(Path::new(&path)).await?))
    }
    async fn load(path: &Path) -> Result<Self> {
        ensure!(path.is_absolute(), "absolute operator config path required");
        let config = OperatorConfig::decode(&read_trusted(path, 16 * 1024, false)?)?;
        // One original pool/inventory exists before owner activation or restore.
        let channels = https::try_ingress_channel()?;
        let certificates = vec![CertificateDer::from(read_trusted(
            &config.certificate_der,
            64 * 1024,
            false,
        )?)];
        let key = PrivateKeyDer::try_from(read_trusted(&config.key_der, 16 * 1024, true)?)
            .map_err(anyhow::Error::msg)?;
        let assets_path = config.assets.canonicalize()?;
        let assets = AssetRoot::open(&assets_path)?;
        let address = config.bind_address().await?;
        let listener = TcpListener::bind(address).await?;
        let endpoint = super::endpoint(
            listener.local_addr()?,
            certificates.clone(),
            key.clone_key(),
        )?;
        Ok(Self {
            listener,
            endpoint,
            config: IngressConfig {
                origin: config.origin,
                assets,
                certificates,
                key,
                local_enrollment: None,
            },
            activate_delivery: config.activate_delivery,
            local_owner_enrollment: config.local_owner_enrollment,
            assets_path,
            channels: Some(channels),
        })
    }

    /// Like [`Self::load`], but use already-bound TCP and UDP sockets whose
    /// local addresses match the configured origin. Production keeps binding
    /// from the origin; this entry point is for tests and socket activation.
    async fn load_bound(
        path: &Path,
        tcp: std::net::TcpListener,
        udp: std::net::UdpSocket,
    ) -> Result<Self> {
        ensure!(path.is_absolute(), "absolute operator config path required");
        let config = OperatorConfig::decode(&read_trusted(path, 16 * 1024, false)?)?;
        let channels = https::try_ingress_channel()?;
        let certificates = vec![CertificateDer::from(read_trusted(
            &config.certificate_der,
            64 * 1024,
            false,
        )?)];
        let key = PrivateKeyDer::try_from(read_trusted(&config.key_der, 16 * 1024, true)?)
            .map_err(anyhow::Error::msg)?;
        let assets_path = config.assets.canonicalize()?;
        let assets = AssetRoot::open(&assets_path)?;
        let address = config.bind_address().await?;
        let tcp_addr = tcp.local_addr()?;
        let udp_addr = udp.local_addr()?;
        ensure!(
            tcp_addr == address && udp_addr == address,
            "bound sockets must match the configured origin"
        );
        tcp.set_nonblocking(true)?;
        let listener = TcpListener::from_std(tcp)?;
        let endpoint = super::endpoint_from_socket(udp, certificates.clone(), key.clone_key())?;
        Ok(Self {
            listener,
            endpoint,
            config: IngressConfig {
                origin: config.origin,
                assets,
                certificates,
                key,
                local_enrollment: None,
            },
            activate_delivery: config.activate_delivery,
            local_owner_enrollment: config.local_owner_enrollment,
            assets_path,
            channels: Some(channels),
        })
    }

    pub(crate) fn activate_owner(&self, owner: &mut SessionOwner) -> Result<()> {
        let root = owner.root().canonicalize()?;
        ensure!(
            !self.assets_path.starts_with(&root) && !root.starts_with(&self.assets_path),
            "assets and native custody must be separate"
        );
        ensure!(
            owner.permits_publication() && owner.host_certificate().capabilities.can_act_as_host,
            "current native host authority required"
        );
        let now = web_time::SystemTime::now();
        owner
            .peer()
            .permissions(owner.peer().authority().device_public_key, now)
            .map_err(anyhow::Error::msg)?;
        if !owner.peer().admission_v5_selected() {
            ensure!(
                self.activate_delivery,
                "activate_delivery true required for schema transition"
            );
            owner.activate_delivery()?;
        }
        owner.install_delivery_transport(
            &self.config.origin,
            crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
            now,
        )?;
        Ok(())
    }
    pub(crate) async fn start(
        mut self,
        owner: &mut SessionOwner,
        current: CommittedAuthority,
        enrollment: Option<(
            Arc<crate::enrollment_http::Authorization>,
            tokio::sync::mpsc::Sender<crate::enrollment_http::EnrollmentCommand>,
        )>,
    ) -> Result<InstalledIngress> {
        if self.local_owner_enrollment {
            let (authorization, commands) =
                enrollment.context("private local owner enrollment custody required")?;
            self.config.local_enrollment = Some(Arc::new(crate::https::LocalEnrollment::new(
                &self.config.origin,
                authorization,
                commands,
            )?));
        }
        let channels = self.channels.take().context("prepared ingress consumed")?;
        InstalledIngress::start_bound(
            owner,
            current,
            self.listener,
            self.config,
            self.endpoint,
            channels,
        )
        .await
    }
}
fn read_trusted(path: &Path, max: usize, private: bool) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= max as u64,
        "invalid bounded deployment file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() }
                && metadata.permissions().mode() & if private { 0o077 } else { 0o022 } == 0,
            "deployment custody permissions denied"
        );
    }
    let mut bytes = Vec::new();
    file.take((max + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "deployment file exceeds bound");
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operator_config_requires_explicit_version_and_trusted_absolute_paths() {
        let valid=br#"{"delivery_version":3,"activate_delivery":true,"origin":"https://localhost:9443","certificate_der":"/tmp/cert.der","key_der":"/tmp/key.der","assets":"/tmp/assets"}"#;
        let config = OperatorConfig::decode(valid).unwrap();
        assert!(config.activate_delivery);
        for bad in [
            String::from_utf8(valid.to_vec())
                .unwrap()
                .replace("\"delivery_version\":3,", ""),
            String::from_utf8(valid.to_vec())
                .unwrap()
                .replace("\"delivery_version\":3", "\"delivery_version\":2"),
            String::from_utf8(valid.to_vec())
                .unwrap()
                .replace("/tmp/key.der", "key.der"),
            String::from_utf8(valid.to_vec())
                .unwrap()
                .replace("https://localhost:9443", "https://user@localhost:9443"),
            String::from_utf8(valid.to_vec())
                .unwrap()
                .replace("https://localhost:9443", "https://localhost:9443/"),
            String::from_utf8(valid.to_vec())
                .unwrap()
                .replace("\"activate_delivery\":true", "\"unknown\":true"),
        ] {
            assert!(OperatorConfig::decode(bad.as_bytes()).is_err())
        }
        assert!(OperatorConfig::decode(&vec![b' '; 16385]).is_err());
    }
    #[test]
    fn public_origin_requires_an_explicit_listener_and_no_local_owner_gateway() {
        let config = |fields: serde_json::Value| {
            let mut value = serde_json::json!({"delivery_version":3,"certificate_der":"/c.der","key_der":"/k.der","assets":"/assets"});
            value
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            OperatorConfig::decode(&serde_json::to_vec(&value).unwrap())
        };
        let public = config(
            serde_json::json!({"origin":"https://c1.viewer.example","listen":"0.0.0.0:7392"}),
        )
        .unwrap();
        assert_eq!(public.listen, Some("0.0.0.0:7392".parse().unwrap()));
        assert!(
            config(
                serde_json::json!({"origin":"https://c1.viewer.example:8443","listen":"[::]:7392"})
            )
            .is_ok()
        );
        assert!(
            config(serde_json::json!({"origin":"https://localhost:7392","listen":"0.0.0.0:7392"}))
                .is_ok()
        );
        for bad in [
            serde_json::json!({"origin":"https://c1.viewer.example"}),
            serde_json::json!({"origin":"https://c1.viewer.example","listen":"0.0.0.0:0"}),
            serde_json::json!({"origin":"https://C1.viewer.example","listen":"0.0.0.0:7392"}),
            serde_json::json!({"origin":"https://203.0.113.9","listen":"0.0.0.0:7392"}),
            serde_json::json!({"origin":"https://-bad.example","listen":"0.0.0.0:7392"}),
            serde_json::json!({"origin":"https://c1.viewer.example/app","listen":"0.0.0.0:7392"}),
            serde_json::json!({"origin":"https://c1.viewer.example","listen":"0.0.0.0:7392","local_owner_enrollment":true}),
            serde_json::json!({"origin":"https://localhost:7392","listen":"0.0.0.0:7392","local_owner_enrollment":true}),
        ] {
            assert!(config(bad.clone()).is_err(), "{bad}");
        }
    }
    #[tokio::test]
    async fn public_origin_binds_tcp_and_udp_on_the_listen_address() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cert = rcgen::generate_simple_self_signed(vec!["c1.viewer.example".into()]).unwrap();
        let cert_path = dir.path().join("cert.der");
        let key_path = dir.path().join("key.der");
        let assets = dir.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(&cert_path, cert.cert.der()).unwrap();
        std::fs::write(&key_path, cert.signing_key.serialize_der()).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let temporary = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let listen = temporary.local_addr().unwrap();
        drop(temporary);
        let config = dir.path().join("operator.json");
        std::fs::write(&config, serde_json::to_vec(&serde_json::json!({"delivery_version":3,"origin":"https://c1.viewer.example","listen":listen,"certificate_der":cert_path,"key_der":key_path,"assets":assets})).unwrap()).unwrap();
        let prepared = PreparedIngress::load(&config).await.unwrap();
        assert_eq!(prepared.listener.local_addr().unwrap(), listen);
        assert_eq!(prepared.endpoint.local_addr().unwrap(), listen);
        assert_eq!(prepared.config.origin, "https://c1.viewer.example");
    }
    #[tokio::test]
    async fn trusted_config_requires_explicit_upgrade_and_rejects_public_key_custody() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut owner = SessionOwner::initialize(&dir.path().join("state")).unwrap();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .unwrap();
        let cert_path = dir.path().join("cert.der");
        let key_path = dir.path().join("key.der");
        let assets = dir.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(&cert_path, cert.cert.der()).unwrap();
        std::fs::write(&key_path, cert.signing_key.serialize_der()).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let temporary = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = temporary.local_addr().unwrap().port();
        drop(temporary);
        let config = dir.path().join("operator.json");
        std::fs::write(&config,serde_json::to_vec(&serde_json::json!({"delivery_version":3,"origin":format!("https://127.0.0.1:{port}"),"certificate_der":cert_path,"key_der":key_path,"assets":assets})).unwrap()).unwrap();
        let mut prepared = PreparedIngress::load(&config).await.unwrap();
        let original_pool = prepared
            .host_bytes()
            .expect("pool exists before owner activation");
        let before = owner.committed_generation();
        assert!(prepared.activate_owner(&mut owner).is_err());
        assert_eq!(owner.committed_generation(), before);
        assert!(!owner.peer().admission_v5_selected());
        prepared.activate_delivery = true;
        prepared.activate_owner(&mut owner).unwrap();
        assert!(owner.peer().admission_v5_selected());
        assert!(owner.stored_delivery_hint().is_ok());
        let current = CommittedAuthority::new(&owner);
        let mut installed = prepared.start(&mut owner, current, None).await.unwrap();
        assert!(original_pool.same_pool(&installed.receiver.host_bytes()));
        assert_eq!(installed.registrations.load(Ordering::Acquire), 3);
        installed.tasks.abort_all();
        while installed.tasks.join_next().await.is_some() {}
        assert_eq!(installed.registrations.load(Ordering::Acquire), 0);
        // Failure is visible without waiting for the host recv branch.
        assert!(installed.discovery_hint(&owner).is_err());
        assert!(
            installed
                .refresh_at(&mut owner, tilo_delivery_protocol::clock::now_ns())
                .is_err()
        );
        installed.shutdown().await.unwrap();
        let mut enabled: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        enabled["local_owner_enrollment"] = serde_json::json!(true);
        // Independent configuration cases use distinct origins. Quinn's socket
        // driver is released asynchronously after the stopped endpoint drops.
        let fresh = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        enabled["origin"] = serde_json::json!(format!(
            "https://127.0.0.1:{}",
            fresh.local_addr().unwrap().port()
        ));
        drop(fresh);
        std::fs::write(&config, serde_json::to_vec(&enabled).unwrap()).unwrap();
        let prepared = PreparedIngress::load(&config).await.unwrap();
        let current = CommittedAuthority::new(&owner);
        let before = owner.committed_generation();
        assert!(prepared.start(&mut owner, current, None).await.is_err());
        assert_eq!(
            owner.committed_generation(),
            before,
            "opt-in is not private authorization"
        );
        let fresh = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        enabled["origin"] = serde_json::json!(format!(
            "https://127.0.0.1:{}",
            fresh.local_addr().unwrap().port()
        ));
        drop(fresh);
        std::fs::write(&config, serde_json::to_vec(&enabled).unwrap()).unwrap();
        let prepared = PreparedIngress::load(&config).await.unwrap();
        prepared.activate_owner(&mut owner).unwrap();
        let authorization = Arc::new(
            crate::enrollment_http::Authorization::local(owner.root(), "127.0.0.1:4000".into())
                .unwrap(),
        );
        let (commands, _receiver) = tokio::sync::mpsc::channel(64);
        let current = CommittedAuthority::new(&owner);
        let installed = prepared
            .start(&mut owner, current, Some((authorization, commands)))
            .await
            .unwrap();
        assert!(installed.discovery_hint(&owner).is_ok());
        installed.shutdown().await.unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(PreparedIngress::load(&config).await.is_err());
        assert!(owner.peer().admission_v5_selected());
    }
}

/// Explicit prepared sockets and shared resource inventory, not an owner witness.
pub struct PreparedTransport(PreparedIngress);
impl PreparedTransport {
    pub async fn load_explicit(path: &Path) -> Result<Self> {
        let prepared = PreparedIngress::load(path).await?;
        ensure!(
            !prepared.local_owner_enrollment,
            "native local-owner cookie gateway is not a reusable owner adapter"
        );
        Ok(Self(prepared))
    }

    /// Prepare from config using already-bound TCP and UDP sockets for the
    /// configured origin. Used by tests and available for socket activation;
    /// production keeps binding via [`Self::load_explicit`].
    pub async fn load_explicit_bound(
        path: &Path,
        tcp: std::net::TcpListener,
        udp: std::net::UdpSocket,
    ) -> Result<Self> {
        let prepared = PreparedIngress::load_bound(path, tcp, udp).await?;
        ensure!(
            !prepared.local_owner_enrollment,
            "native local-owner cookie gateway is not a reusable owner adapter"
        );
        Ok(Self(prepared))
    }
    pub fn host_bytes(&self) -> Result<crate::host_bytes::HostBytes> {
        self.0.host_bytes()
    }
    pub fn host_work(&self) -> Result<crate::store::HostWorkInventory> {
        self.0.host_work()
    }
    pub fn origin(&self) -> &str {
        &self.0.config.origin
    }
    pub fn activation_requested(&self) -> bool {
        self.0.activate_delivery
    }
    pub fn ensure_store_separate(&self, root: &Path) -> Result<()> {
        let root = root.canonicalize()?;
        ensure!(
            !self.0.assets_path.starts_with(&root) && !root.starts_with(&self.0.assets_path),
            "assets and owner custody must be separate"
        );
        Ok(())
    }
    pub async fn start(
        self,
        authority: Arc<dyn tilo_moq::relay::RelayAuthority>,
    ) -> Result<super::transport::RunningTransport> {
        let mut inner = self.0;
        let channels = inner.channels.take().context("prepared ingress consumed")?;
        super::transport::RunningTransport::start_bound(
            inner.listener,
            inner.config,
            inner.endpoint,
            channels,
            tilo_moq::relay::Relay::new(authority),
        )
        .await
    }
}

#[cfg(test)]
mod reusable_tests {
    use super::*;
    struct Deny;
    impl tilo_moq::relay::RelayAuthority for Deny {
        fn admit(
            &self,
            _: &[u8],
        ) -> std::result::Result<
            Arc<dyn tilo_moq::relay::SessionAuthorization>,
            tilo_moq::relay::Denied,
        > {
            Err(tilo_moq::relay::Denied)
        }
    }
    #[tokio::test]
    async fn reusable_transport_moves_original_pool_into_actual_tcp_udp_and_stops() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .unwrap();
        let cert_path = dir.path().join("cert.der");
        let key_path = dir.path().join("key.der");
        let assets = dir.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(&cert_path, cert.cert.der()).unwrap();
        std::fs::write(&key_path, cert.signing_key.serialize_der()).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let temporary = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = temporary.local_addr().unwrap().port();
        drop(temporary);
        let config = dir.path().join("operator.json");
        std::fs::write(&config, serde_json::to_vec(&serde_json::json!({"delivery_version":3,"origin":format!("https://127.0.0.1:{port}"),"certificate_der":cert_path,"key_der":key_path,"assets":assets})).unwrap()).unwrap();
        let prepared = PreparedTransport::load_explicit(&config).await.unwrap();
        let budget = prepared.host_bytes().unwrap();
        let inventory = prepared.host_work().unwrap();
        assert!(!prepared.activation_requested());
        assert!(prepared.ensure_store_separate(&assets).is_err());
        let mut running = prepared.start(Arc::new(Deny)).await.unwrap();
        assert!(running.is_live());
        assert!(budget.same_pool(&running.host_bytes()));
        assert!(inventory.same_pool(&running.host_bytes()));
        let lease = inventory.try_large_decode().unwrap();
        assert!(running.host_work().try_large_decode().is_err());
        drop(lease);
        running.tasks.abort_all();
        while running.tasks.join_next().await.is_some() {}
        assert!(!running.is_live());
        assert!(running.recv().await.is_none());
        running.shutdown().await.unwrap();
    }

    /// Holding the origin proves load-from-config cannot bind it; the bound
    /// entry point must accept those sockets instead of retrying another port.
    #[tokio::test]
    async fn reusable_transport_accepts_prebound_sockets_when_origin_is_held() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .unwrap();
        let cert_path = dir.path().join("cert.der");
        let key_path = dir.path().join("key.der");
        let assets = dir.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(&cert_path, cert.cert.der()).unwrap();
        std::fs::write(&key_path, cert.signing_key.serialize_der()).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut refused = Vec::new();
        let (udp, address, tcp) = loop {
            let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let address = udp.local_addr().unwrap();
            match std::net::TcpListener::bind(address) {
                Ok(tcp) => break (udp, address, tcp),
                Err(error) => {
                    assert!(refused.len() < 64, "no free TCP/UDP loopback pair: {error}");
                    refused.push(udp);
                }
            }
        };
        drop(refused);
        let config = dir.path().join("operator.json");
        std::fs::write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "delivery_version": 3,
                "origin": format!("https://{address}"),
                "certificate_der": cert_path,
                "key_der": key_path,
                "assets": assets,
            }))
            .unwrap(),
        )
        .unwrap();
        // While the reservation is held, binding from the config alone fails.
        let error = match PreparedTransport::load_explicit(&config).await {
            Ok(_) => panic!("expected bind failure while sockets are held"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("Address already in use"),
            "expected bind failure while sockets are held, got {error}"
        );
        // Handoff must use those sockets — never release and retry another port.
        let prepared = PreparedTransport::load_explicit_bound(&config, tcp, udp)
            .await
            .unwrap();
        assert_eq!(prepared.origin(), &format!("https://{address}"));
        let running = prepared.start(Arc::new(Deny)).await.unwrap();
        assert!(running.is_live());
        assert_eq!(running.origin(), &format!("https://{address}"));
        running.shutdown().await.unwrap();
    }
}

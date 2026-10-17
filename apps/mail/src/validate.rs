//! Account **test-before-save**: before an account is persisted, the app
//! performs an IMAP login and an SMTP handshake, and saves it only if both
//! succeed. A specific error is surfaced otherwise.
//!
//! ## Injectable transport
//! The check runs through the [`ProbeTransport`] trait, so the state machine is
//! testable without a network: a mock returns canned success/failure. The
//! network probe ([`validate_account_cancellable`]) is compiled only under
//! `live`. The default
//! (offline) build uses [`OfflineProbe`], which reports success without opening
//! a socket so `--shots` and the headless tests can still save an account.

use crate::model::AccountConfig;

/// The connection probe seam. Implementors attempt the two round-trips that
/// prove an account works.
pub trait ProbeTransport: Sync {
    /// Attempt an IMAP `LOGIN` (or `AUTHENTICATE XOAUTH2`) to the account's
    /// incoming server. `Ok(())` on a successful greeting + auth.
    ///
    /// # Errors
    ///
    /// Returns the server's or transport's reason when the connection,
    /// TLS handshake, or authentication fails.
    fn imap_login(&self, cfg: &AccountConfig) -> Result<(), String>;
    /// Attempt an SMTP connect + `EHLO` (+ `AUTH`) to the outgoing server.
    ///
    /// # Errors
    ///
    /// Returns the server's or transport's reason when the connection,
    /// TLS handshake, or authentication fails.
    fn smtp_handshake(&self, cfg: &AccountConfig) -> Result<(), String>;
}

/// The outcome of a validation attempt. Drives the setup sheet: `Ok` persists,
/// `Failed` keeps the sheet open with the message shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationState {
    /// The config itself is incomplete (before any network is touched).
    Invalid(String),
    /// Both IMAP and SMTP succeeded — safe to persist.
    Ok,
    /// A round-trip failed: what it says to the person, and where.
    Failed(Failure),
}

/// What the person has to change for the account to work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trouble {
    /// The server refused the password (or the sign-in).
    Password,
    /// The incoming server could not be reached or spoken to.
    Incoming,
    /// The outgoing server could not be reached or spoken to.
    Outgoing,
    /// Nothing in the form: the check itself stopped.
    Other,
}

/// A failed check: the field it is about and what to say there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub trouble: Trouble,
    pub message: String,
}

impl Failure {
    /// A failure that is not about any one field.
    pub fn other(message: impl Into<String>) -> Self {
        Self {
            trouble: Trouble::Other,
            message: message.into(),
        }
    }

    /// The server refused the password, saying `message`.
    pub fn password(message: impl Into<String>) -> Self {
        Self {
            trouble: Trouble::Password,
            message: message.into(),
        }
    }

    /// The incoming server refused or failed with `reason`.
    fn incoming(cfg: &AccountConfig, reason: &str) -> Self {
        if is_auth_failure(reason) {
            return Self {
                trouble: Trouble::Password,
                message: if cfg.auth == crate::model::AuthMethod::OAuth {
                    "The sign-in has expired. Sign in again.".into()
                } else {
                    "That password didn’t work.".into()
                },
            };
        }
        Self {
            trouble: Trouble::Incoming,
            message: unreachable_message(cfg.imap_host.trim(), reason),
        }
    }

    /// The outgoing server failed with `reason`.
    fn outgoing(cfg: &AccountConfig, reason: &str) -> Self {
        Self {
            trouble: Trouble::Outgoing,
            message: unreachable_message(cfg.smtp_host.trim(), reason),
        }
    }
}

/// "Can’t reach the server": the host by name, and why in plain words.
fn unreachable_message(host: &str, reason: &str) -> String {
    let host = if host.is_empty() { "the server" } else { host };
    format!("Can’t reach {host}: {reason}.")
}

impl ValidationState {
    /// Whether this outcome should persist the account.
    pub fn persisted(&self) -> bool {
        matches!(self, ValidationState::Ok)
    }

    /// The user-facing message for a non-Ok outcome (empty for `Ok`).
    pub fn message(&self) -> String {
        match self {
            ValidationState::Ok => String::new(),
            ValidationState::Invalid(m) => m.clone(),
            ValidationState::Failed(failure) => failure.message.clone(),
        }
    }
}

/// Run the full check: config-level validation first (no network), then IMAP
/// and SMTP together. The two servers are independent, so the person waits for
/// the slower one rather than for both in turn; when both fail, the IMAP
/// reason is the one shown, as the most upstream problem.
pub fn validate_account<T: ProbeTransport + ?Sized>(
    cfg: &AccountConfig,
    transport: &T,
) -> ValidationState {
    if cfg.import_pending {
        return ValidationState::Invalid(crate::i18n::t("mail-import-sign-in-required"));
    }

    if let Some((_, msg)) = cfg.validate().into_iter().next() {
        return ValidationState::Invalid(msg);
    }
    let (imap, smtp) = std::thread::scope(|scope| {
        let smtp = scope.spawn(|| transport.smtp_handshake(cfg));
        let imap = transport.imap_login(cfg);
        let smtp = smtp
            .join()
            .unwrap_or_else(|_| Err("the check stopped unexpectedly".into()));
        (imap, smtp)
    });
    if let Err(e) = imap {
        return ValidationState::Failed(Failure::incoming(cfg, &e));
    }
    if let Err(e) = smtp {
        return ValidationState::Failed(Failure::outgoing(cfg, &e));
    }
    ValidationState::Ok
}

/// Classify a raw connection/transport error string into a specific,
/// user-facing reason. Pure + dependency-free so it is compiled and tested in
/// every build; the live IMAP/SMTP paths and the account probe both funnel
/// their errors through it, so a "connection refused" reads the same whether it
/// came from a fetch or a save-time check.
pub fn classify_conn_failure(detail: &str) -> String {
    let d = detail.to_ascii_lowercase();
    // Auth first: a 535 / "authentication failed" is more specific than the
    // generic transport keywords that might co-occur in the message.
    if is_auth_failure(&d) {
        return "authentication failed — check the username and password".into();
    }
    if d.contains("certificate")
        || d.contains("tls")
        || d.contains("handshake")
        || d.contains("self-signed")
        || d.contains("ssl")
    {
        return "TLS/certificate error — the server's certificate wasn't trusted".into();
    }
    if d.contains("refused") {
        return "connection refused — check the host and port".into();
    }
    if d.contains("timed out") || d.contains("timeout") {
        return "connection timed out — the server didn't respond".into();
    }
    if d.contains("dns")
        || d.contains("lookup")
        || d.contains("resolve")
        || d.contains("name or service not known")
        || d.contains("not known")
        || d.contains("no such host")
    {
        return "host not found — check the server address".into();
    }
    if d.contains("unreachable") || d.contains("network is unreachable") {
        return "network unreachable — check your connection".into();
    }
    // Preserve the underlying detail rather than swallowing it.
    format!("connection failed: {detail}")
}

/// Whether a server's or transport's error says the credentials were
/// refused, rather than that the server could not be reached.
pub fn is_auth_failure(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    d.contains("535")
        || d.contains("authenticationfailed")
        || d.contains("authentication failed")
        || d.contains("invalid credentials")
        || d.contains("auth")
            && (d.contains("fail")
                || d.contains("reject")
                || d.contains("denied")
                || d.contains("invalid"))
        || d.contains("login failed")
        || d.contains("enter a password")
}

/// The offline probe used by the default build / `--shots`: it never opens a
/// socket and reports success, so an account can still be saved without a
/// network. (Config-level validation in [`validate_account`] still runs.)
pub struct OfflineProbe;

impl ProbeTransport for OfflineProbe {
    fn imap_login(&self, _cfg: &AccountConfig) -> Result<(), String> {
        Ok(())
    }
    fn smtp_handshake(&self, _cfg: &AccountConfig) -> Result<(), String> {
        Ok(())
    }
}

/// Validate on a setup worker, stopping network waits when its sheet closes.
pub fn validate_account_cancellable(
    cfg: &AccountConfig,
    cancellation: &std::sync::atomic::AtomicBool,
) -> ValidationState {
    if cancellation.load(std::sync::atomic::Ordering::Relaxed) {
        return ValidationState::Failed(Failure::other("Account verification cancelled"));
    }
    #[cfg(feature = "live")]
    {
        struct SetupProbe<'a>(&'a std::sync::atomic::AtomicBool);
        impl ProbeTransport for SetupProbe<'_> {
            fn imap_login(&self, cfg: &AccountConfig) -> Result<(), String> {
                live_probe::imap_login(cfg, Some(self.0))
            }
            fn smtp_handshake(&self, cfg: &AccountConfig) -> Result<(), String> {
                live_probe::smtp_handshake(cfg, Some(self.0))
            }
        }
        validate_account(cfg, &SetupProbe(cancellation))
    }
    #[cfg(not(feature = "live"))]
    validate_account(cfg, &OfflineProbe)
}

#[cfg(feature = "live")]
mod live_probe {
    use std::sync::Arc;

    use super::{AccountConfig, classify_conn_failure};
    use crate::model::AuthMethod;

    /// Run a bounded network probe from its worker thread on the shared app
    /// runtime. Cancelling drops the network future and closes its sockets
    /// instead of leaving a detached worker waiting indefinitely for a remote
    /// TLS greeting. A runtime that cannot be used fails the probe: without
    /// the round-trips there is no honest "your account works".
    fn block_on<F: std::future::Future<Output = Result<(), String>>>(
        fut: F,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        ui::rt::try_app()
            .and_then(|runtime| runtime.block_on(async {
                tokio::select! {
                    biased;
                    () = async {
                        loop {
                            if cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
                                return;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                    } => Err("Account verification cancelled".into()),
                    result = tokio::time::timeout(std::time::Duration::from_secs(20), fut) => {
                        result.unwrap_or_else(|_| Err("Account verification timed out".into()))
                    }
                }
            }))
            .map_err(|e| e.to_string())?
    }

    /// # Panics
    /// Never in practice: the bundled ring provider always advertises the
    /// default protocol versions.
    #[allow(
        clippy::expect_used,
        reason = "the bundled ring provider always supports the default protocol versions; a TLS config with no usable version is not something the probe can degrade to"
    )]
    fn tls_config() -> Arc<rustls::ClientConfig> {
        Arc::new(tilo_net::client_config().expect("rustls default protocol versions"))
    }

    /// The access token / password to present. Mirrors the engine's resolution:
    /// a password from the config, or an OAuth token read from the vault.
    fn creds(cfg: &AccountConfig) -> Result<(String, Option<String>), String> {
        let user = if cfg.username.trim().is_empty() {
            cfg.email.clone()
        } else {
            cfg.username.clone()
        };
        match cfg.auth {
            AuthMethod::Password => {
                if cfg.password.is_empty() {
                    return Err("enter a password to verify the account".into());
                }
                Ok((user, None))
            }
            AuthMethod::OAuth => {
                let token = oauth_access_token(cfg)?;
                Ok((user, Some(token)))
            }
        }
    }

    /// A current OAuth access token for the account from custody (the
    /// sign-in flow sealed the grant before the account is saved).
    fn oauth_access_token(cfg: &AccountConfig) -> Result<String, String> {
        let provider = crate::oauth::OAuthProvider::for_account(&cfg.email, &cfg.imap_host)
            .ok_or("OAuth sign-in is not supported for this provider")?;
        crate::oauth::access_token(
            &account_custody::Custodian::live(),
            &cfg.email,
            provider,
            false,
        )
    }

    pub fn imap_login(
        cfg: &AccountConfig,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        if cfg.imap_host.trim().is_empty() {
            return Err("no IMAP host".into());
        }
        let (user, token) = creds(cfg)?;
        let tls = tls_config();
        block_on(
            async move {
                let host = cfg.imap_host.trim();
                let port = if cfg.imap_port == 0 {
                    993
                } else {
                    cfg.imap_port
                };
                let tcp = crate::connect::tcp(host, port).await?;
                let connector = tokio_rustls::TlsConnector::from(tls);
                let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
                    .map_err(|e| format!("invalid IMAP host name: {e}"))?;
                let stream = connector
                    .connect(server_name, tcp)
                    .await
                    .map_err(|e| classify_conn_failure(&e.to_string()))?;
                let client = async_imap::Client::new(stream);
                let mut session = match token {
                    Some(access) => {
                        let auth = crate::oauth::XOAuth2 {
                            user: user.clone(),
                            token: access,
                        };
                        client
                            .authenticate("XOAUTH2", auth)
                            .await
                            .map_err(|(e, _)| classify_conn_failure(&e.to_string()))?
                    }
                    None => client
                        .login(&user, &cfg.password)
                        .await
                        .map_err(|(e, _)| classify_conn_failure(&e.to_string()))?,
                };
                // The login is the proof. Saying goodbye is the server's
                // business: the check does not wait for its answer.
                tokio::spawn(async move {
                    let _ = session.logout().await;
                });
                Ok::<(), String>(())
            },
            cancellation,
        )
    }

    pub fn smtp_handshake(
        cfg: &AccountConfig,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        use lettre::transport::smtp::client::TlsParameters;

        if cfg.smtp_host.trim().is_empty() {
            return Err("no SMTP host".into());
        }
        let host = cfg.smtp_host.trim();
        let port = if cfg.smtp_port == 0 {
            465
        } else {
            cfg.smtp_port
        };
        // Credential validity is proven authoritatively by the IMAP login
        // (SMTP has no no-op AUTH probe); this proves the outgoing server is
        // reachable and speaks the negotiated security. Resolving them first
        // keeps an OAuth sign-in that lapsed from reading as a server fault.
        creds(cfg)?;
        let starttls = TlsParameters::new(host.to_owned())
            .map_err(|e| classify_conn_failure(&e.to_string()))?;
        block_on(
            async move {
                let mut connection =
                    crate::connect::smtp(host, port, tls_config(), starttls).await?;
                let alive = connection.test_connected().await;
                tokio::spawn(async move {
                    let _ = connection.quit().await;
                });
                if alive {
                    Ok(())
                } else {
                    Err("SMTP server did not accept the connection".into())
                }
            },
            cancellation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_metadata_never_reaches_a_connection_probe() {
        // Sync, as ProbeTransport is: the checks run concurrently.
        struct RefuseProbe(std::sync::atomic::AtomicUsize);
        impl ProbeTransport for RefuseProbe {
            fn imap_login(&self, _: &AccountConfig) -> Result<(), String> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err("pending metadata must not open IMAP".into())
            }
            fn smtp_handshake(&self, _: &AccountConfig) -> Result<(), String> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err("pending metadata must not open SMTP".into())
            }
        }
        let config = AccountConfig {
            import_pending: true,
            ..good_cfg()
        };
        let probe = RefuseProbe(std::sync::atomic::AtomicUsize::new(0));
        assert!(matches!(
            validate_account(&config, &probe),
            ValidationState::Invalid(_)
        ));
        assert_eq!(
            probe.0.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "pending metadata must not open IMAP or SMTP"
        );
    }

    fn good_cfg() -> AccountConfig {
        AccountConfig {
            email: "me@host.example".into(),
            username: "me@host.example".into(),
            imap_host: "imap.host.example".into(),
            smtp_host: "smtp.host.example".into(),
            ..Default::default()
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn setup_probe_persists_only_when_both_servers_succeed() {
        // Phase `both_succeed_yields_ok_and_persists`.
        {
            let st = validate_account(
                &good_cfg(),
                &MockProbe {
                    imap: Ok(()),
                    smtp: Ok(()),
                },
            );
            assert_eq!(st, ValidationState::Ok);
            assert!(
                st.persisted(),
                "a fully-successful check persists the account"
            );
        }

        // Phase `imap_failure_does_not_persist_and_reports_imap`.
        {
            let st = validate_account(
                &good_cfg(),
                &MockProbe {
                    imap: Err("auth rejected (535)".into()),
                    smtp: Ok(()),
                },
            );
            assert!(
                !st.persisted(),
                "an IMAP failure must not persist the account"
            );
            assert_eq!(
                st,
                ValidationState::Failed(Failure {
                    trouble: Trouble::Password,
                    message: "That password didn’t work.".into(),
                }),
                "a refused login is about the password, said plainly"
            );
        }

        // Phase `smtp_failure_does_not_persist_and_reports_smtp`.
        {
            let st = validate_account(
                &good_cfg(),
                &MockProbe {
                    imap: Ok(()),
                    smtp: Err("connection refused".into()),
                },
            );
            assert!(!st.persisted());
            assert!(
                matches!(&st, ValidationState::Failed(failure) if failure.trouble == Trouble::Outgoing),
                "{st:?}"
            );
            assert!(
                st.message().contains("smtp.host.example"),
                "{}",
                st.message()
            );
        }

        // Phase `invalid_config_short_circuits_before_network`.
        {
            // A blank IMAP host is a config error; the probe must never be reached.
            struct PanicProbe;
            // Panicking IS the assertion here: the test proves the probe is never
            // reached, so returning Err instead would let a regression that does
            // call it pass silently.
            #[allow(
                clippy::panic_in_result_fn,
                reason = "the panic is the assertion — an Err return would let a regression that reaches the probe pass silently"
            )]
            impl ProbeTransport for PanicProbe {
                fn imap_login(&self, _cfg: &AccountConfig) -> Result<(), String> {
                    panic!("probe reached despite invalid config");
                }
                fn smtp_handshake(&self, _cfg: &AccountConfig) -> Result<(), String> {
                    panic!("probe reached despite invalid config");
                }
            }
            let mut cfg = good_cfg();
            cfg.imap_host.clear();
            let st = validate_account(&cfg, &PanicProbe);
            assert!(matches!(st, ValidationState::Invalid(_)));
            assert!(!st.persisted());
        }

        // Phase `offline_probe_reports_success`.
        {
            let st = validate_account(&good_cfg(), &OfflineProbe);
            assert_eq!(
                st,
                ValidationState::Ok,
                "offline probe lets an account save"
            );
        }

        // Phase `cancelling_setup_closes_a_stalled_tls_connection`.
        #[cfg(feature = "live")]
        {
            use std::io::Read;
            use std::sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            };
            use ui::time::Duration;
            /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
            const HANG_BOUND: Duration = Duration::from_secs(30);

            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let mut cfg = good_cfg();
            cfg.imap_host = "127.0.0.1".into();
            cfg.imap_port = listener.local_addr().unwrap().port();
            cfg.password = "local fixture secret".into();
            let cancellation = Arc::new(AtomicBool::new(false));
            let worker_cancellation = Arc::clone(&cancellation);
            let (sender, receiver) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                sender
                    .send(validate_account_cancellable(&cfg, &worker_cancellation))
                    .unwrap();
            });
            let deadline = tilo_test_env::hang_deadline();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(ui::time::now() < deadline, "verification never connected");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("local listener: {error}"),
                }
            };
            // Windows accepted sockets inherit the listener's nonblocking mode.
            // This fixture uses bounded blocking reads to observe TLS and EOF.
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(HANG_BOUND)).unwrap();
            let mut hello = [0; 4096];
            assert!(
                stream.read(&mut hello).unwrap() > 0,
                "real TLS handshake began"
            );
            cancellation.store(true, Ordering::Relaxed);
            let outcome = receiver.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            assert!(!outcome.persisted());
            assert!(outcome.message().contains("cancelled"), "{outcome:?}");
            let mut remaining = Vec::new();
            stream
                .read_to_end(&mut remaining)
                .expect("cancellation must close the TLS socket");
            worker.join().unwrap();
        }
    }

    #[test]
    fn imap_and_smtp_are_checked_at_the_same_time() {
        use std::sync::Mutex;
        use std::sync::mpsc::{Receiver, Sender, channel};
        use std::time::Duration;
        /// Each side announces itself and waits for the other: only a check
        /// that runs both together gets past it.
        struct Rendezvous {
            imap: (Mutex<Sender<()>>, Mutex<Receiver<()>>),
            smtp: (Mutex<Sender<()>>, Mutex<Receiver<()>>),
        }
        impl ProbeTransport for Rendezvous {
            fn imap_login(&self, _cfg: &AccountConfig) -> Result<(), String> {
                self.imap.0.lock().unwrap().send(()).unwrap();
                self.imap
                    .1
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| "smtp never started".to_owned())
            }
            fn smtp_handshake(&self, _cfg: &AccountConfig) -> Result<(), String> {
                self.smtp.0.lock().unwrap().send(()).unwrap();
                self.smtp
                    .1
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| "imap never started".to_owned())
            }
        }
        let (to_smtp, from_imap) = channel();
        let (to_imap, from_smtp) = channel();
        let probe = Rendezvous {
            imap: (Mutex::new(to_smtp), Mutex::new(from_smtp)),
            smtp: (Mutex::new(to_imap), Mutex::new(from_imap)),
        };
        assert_eq!(validate_account(&good_cfg(), &probe), ValidationState::Ok);
    }

    /// A programmable probe for the state-machine tests.
    struct MockProbe {
        imap: Result<(), String>,
        smtp: Result<(), String>,
    }
    impl ProbeTransport for MockProbe {
        fn imap_login(&self, _cfg: &AccountConfig) -> Result<(), String> {
            self.imap.clone()
        }
        fn smtp_handshake(&self, _cfg: &AccountConfig) -> Result<(), String> {
            self.smtp.clone()
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn classify_maps_transport_errors_with_auth_first() {
        // Phase `classify_maps_real_transport_errors_to_specific_reasons`.
        {
            // Auth: the exact IMAP tagged-NO an server sends on bad credentials.
            assert!(
                classify_conn_failure("[AUTHENTICATIONFAILED] Invalid credentials")
                    .contains("authentication failed")
            );
            assert!(
                classify_conn_failure("535 5.7.8 Username and Password not accepted")
                    .contains("authentication failed")
            );
            // Connection refused (nothing listening on the port).
            assert!(
                classify_conn_failure("Connection refused (os error 111)")
                    .contains("connection refused")
            );
            // DNS failure.
            assert!(
                classify_conn_failure(
                    "failed to lookup address information: Name or service not known"
                )
                .contains("host not found")
            );
            // TLS/certificate.
            assert!(
                classify_conn_failure("invalid peer certificate: UnknownIssuer")
                    .contains("TLS/certificate")
            );
            // Timeout.
            assert!(classify_conn_failure("connection timed out").contains("timed out"));
            // Unknown → the underlying detail is preserved, not swallowed.
            let unknown = classify_conn_failure("some novel failure mode");
            assert!(unknown.contains("some novel failure mode"), "{unknown}");
        }

        // Phase `classify_prioritises_auth_over_generic_transport_words`.
        {
            // A message that mentions both a socket concept and auth must classify
            // as auth (the more actionable, specific cause).
            let msg = "AUTHENTICATE failed: login rejected";
            assert!(
                classify_conn_failure(msg).contains("authentication failed"),
                "{}",
                classify_conn_failure(msg)
            );
        }
    }
}

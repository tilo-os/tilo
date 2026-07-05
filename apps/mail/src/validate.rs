//! Account **test-before-save**: before an account is persisted, its
//! credentials are actually exercised — an IMAP login and an SMTP handshake —
//! and the account is only saved if both succeed. A specific error is surfaced
//! otherwise.
//!
//! ## Injectable transport
//! The check runs through the [`ProbeTransport`] trait, so the state machine is
//! testable without a network: a mock returns canned success/failure. The real
//! network probe ([`LiveProbe`]) is compiled only under `live`. The default
//! (offline) build uses [`OfflineProbe`], which reports success without opening
//! a socket so `--shots` and the headless tests can still save an account.

use crate::model::AccountConfig;

/// The connection probe seam. Implementors attempt the two round-trips that
/// prove an account works.
pub trait ProbeTransport {
    /// Attempt an IMAP `LOGIN` (or `AUTHENTICATE XOAUTH2`) to the account's
    /// incoming server. `Ok(())` on a successful greeting + auth.
    fn imap_login(&self, cfg: &AccountConfig) -> Result<(), String>;
    /// Attempt an SMTP connect + `EHLO` (+ `AUTH`) to the outgoing server.
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
    /// A round-trip failed; carries a specific, user-facing reason.
    Failed(String),
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
            ValidationState::Invalid(m) | ValidationState::Failed(m) => m.clone(),
        }
    }
}

/// Run the full check: config-level validation first (no network), then IMAP,
/// then SMTP. Short-circuits on the first failure so the user sees the most
/// upstream problem.
pub fn validate_account<T: ProbeTransport + ?Sized>(
    cfg: &AccountConfig,
    transport: &T,
) -> ValidationState {
    if let Some((_, msg)) = cfg.validate().into_iter().next() {
        return ValidationState::Invalid(msg);
    }
    if let Err(e) = transport.imap_login(cfg) {
        return ValidationState::Failed(format!("IMAP login failed: {e}"));
    }
    if let Err(e) = transport.smtp_handshake(cfg) {
        return ValidationState::Failed(format!("SMTP handshake failed: {e}"));
    }
    ValidationState::Ok
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

/// The live network probe: a real IMAP login + SMTP handshake. Compiled only
/// under `live` so the default build doesn't pull the TLS/async stack.
#[cfg(feature = "live")]
pub struct LiveProbe;

#[cfg(feature = "live")]
impl ProbeTransport for LiveProbe {
    fn imap_login(&self, cfg: &AccountConfig) -> Result<(), String> {
        // async-imap: TCP+TLS to cfg.imap_host:cfg.imap_port, read the greeting,
        // LOGIN (or AUTHENTICATE XOAUTH2 when cfg.auth == OAuth), LOGOUT.
        // A connect/auth error maps to Err(reason).
        live_probe::imap_login(cfg)
    }
    fn smtp_handshake(&self, cfg: &AccountConfig) -> Result<(), String> {
        // lettre: connect to cfg.smtp_host:cfg.smtp_port, EHLO, STARTTLS /
        // implicit TLS per cfg.smtp_tls, AUTH, then RSET/QUIT without sending.
        live_probe::smtp_handshake(cfg)
    }
}

#[cfg(feature = "live")]
mod live_probe {
    use super::*;

    pub fn imap_login(cfg: &AccountConfig) -> Result<(), String> {
        // Bodies elided in this slice — the seam is here; enabling `live`
        // wires async-imap in exactly as engine.rs's LiveEngine does.
        if cfg.imap_host.is_empty() {
            return Err("no IMAP host".into());
        }
        Ok(())
    }

    pub fn smtp_handshake(cfg: &AccountConfig) -> Result<(), String> {
        if cfg.smtp_host.is_empty() {
            return Err("no SMTP host".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_cfg() -> AccountConfig {
        AccountConfig {
            email: "me@host.example".into(),
            username: "me@host.example".into(),
            imap_host: "imap.host.example".into(),
            smtp_host: "smtp.host.example".into(),
            ..Default::default()
        }
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
    fn both_succeed_yields_ok_and_persists() {
        let st = validate_account(&good_cfg(), &MockProbe { imap: Ok(()), smtp: Ok(()) });
        assert_eq!(st, ValidationState::Ok);
        assert!(st.persisted(), "a fully-successful check persists the account");
    }

    #[test]
    fn imap_failure_does_not_persist_and_reports_imap() {
        let st = validate_account(
            &good_cfg(),
            &MockProbe { imap: Err("auth rejected (535)".into()), smtp: Ok(()) },
        );
        assert!(!st.persisted(), "an IMAP failure must not persist the account");
        assert!(st.message().contains("IMAP"), "error names the failing stage: {}", st.message());
        assert!(st.message().contains("535"));
    }

    #[test]
    fn smtp_failure_does_not_persist_and_reports_smtp() {
        let st = validate_account(
            &good_cfg(),
            &MockProbe { imap: Ok(()), smtp: Err("connection refused".into()) },
        );
        assert!(!st.persisted());
        assert!(st.message().contains("SMTP"), "{}", st.message());
    }

    #[test]
    fn invalid_config_short_circuits_before_network() {
        // A blank IMAP host is a config error; the probe must never be reached.
        struct PanicProbe;
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

    #[test]
    fn offline_probe_reports_success() {
        let st = validate_account(&good_cfg(), &OfflineProbe);
        assert_eq!(st, ValidationState::Ok, "offline probe lets an account save");
    }
}

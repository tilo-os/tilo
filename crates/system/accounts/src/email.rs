//! The email account type's configuration: the IMAP/SMTP settings stored as
//! an email [`crate::Account`]'s `config`, shared by Mail (which connects with
//! it) and Settings (which creates and edits it).

use serde::{Deserialize, Serialize};

/// How an account authenticates to its IMAP/SMTP servers. Password is the
/// classic user+secret; OAuth is the modern token flow the big providers now
/// **require** (Gmail and Outlook block plain-password IMAP), so those
/// providers preset to [`AuthMethod::OAuth`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AuthMethod {
    /// Username + password (the field the setup form collects).
    #[default]
    Password,
    /// `OAuth2` authorization-code + PKCE; the account holds a bearer token
    /// instead of a password (Mail's `oauth` module runs the flow).
    OAuth,
}

impl AuthMethod {
    pub fn as_key(self) -> &'static str {
        match self {
            AuthMethod::Password => "password",
            AuthMethod::OAuth => "oauth",
        }
    }
    pub fn from_key(s: &str) -> Self {
        match s {
            "oauth" => AuthMethod::OAuth,
            _ => AuthMethod::Password,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            AuthMethod::Password => "Password",
            AuthMethod::OAuth => "OAuth (sign in)",
        }
    }
}

/// Account-level connection settings. This is exactly the form the
/// account-setup screen collects and the transport layer consumes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountConfig {
    pub display_name: String,
    pub email: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: bool,
    pub username: String,
    /// How this account signs in. Providers that block password IMAP
    /// (Gmail/Outlook) preset to [`AuthMethod::OAuth`].
    #[serde(default)]
    pub auth: AuthMethod,
    /// Imported metadata must not reuse any existing secret or open a connection
    /// until the person explicitly finishes this account's secure sign-in.
    #[serde(default)]
    pub import_pending: bool,
    /// Held only in memory: the secret lives in the [`crate::SecretVault`] and
    /// is injected at connect time, never written to the account store —
    /// hence `serde(skip)`.
    #[serde(skip)]
    pub password: String,
}

impl std::fmt::Debug for AccountConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountConfig").finish_non_exhaustive()
    }
}

impl Default for AccountConfig {
    fn default() -> Self {
        Self {
            display_name: String::new(),
            email: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            imap_tls: true,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_tls: true,
            username: String::new(),
            auth: AuthMethod::Password,
            import_pending: false,
            password: String::new(),
        }
    }
}

impl AccountConfig {
    /// Decode non-secret connection metadata using the shared account schema.
    ///
    /// Keeping this concrete entry point beside the schema lets thin apps use
    /// the shared runtime's decoder rather than each instantiating one.
    pub fn from_value(value: serde_json::Value) -> serde_json::Result<Self> {
        serde_json::from_value(value)
    }

    /// Encode connection metadata. The in-memory password is never included.
    ///
    /// # Errors
    /// Returns an error if the connection metadata cannot be serialized.
    pub fn to_value(&self) -> serde_json::Result<serde_json::Value> {
        serde_json::to_value(self)
    }

    /// What the account is called: its display name, falling back to the
    /// email, falling back to a generic placeholder.
    pub fn label(&self) -> &str {
        if !self.display_name.is_empty() {
            &self.display_name
        } else if !self.email.is_empty() {
            &self.email
        } else {
            "New account"
        }
    }

    /// Heuristic autofill: given an email, guess the provider's
    /// IMAP/SMTP hosts. Mirrors what Thunderbird's autoconfig does for the
    /// big providers. Returns true if it recognised the domain.
    pub fn autofill_from_email(&mut self) -> bool {
        let Some((local, domain)) = self.email.split_once('@') else {
            return false;
        };
        if self.username.is_empty() {
            self.username = self.email.clone();
        }
        if self.display_name.is_empty() {
            self.display_name = local.to_string();
        }
        let (imap, smtp) = match domain.to_ascii_lowercase().as_str() {
            "gmail.com" | "googlemail.com" => ("imap.gmail.com", "smtp.gmail.com"),
            "outlook.com" | "hotmail.com" | "live.com" => {
                ("outlook.office365.com", "smtp.office365.com")
            }
            "yahoo.com" => ("imap.mail.yahoo.com", "smtp.mail.yahoo.com"),
            "icloud.com" | "me.com" => ("imap.mail.me.com", "smtp.mail.me.com"),
            "proton.me" | "protonmail.com" => ("127.0.0.1", "127.0.0.1"), // bridge
            other => {
                // Generic guess: imap./smtp. + domain.
                self.imap_host = format!("imap.{other}");
                self.smtp_host = format!("smtp.{other}");
                self.imap_port = 993;
                self.imap_tls = true;
                self.smtp_port = 465;
                self.smtp_tls = true;
                return false;
            }
        };
        self.imap_host = imap.to_string();
        self.smtp_host = smtp.to_string();
        self.imap_port = 993;
        self.imap_tls = true;
        // Office365 / Gmail prefer 587 STARTTLS; both also accept 465.
        self.smtp_port = 587;
        self.smtp_tls = true;
        true
    }

    /// Field-level validation for the setup form. Returns a list of
    /// (field-key, message) problems; empty means valid.
    pub fn validate(&self) -> Vec<(&'static str, String)> {
        let mut errs = Vec::new();
        if !self.email.contains('@') {
            errs.push(("email", "Enter a valid email address".into()));
        }
        if self.imap_host.trim().is_empty() {
            errs.push(("imap_host", "IMAP host is required".into()));
        }
        if self.smtp_host.trim().is_empty() {
            errs.push(("smtp_host", "SMTP host is required".into()));
        }
        if self.username.trim().is_empty() {
            errs.push(("username", "Username is required".into()));
        }
        if self.imap_port == 0 {
            errs.push(("imap_port", "Port must be 1-65535".into()));
        }
        if self.smtp_port == 0 {
            errs.push(("smtp_port", "Port must be 1-65535".into()));
        }
        errs
    }

    pub fn is_valid(&self) -> bool {
        self.validate().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_metadata_roundtrip_excludes_passwords() {
        let config = AccountConfig {
            email: "person@example.test".into(),
            auth: AuthMethod::OAuth,
            import_pending: true,
            password: "in-memory-only".into(),
            ..Default::default()
        };
        let mut value = config.to_value().expect("encode connection metadata");
        assert!(value.get("password").is_none());
        assert!(!value.to_string().contains("in-memory-only"));
        // Even an untrusted metadata document cannot supply a credential.
        value["password"] = serde_json::json!("untrusted-password");
        let decoded = AccountConfig::from_value(value).expect("decode connection metadata");
        assert_eq!(
            decoded,
            AccountConfig {
                password: String::new(),
                ..config
            }
        );
        assert!(AccountConfig::from_value(serde_json::json!({"email": 42})).is_err());
    }

    #[test]
    fn setup_model_autofills_and_validates() {
        {
            let mut cfg = AccountConfig {
                email: "person@gmail.com".into(),
                ..Default::default()
            };
            assert!(cfg.autofill_from_email());
            assert_eq!(cfg.imap_host, "imap.gmail.com");
            assert_eq!(cfg.smtp_host, "smtp.gmail.com");
            assert!(cfg.is_valid());
        }

        {
            let mut cfg = AccountConfig {
                email: "person@acme.test".into(),
                ..Default::default()
            };
            assert!(!cfg.autofill_from_email());
            assert_eq!(cfg.imap_host, "imap.acme.test");
            assert_eq!(cfg.smtp_host, "smtp.acme.test");
        }

        {
            let cfg = AccountConfig::default();
            assert!(!cfg.is_valid());
            let keys: Vec<_> = cfg.validate().into_iter().map(|(k, _)| k).collect();
            assert!(keys.contains(&"email"));
            assert!(keys.contains(&"imap_host"));
        }
    }
}

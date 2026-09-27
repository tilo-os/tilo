//! Forge accounts: a Forgejo repository and the token that files issues in
//! it, held in custody like every other online account.
//!
//! The account is an ordinary [`accounts::Account`] of type [`FORGEJO_TYPE`]
//! whose `config` is a [`ForgeConfig`] (the server and the repository, no
//! secret) and whose token is sealed in the account vault. Nothing outside
//! this crate reads the token: a consumer hands over the issue it wants
//! filed ([`ForgeIssue`]) and gets back where it was filed ([`FiledIssue`]).
//! The feedback triage agent files a drafted issue this way, and only after
//! an Editor approved it.

use std::io::Read as _;
use std::time::Duration;

use accounts::AccountId;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::custodian::{Custodian, CustodyError};

/// The account type of a Forgejo repository.
pub const FORGEJO_TYPE: &str = "forgejo";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;
const USER_AGENT: &str = concat!("Tilo/", env!("CARGO_PKG_VERSION"));

/// A forge account's connection settings. No secret: the token is in the
/// vault.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeConfig {
    /// The server's origin, `https://codeberg.org`.
    pub server: String,
    /// The repository issues are filed in, `owner/name`.
    pub repository: String,
}

/// An issue to file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgeIssue {
    pub title: String,
    pub body: String,
}

/// Where an issue was filed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiledIssue {
    pub number: u64,
    /// The issue's page on the forge.
    pub url: String,
}

impl ForgeConfig {
    /// The endpoint issues are created at. HTTPS, or plain HTTP to this
    /// machine only; no credentials in the URL.
    fn issues_endpoint(&self) -> Result<Url, CustodyError> {
        let invalid = |why: &str| CustodyError::Account(format!("forge account: {why}"));
        let server = Url::parse(self.server.trim()).map_err(|_| invalid("not a server URL"))?;
        let local = matches!(
            server.host(),
            Some(url::Host::Ipv4(ip)) if ip.is_loopback()
        ) || server.host_str() == Some("localhost");
        let secure = match server.scheme() {
            "https" => server.host().is_some(),
            "http" => local,
            _ => false,
        };
        if !secure {
            return Err(invalid("use an HTTPS server URL"));
        }
        if !server.username().is_empty() || server.password().is_some() {
            return Err(invalid("remove credentials from the server URL"));
        }
        let (owner, name) = self
            .repository
            .split_once('/')
            .filter(|(owner, name)| valid_segment(owner) && valid_segment(name))
            .ok_or_else(|| invalid("the repository is not owner/name"))?;
        let base = server.as_str().trim_end_matches('/');
        Url::parse(&format!("{base}/api/v1/repos/{owner}/{name}/issues"))
            .map_err(|_| invalid("not a server URL"))
    }
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 100
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && segment != "."
        && segment != ".."
}

#[derive(Deserialize)]
struct CreatedIssue {
    number: u64,
    html_url: String,
}

impl Custodian {
    /// File `issue` in the repository of forge account `id`, with the token
    /// sealed for it. The outcome is the account's health, as for any other
    /// account.
    ///
    /// # Errors
    /// [`CustodyError::Account`] when the account is missing, not a forge
    /// account or misconfigured; [`CustodyError::NeedsAuthentication`] when
    /// it has no token or the forge refuses it; [`CustodyError::Offline`]
    /// when the forge cannot be reached or answers with anything else.
    pub fn file_forge_issue(
        &self,
        id: &AccountId,
        issue: &ForgeIssue,
    ) -> Result<FiledIssue, CustodyError> {
        let result = self.file(id, issue);
        self.report_health(id, &result);
        result
    }

    fn file(&self, id: &AccountId, issue: &ForgeIssue) -> Result<FiledIssue, CustodyError> {
        let account = self
            .store()
            .get(id)
            .cloned()
            .ok_or_else(|| CustodyError::Account("account no longer exists".into()))?;
        if account.type_id.as_str() != FORGEJO_TYPE {
            return Err(CustodyError::Account("this is not a forge account".into()));
        }
        let config: ForgeConfig = account
            .config_as()
            .ok_or_else(|| CustodyError::Account("forge account: no server".into()))?;
        let endpoint = config.issues_endpoint()?;
        let vault = self.vault();
        if let Some(error) = vault.recovery_error() {
            return Err(CustodyError::Storage(error.to_string()));
        }
        let token = vault
            .get(id)
            .and_then(|secret| secret.token.filter(|token| !token.trim().is_empty()))
            .map(zeroize::Zeroizing::new)
            .ok_or(CustodyError::NeedsAuthentication)?;
        let http = tilo_net::http::blocking_client()
            .user_agent(USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CustodyError::Offline)?;
        let response = http
            .post(endpoint)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("token {}", token.as_str()),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::json!({ "title": issue.title, "body": issue.body }).to_string())
            .send()
            .map_err(|_| CustodyError::Offline)?;
        drop(token);
        match response.status().as_u16() {
            201 => {}
            401 | 403 => return Err(CustodyError::NeedsAuthentication),
            404 => {
                return Err(CustodyError::Account(
                    "the forge does not know that repository".into(),
                ));
            }
            _ => return Err(CustodyError::Offline),
        }
        let mut body = String::new();
        response
            .take(MAX_RESPONSE_BYTES)
            .read_to_string(&mut body)
            .map_err(|_| CustodyError::Offline)?;
        let created: CreatedIssue =
            serde_json::from_str(&body).map_err(|_| CustodyError::Offline)?;
        Ok(FiledIssue {
            number: created.number,
            url: created.html_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accounts::{Account, AccountStore, Secret, SecretVault};

    fn forge_account(root: &std::path::Path, server: &str, token: Option<&str>) -> AccountId {
        let id = AccountId::new("forge-tilo");
        let mut account = Account::new(id.clone(), FORGEJO_TYPE);
        account.set_config(&ForgeConfig {
            server: server.into(),
            repository: "tilo/tilo".into(),
        });
        let mut store = AccountStore::load_in(root);
        store.upsert(account);
        store.save_in(root).expect("save the account");
        if let Some(token) = token {
            let mut vault = SecretVault::open_in(root);
            vault.put(
                &id,
                &Secret {
                    password: String::new(),
                    token: Some(token.into()),
                },
            );
            vault.try_save().expect("seal the token");
        }
        id
    }

    #[test]
    fn an_issue_is_filed_with_the_sealed_token_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tilo");
        let forge = test_http::Responder::start(vec![
            test_http::Reply::status(201)
                .header("Content-Type", "application/json")
                .body(r#"{"number":42,"html_url":"https://forge.example/tilo/tilo/issues/42"}"#),
        ]);
        let id = forge_account(&root, &forge.url(), Some("sealed-token"));
        let custodian = Custodian::for_root(Some(root));
        let filed = custodian
            .file_forge_issue(
                &id,
                &ForgeIssue {
                    title: "Send does nothing".into(),
                    body: "Report: tilo://chat/m_1".into(),
                },
            )
            .expect("filed");
        assert_eq!(filed.number, 42);
        let request = forge.next_request();
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/api/v1/repos/tilo/tilo/issues");
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("authorization")
                    && value == "token sealed-token")
        );
        let sent: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(sent["title"], "Send does nothing");
    }

    #[test]
    fn an_account_without_a_token_or_of_another_kind_files_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tilo");
        let forge = test_http::Responder::silent();
        let id = forge_account(&root, &forge.url(), None);
        let custodian = Custodian::for_root(Some(root));
        let issue = ForgeIssue {
            title: "t".into(),
            body: String::new(),
        };
        assert_eq!(
            custodian.file_forge_issue(&id, &issue),
            Err(CustodyError::NeedsAuthentication)
        );
        assert!(matches!(
            custodian.file_forge_issue(&AccountId::new("nobody"), &issue),
            Err(CustodyError::Account(_))
        ));
        forge.assert_untouched();
        assert!(
            ForgeConfig {
                server: "http://forge.example".into(),
                repository: "tilo/tilo".into()
            }
            .issues_endpoint()
            .is_err(),
            "a token never crosses the network in the clear"
        );
        assert!(
            ForgeConfig {
                server: "https://forge.example".into(),
                repository: "../admin".into()
            }
            .issues_endpoint()
            .is_err()
        );
    }
}

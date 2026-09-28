//! Forge accounts: a Forgejo repository and the token that files issues in
//! it, held in custody like every other online account.
//!
//! The account is an ordinary [`accounts::Account`] of type [`FORGEJO_TYPE`]
//! whose `config` is a [`ForgeConfig`] (the server and the repository, no
//! secret) and whose token is sealed in the account vault. Nothing outside
//! this crate reads the token: a consumer hands over the issue it wants
//! filed ([`ForgeIssue`]) and gets back where it was filed ([`FiledIssue`]),
//! through the accounts service when it is a sandboxed app
//! ([`crate::service`]). The feedback triage agent files a drafted issue
//! this way, and only after an Editor approved it.
//!
//! Filing is idempotent by the issue's marker: the filed issue carries
//! [`tilo_account_service::marker_line`], and before filing, the repository
//! is searched for an issue that carries it. A caller that filed, lost the
//! answer and asks again gets the issue it already filed.

use std::io::Read as _;
use std::time::Duration;

use accounts::AccountId;
use serde::{Deserialize, Serialize};
use url::Url;

pub use tilo_account_service::{FiledIssue, ForgeIssue};

use crate::custodian::{Custodian, CustodyError};

/// The account type of a Forgejo repository.
pub const FORGEJO_TYPE: &str = "forgejo";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
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

impl ForgeConfig {
    /// The repository's issues endpoint. HTTPS, or plain HTTP to this
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

    /// Whether `server` and `repository` name a forge repository the
    /// custodian will file in. Settings checks this before saving.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.issues_endpoint().is_ok()
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
struct ForgeIssueRow {
    number: u64,
    html_url: String,
    #[serde(default)]
    body: String,
}

/// A forge repository and the token for it, for one call.
struct Repository {
    endpoint: Url,
    token: zeroize::Zeroizing<String>,
    http: reqwest::blocking::Client,
}

impl Repository {
    fn authorization(&self) -> String {
        format!("token {}", self.token.as_str())
    }

    fn read(response: reqwest::blocking::Response) -> Result<String, CustodyError> {
        match response.status().as_u16() {
            200 | 201 => {}
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
        Ok(body)
    }

    /// The issue that carries `marker`'s line, open or closed.
    fn find(&self, marker: &str) -> Result<Option<FiledIssue>, CustodyError> {
        let line = tilo_account_service::marker_line(marker);
        let mut url = self.endpoint.clone();
        url.query_pairs_mut()
            .append_pair("state", "all")
            .append_pair("type", "issues")
            .append_pair("q", &line);
        let response = self
            .http
            .get(url)
            .header(reqwest::header::AUTHORIZATION, self.authorization())
            .send()
            .map_err(|_| CustodyError::Offline)?;
        let rows: Vec<ForgeIssueRow> =
            serde_json::from_str(&Self::read(response)?).map_err(|_| CustodyError::Offline)?;
        // The forge's search is a hint; the marker line is the proof.
        Ok(rows
            .into_iter()
            .find(|row| row.body.contains(&line))
            .map(|row| FiledIssue {
                number: row.number,
                url: row.html_url,
            }))
    }

    fn create(&self, issue: &ForgeIssue) -> Result<FiledIssue, CustodyError> {
        let response = self
            .http
            .post(self.endpoint.clone())
            .header(reqwest::header::AUTHORIZATION, self.authorization())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(
                serde_json::json!({ "title": issue.title, "body": issue.filed_body() }).to_string(),
            )
            .send()
            .map_err(|_| CustodyError::Offline)?;
        let row: ForgeIssueRow =
            serde_json::from_str(&Self::read(response)?).map_err(|_| CustodyError::Offline)?;
        Ok(FiledIssue {
            number: row.number,
            url: row.html_url,
        })
    }
}

/// Check that `token` may file issues in `config`'s repository before an
/// account is saved: the repository answers with the token and says it
/// takes issues. Settings runs this on a draft; nothing is stored.
///
/// # Errors
/// [`CustodyError::Account`] for a malformed server or repository, a token
/// the forge refuses or a repository without issues;
/// [`CustodyError::Offline`] otherwise.
pub fn verify_forge(config: &ForgeConfig, token: &str) -> Result<(), CustodyError> {
    let endpoint = config.issues_endpoint()?;
    let repository_url = Url::parse(endpoint.as_str().trim_end_matches("/issues"))
        .map_err(|_| CustodyError::Account("forge account: not a server URL".into()))?;
    let repository = Repository {
        endpoint,
        token: zeroize::Zeroizing::new(token.trim().to_string()),
        http: tilo_net::http::blocking_client()
            .user_agent(USER_AGENT)
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CustodyError::Offline)?,
    };
    let response = repository
        .http
        .get(repository_url)
        .header(reqwest::header::AUTHORIZATION, repository.authorization())
        .send()
        .map_err(|_| CustodyError::Offline)?;
    let body = Repository::read(response).map_err(|error| match error {
        CustodyError::NeedsAuthentication => {
            CustodyError::Account("the forge refused this token".into())
        }
        other => other,
    })?;
    let body: serde_json::Value = serde_json::from_str(&body).map_err(|_| CustodyError::Offline)?;
    if body.get("has_issues").and_then(serde_json::Value::as_bool) == Some(false) {
        return Err(CustodyError::Account(
            "that repository does not take issues".into(),
        ));
    }
    Ok(())
}

impl Custodian {
    /// File `issue` in the repository of forge account `id`, with the token
    /// sealed for it, unless an issue carrying its marker is already there;
    /// that issue is then the answer. The outcome is the account's health,
    /// as for any other account.
    ///
    /// # Errors
    /// [`CustodyError::Account`] when the account is missing, not a forge
    /// account, misconfigured or the issue is malformed;
    /// [`CustodyError::NeedsAuthentication`] when it has no token or the
    /// forge refuses it; [`CustodyError::Offline`] when the forge cannot be
    /// reached or answers with anything else.
    pub fn file_forge_issue(
        &self,
        id: &AccountId,
        issue: &ForgeIssue,
    ) -> Result<FiledIssue, CustodyError> {
        let result = self.repository(id).and_then(|repository| {
            if !issue.is_valid() {
                return Err(CustodyError::Account("the issue is malformed".into()));
            }
            match repository.find(&issue.marker)? {
                Some(filed) => Ok(filed),
                None => repository.create(issue),
            }
        });
        self.report_health(id, &result);
        result
    }

    /// The forge account `named`, or, when the caller names none, the one
    /// forge account this person connected: the team connects one in
    /// Settings and the triage agent files with it.
    ///
    /// # Errors
    /// [`CustodyError::Account`] when none is named and there is not exactly
    /// one forge account.
    pub fn forge_account(&self, named: &str) -> Result<AccountId, CustodyError> {
        if !named.trim().is_empty() {
            return Ok(AccountId::new(named.trim()));
        }
        let store = self.store();
        let forges = store.list(&accounts::AccountTypeId::from(FORGEJO_TYPE));
        match forges.as_slice() {
            [only] => Ok(only.id.clone()),
            [] => Err(CustodyError::Account(
                "no forge account is connected; add one in Settings › Accounts".into(),
            )),
            _ => Err(CustodyError::Account(
                "several forge accounts are connected; name the one to file with".into(),
            )),
        }
    }

    /// The issue filed for `marker` in forge account `id`'s repository.
    ///
    /// # Errors
    /// As [`Self::file_forge_issue`].
    pub fn find_forge_issue(
        &self,
        id: &AccountId,
        marker: &str,
    ) -> Result<Option<FiledIssue>, CustodyError> {
        if !tilo_account_service::valid_marker(marker) {
            return Err(CustodyError::Account("the marker is malformed".into()));
        }
        let result = self
            .repository(id)
            .and_then(|repository| repository.find(marker));
        self.report_health(id, &result);
        result
    }

    fn repository(&self, id: &AccountId) -> Result<Repository, CustodyError> {
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
        Ok(Repository {
            endpoint,
            token,
            http,
        })
    }
}

/// A fake Forgejo repository for tests: it keeps the issues filed in it and
/// answers the issue search and creation the custodian makes.
#[cfg(any(test, feature = "fake-forge"))]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test support: a fixture that cannot keep its issues fails the test using it"
)]
pub mod fake {
    use std::sync::{Arc, Mutex};

    /// One issue in the fake repository.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Filed {
        pub number: u64,
        pub title: String,
        pub body: String,
    }

    /// A running fake forge.
    pub struct FakeForge {
        responder: test_http::Responder,
        issues: Arc<Mutex<Vec<Filed>>>,
        token: String,
    }

    impl FakeForge {
        /// A forge that accepts `token` and nothing else.
        #[must_use]
        pub fn start(token: &str) -> Self {
            let issues: Arc<Mutex<Vec<Filed>>> = Arc::default();
            let held = Arc::clone(&issues);
            let expected = format!("token {token}");
            let responder = test_http::Responder::handle(move |request| {
                let authorized = request.headers.iter().any(|(name, value)| {
                    name.eq_ignore_ascii_case("authorization") && *value == expected
                });
                if !authorized {
                    return Some(test_http::Reply::status(401));
                }
                let (path, query) = request
                    .target
                    .split_once('?')
                    .unwrap_or((request.target.as_str(), ""));
                if path.ends_with("/repos/tilo/tilo") && request.method == "GET" {
                    return Some(
                        test_http::Reply::status(200)
                            .header("Content-Type", "application/json")
                            .body(r#"{"full_name":"tilo/tilo","has_issues":true}"#),
                    );
                }
                if !path.ends_with("/issues") {
                    return Some(test_http::Reply::status(404));
                }
                let mut issues = held
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let row = |issue: &Filed| {
                    serde_json::json!({
                        "number": issue.number,
                        "html_url": format!("https://forge.example/tilo/tilo/issues/{}", issue.number),
                        "title": issue.title,
                        "body": issue.body,
                    })
                };
                let json = |value: serde_json::Value, status: u16| {
                    Some(
                        test_http::Reply::status(status)
                            .header("Content-Type", "application/json")
                            .body(value.to_string()),
                    )
                };
                match request.method.as_str() {
                    "GET" => {
                        let wanted = url::form_urlencoded::parse(query.as_bytes())
                            .find(|(key, _)| key == "q")
                            .map(|(_, value)| value.into_owned())
                            .unwrap_or_default();
                        let rows: Vec<_> = issues
                            .iter()
                            .filter(|issue| wanted.is_empty() || issue.body.contains(&wanted))
                            .map(row)
                            .collect();
                        json(serde_json::Value::Array(rows), 200)
                    }
                    "POST" => {
                        let sent: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
                        let filed = Filed {
                            number: u64::try_from(issues.len()).ok()?.saturating_add(1),
                            title: sent["title"].as_str()?.to_string(),
                            body: sent["body"].as_str()?.to_string(),
                        };
                        let answer = row(&filed);
                        issues.push(filed);
                        json(answer, 201)
                    }
                    _ => Some(test_http::Reply::status(405)),
                }
            });
            Self {
                responder,
                issues,
                token: token.into(),
            }
        }

        /// The server origin to configure a forge account with.
        #[must_use]
        pub fn url(&self) -> String {
            self.responder.url()
        }

        /// The token the forge accepts.
        #[must_use]
        pub fn token(&self) -> &str {
            &self.token
        }

        /// Every issue filed so far.
        #[must_use]
        pub fn issues(&self) -> Vec<Filed> {
            self.issues
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    /// Save a forge account for `forge` under `root`, its token sealed.
    ///
    /// # Panics
    /// When the account store or the vault cannot be written.
    pub fn connect(root: &std::path::Path, id: &str, forge: &FakeForge) -> accounts::AccountId {
        let id = accounts::AccountId::new(id);
        let mut account = accounts::Account::new(id.clone(), super::FORGEJO_TYPE);
        account.set_config(&super::ForgeConfig {
            server: forge.url(),
            repository: "tilo/tilo".into(),
        });
        let mut store = accounts::AccountStore::load_in(root);
        store.upsert(account);
        store.save_in(root).expect("save the forge account");
        let mut vault = accounts::SecretVault::open_in(root);
        vault.put(
            &id,
            &accounts::Secret {
                password: String::new(),
                token: Some(forge.token().into()),
            },
        );
        vault.try_save().expect("seal the forge token");
        id
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeForge, connect};
    use super::*;

    fn issue(marker: &str) -> ForgeIssue {
        ForgeIssue {
            title: "Send does nothing".into(),
            body: "Report: tilo:room/r/chat.message:m_1".into(),
            marker: marker.into(),
        }
    }

    #[test]
    fn an_issue_is_filed_once_per_marker_with_the_sealed_token() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tilo");
        let forge = FakeForge::start("sealed-token");
        let id = connect(&root, "forge-tilo", &forge);
        let custodian = Custodian::for_root(Some(root));
        assert_eq!(
            custodian.forge_account("").unwrap(),
            id,
            "the one forge account"
        );
        assert_eq!(custodian.find_forge_issue(&id, "m_draft").unwrap(), None);
        let filed = custodian.file_forge_issue(&id, &issue("m_draft")).unwrap();
        assert_eq!(filed.number, 1);
        let again = custodian.file_forge_issue(&id, &issue("m_draft")).unwrap();
        assert_eq!(again, filed, "a second filing finds the first");
        assert_eq!(forge.issues().len(), 1);
        assert!(
            forge.issues()[0]
                .body
                .ends_with("<!-- tilo-filed-for: m_draft -->")
        );
        assert_eq!(
            custodian.find_forge_issue(&id, "m_draft").unwrap(),
            Some(filed)
        );
        custodian.file_forge_issue(&id, &issue("m_other")).unwrap();
        assert_eq!(forge.issues().len(), 2, "another marker is another issue");
        let config = ForgeConfig {
            server: forge.url(),
            repository: "tilo/tilo".into(),
        };
        assert_eq!(verify_forge(&config, "sealed-token"), Ok(()));
        assert_eq!(
            verify_forge(&config, "wrong"),
            Err(CustodyError::Account("the forge refused this token".into()))
        );
    }

    #[test]
    fn an_account_without_a_token_or_of_another_kind_files_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tilo");
        let forge = FakeForge::start("right");
        let id = connect(&root, "forge-tilo", &forge);
        let mut vault = accounts::SecretVault::open_in(&root);
        vault.put(
            &id,
            &accounts::Secret {
                password: String::new(),
                token: Some("wrong".into()),
            },
        );
        vault.try_save().unwrap();
        let custodian = Custodian::for_root(Some(root));
        assert_eq!(
            custodian.file_forge_issue(&id, &issue("m_1")),
            Err(CustodyError::NeedsAuthentication)
        );
        assert!(matches!(
            custodian.file_forge_issue(&AccountId::new("nobody"), &issue("m_1")),
            Err(CustodyError::Account(_))
        ));
        assert!(forge.issues().is_empty());
        assert!(
            !ForgeConfig {
                server: "http://forge.example".into(),
                repository: "tilo/tilo".into()
            }
            .is_valid(),
            "a token never crosses the network in the clear"
        );
        assert!(
            !ForgeConfig {
                server: "https://forge.example".into(),
                repository: "../admin".into()
            }
            .is_valid()
        );
    }
}

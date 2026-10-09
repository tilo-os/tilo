//! Native project checks selected by ID. Commands and execution bounds are host-only.
use serde::{Deserialize, Serialize};

pub const MAX_REQUEST_BYTES: usize = 1024;
pub const MAX_RECEIPT_BYTES: usize = 16384;
pub const MAX_PREVIEW_BYTES: usize = 2048;
pub const ACTION_ID: &str = "run-approved-check";

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn invocation(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, b)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
        && value != "00000000-0000-0000-0000-000000000000"
}

/// Inert correlation only; the broker must recover actual target custody.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u8,
    pub invocation: String,
    pub check_id: String,
}
impl Request {
    /// Encode bounded correlation data, never execution authority.
    ///
    /// # Errors
    /// Refuses invalid identities, unknown schema or oversized data.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if self.version != 1 || !invocation(&self.invocation) || !identifier(&self.check_id) {
            return Err("invalid approved-check request".into());
        }
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("approved-check request too large".into());
        }
        Ok(bytes)
    }
    /// Decode the closed request vocabulary.
    ///
    /// # Errors
    /// Refuses malformed, duplicate, unknown or invalid fields and oversized input.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err("approved-check request too large".into());
        }
        let request: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        request.encode()?;
        Ok(request)
    }
}

/// Canonical resource identity resolved by the admitted project host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub app_id: String,
    pub workspace: String,
    pub host: [u8; 32],
    pub record: String,
    pub resource: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Exited,
    Cancelled,
    TimedOut,
    OutputLimit,
    Failed,
}

/// A bounded UTF-8 diagnostic excerpt; its hash identifies the complete captured stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub sha256: Option<String>,
    pub preview: String,
    pub truncated: bool,
}

/// Observed execution metadata. This is not a grader verdict or a model score.
/// The full private receipt remains with the trusted host under its content hash.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub version: u8,
    pub invocation: String,
    pub check_id: String,
    pub binding: Binding,
    pub artifact_sha256: String,
    pub config_sha256: String,
    pub runtime_binary_sha256: String,
    pub supervisor_source_sha256: String,
    pub outcome: Outcome,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub elapsed_ms: u64,
    pub cleanup_observed: bool,
    pub stdout: Diagnostic,
    pub stderr: Diagnostic,
}
impl Receipt {
    fn valid(&self) -> bool {
        self.version == 1
            && invocation(&self.invocation)
            && identifier(&self.check_id)
            && identifier(&self.binding.app_id)
            && invocation(&self.binding.workspace)
            && !self.binding.record.is_empty()
            && self.binding.record.len() <= 1024
            && !self.binding.record.contains('\0')
            && !self.binding.resource.is_empty()
            && self.binding.resource.len() <= 1024
            && !self.binding.resource.contains('\0')
            && self.binding.host != [0; 32]
            && [
                &self.artifact_sha256,
                &self.config_sha256,
                &self.runtime_binary_sha256,
                &self.supervisor_source_sha256,
            ]
            .into_iter()
            .all(|v| digest(v))
            && [&self.stdout, &self.stderr].into_iter().all(|v| {
                v.preview.len() <= MAX_PREVIEW_BYTES
                    && v.sha256.as_ref().is_none_or(|s| digest(s))
                    && (v.sha256.is_some() || v.preview.is_empty() && !v.truncated)
            })
            && (self.exit_code.is_none() || self.signal.is_none())
            && (self.outcome != Outcome::Exited
                || self.cleanup_observed && (self.exit_code.is_some() || self.signal.is_some()))
    }
    /// Encode only bounded observed metadata and diagnostics.
    ///
    /// # Errors
    /// Refuses invalid or oversized receipts.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if !self.valid() {
            return Err("invalid approved-check receipt".into());
        }
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err("approved-check receipt too large".into());
        }
        Ok(bytes)
    }
    /// Decode one bounded receipt without trusting its contents as authority.
    ///
    /// # Errors
    /// Refuses malformed, duplicate, unknown, invalid or oversized data.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err("approved-check receipt too large".into());
        }
        let receipt: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        receipt.encode()?;
        Ok(receipt)
    }
}

#[cfg(feature = "client")]
impl crate::ChannelHandle {
    /// Execute one host-approved ID for this target's actual accepted action.
    /// No fallback exists on brokers without this method.
    ///
    /// # Errors
    /// Refuses invalid requests, unavailable transport, stale custody or invalid replies.
    pub async fn approved_check(&self, request: &Request) -> Result<Receipt, crate::NotAvailable> {
        let encoded = request
            .encode()
            .map_err(|error| crate::NotAvailable::Capnp(capnp::Error::failed(error)))?;
        let mut call = self.inner.approved_check_request();
        call.get().set_request(&encoded);
        let response = call.send().promise.await?;
        let receipt = Receipt::decode(response.get()?.get_receipt()?)
            .map_err(|error| crate::NotAvailable::Capnp(capnp::Error::failed(error)))?;
        if receipt.invocation != request.invocation || receipt.check_id != request.check_id {
            return Err(crate::NotAvailable::Capnp(capnp::Error::failed(
                "approved-check response correlation changed".into(),
            )));
        }
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn receipt() -> Receipt {
        Receipt {
            version: 1,
            invocation: ID.into(),
            check_id: "regression".into(),
            binding: Binding {
                app_id: "so.tilo.git".into(),
                workspace: ID.into(),
                host: [7; 32],
                record: "project".into(),
                resource: "repository".into(),
            },
            artifact_sha256: "a".repeat(64),
            config_sha256: "b".repeat(64),
            runtime_binary_sha256: "c".repeat(64),
            supervisor_source_sha256: "d".repeat(64),
            outcome: Outcome::Exited,
            exit_code: Some(1),
            signal: None,
            elapsed_ms: 34,
            cleanup_observed: true,
            stdout: Diagnostic {
                sha256: Some("e".repeat(64)),
                preview: "failed regression".into(),
                truncated: false,
            },
            stderr: Diagnostic {
                sha256: None,
                preview: String::new(),
                truncated: false,
            },
        }
    }

    #[test]
    fn approved_check_request_refuses_execution_inputs_and_ambiguous_identities() {
        let request = Request {
            version: 1,
            invocation: ID.into(),
            check_id: "regression".into(),
        };
        let bytes = request.encode().unwrap();
        assert_eq!(Request::decode(&bytes).unwrap(), request);
        for extra in [
            "command",
            "argv",
            "environment",
            "checkout",
            "host",
            "certificate",
        ] {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            value[extra] = serde_json::json!("caller-supplied");
            assert!(Request::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        let duplicate = format!(
            r#"{{"version":1,"invocation":"{ID}","check_id":"regression","check_id":"other"}}"#
        );
        assert!(Request::decode(duplicate.as_bytes()).is_err());
        for check_id in ["../regression", "regression;id", "-option", "", "run test"] {
            assert!(
                Request {
                    check_id: check_id.into(),
                    ..request.clone()
                }
                .encode()
                .is_err()
            );
        }
        assert!(Request::decode(&vec![b' '; MAX_REQUEST_BYTES + 1]).is_err());
    }

    #[test]
    fn approved_check_receipt_preserves_failure_and_refuses_unobserved_exit_or_unbounded_diagnostics()
     {
        let actual = receipt();
        assert_eq!(
            Receipt::decode(&actual.encode().unwrap())
                .unwrap()
                .exit_code,
            Some(1)
        );
        let mut changed = actual.clone();
        changed.cleanup_observed = false;
        assert!(changed.encode().is_err());
        changed.outcome = Outcome::Failed;
        assert!(
            changed.encode().is_ok(),
            "failed cleanup remains reportable as failed execution"
        );
        changed = actual.clone();
        changed.exit_code = None;
        assert!(changed.encode().is_err());
        changed = actual.clone();
        changed.signal = Some(9);
        assert!(changed.encode().is_err());
        changed = actual.clone();
        changed.stdout.preview = "x".repeat(MAX_PREVIEW_BYTES + 1);
        assert!(changed.encode().is_err());
        changed = actual.clone();
        changed.stdout.sha256 = None;
        assert!(
            changed.encode().is_err(),
            "captured diagnostic must identify the complete stream"
        );
        changed = actual.clone();
        changed.binding.host = [0; 32];
        assert!(changed.encode().is_err());
        let duplicate = String::from_utf8(actual.encode().unwrap())
            .unwrap()
            .replacen('{', r#"{"version":1,"#, 1);
        assert!(Receipt::decode(duplicate.as_bytes()).is_err());
        assert!(Receipt::decode(&vec![b' '; MAX_RECEIPT_BYTES + 1]).is_err());
    }

    #[cfg(feature = "client")]
    struct WireAbi;

    #[cfg(feature = "client")]
    impl crate::collab_capnp::channel_handle::Server for WireAbi {
        fn local_file_clipboard(
            self: capnp::capability::Rc<Self>,
            _: crate::collab_capnp::channel_handle::LocalFileClipboardParams,
            mut results: crate::collab_capnp::channel_handle::LocalFileClipboardResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            results.get().set_reply_json(b"clipboard56");
            std::future::ready(Ok(()))
        }

        fn audio_output_command(
            self: capnp::capability::Rc<Self>,
            _: crate::collab_capnp::channel_handle::AudioOutputCommandParams,
            mut results: crate::collab_capnp::channel_handle::AudioOutputCommandResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            results.get().set_reply(b"audio57");
            std::future::ready(Ok(()))
        }

        fn approved_check(
            self: capnp::capability::Rc<Self>,
            _: crate::collab_capnp::channel_handle::ApprovedCheckParams,
            mut results: crate::collab_capnp::channel_handle::ApprovedCheckResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            results.get().set_receipt(b"approved58");
            std::future::ready(Ok(()))
        }
    }

    /// Literal wire slots model an independently compiled existing caller,
    /// rather than allowing client and server renumbering to cancel each other.
    #[cfg(feature = "client")]
    #[tokio::test(flavor = "current_thread")]
    async fn approved_check_append_preserves_clipboard_and_audio_wire_slots() {
        use crate::collab_capnp::channel_handle;
        use capnp::capability::Request as RpcRequest;
        use capnp::traits::HasTypeId;

        let client: channel_handle::Client = capnp_rpc::new_client(WireAbi);
        let clipboard: RpcRequest<
            channel_handle::local_file_clipboard_params::Owned,
            channel_handle::local_file_clipboard_results::Owned,
        > = client
            .client
            .new_call(channel_handle::Client::TYPE_ID, 56, None);
        assert_eq!(
            clipboard
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_reply_json()
                .unwrap(),
            b"clipboard56"
        );
        let audio: RpcRequest<
            channel_handle::audio_output_command_params::Owned,
            channel_handle::audio_output_command_results::Owned,
        > = client
            .client
            .new_call(channel_handle::Client::TYPE_ID, 57, None);
        assert_eq!(
            audio
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_reply()
                .unwrap(),
            b"audio57"
        );
        let approved: RpcRequest<
            channel_handle::approved_check_params::Owned,
            channel_handle::approved_check_results::Owned,
        > = client
            .client
            .new_call(channel_handle::Client::TYPE_ID, 58, None);
        assert_eq!(
            approved
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_receipt()
                .unwrap(),
            b"approved58"
        );
        assert_eq!(
            client
                .approved_check_request()
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_receipt()
                .unwrap(),
            b"approved58"
        );
    }
}

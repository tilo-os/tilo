use super::*;
use agent_provider_config::protocol::{ErrorKind, ModelError};

#[test]
fn typed_rpc_errors_override_legacy_prose_and_redact_messages() {
    for kind in [
        ErrorKind::Unavailable,
        ErrorKind::Authentication,
        ErrorKind::PermissionDenied,
        ErrorKind::InvalidRequest,
        ErrorKind::InvalidResponse,
        ErrorKind::Unsupported,
        ErrorKind::RateLimited,
        ErrorKind::CreditsExhausted,
        ErrorKind::ContextLimit,
        ErrorKind::Provider,
        ErrorKind::TimedOut,
        ErrorKind::Cancelled,
        ErrorKind::Truncated,
        ErrorKind::Refused,
        ErrorKind::Transport,
    ] {
        let mut response = Response::failure(ModelError {
            kind,
            message: "secret-never-echo".into(),
        });
        let wire = serde_json::to_string(&response).unwrap();
        assert!(!wire.contains("secret-never-echo"));
        response.error = Some("credits exhausted".into());
        let response: Response =
            serde_json::from_str(&serde_json::to_string(&response).unwrap()).unwrap();
        assert_eq!(response.into_result().unwrap_err().model_error().kind, kind);
    }
}

#[test]
fn rpc3_legacy_errors_remain_readable() {
    #[derive(serde::Deserialize)]
    struct OldResponse {
        ok: bool,
        error: Option<String>,
    }
    let response: Response =
        serde_json::from_str(r#"{"ok":false,"error":"provider credits exhausted"}"#).unwrap();
    assert!(response.model_error.is_none());
    assert_eq!(
        response.failure_error().model_error().kind,
        ErrorKind::CreditsExhausted
    );
    // Older RPC3 clients ignore the additive diagnostic.
    let wire =
        serde_json::to_string(&Response::failure(ModelError::new(ErrorKind::Transport))).unwrap();
    let old: OldResponse = serde_json::from_str(&wire).unwrap();
    assert!(!old.ok);
    assert!(old.error.is_some());
}

#[test]
fn transport_kinds_come_from_source_metadata() {
    for (timeout, connect, kind, retry) in [
        (true, false, ErrorKind::TimedOut, true),
        (false, true, ErrorKind::Transport, true),
        (false, false, ErrorKind::Transport, false),
    ] {
        let failure = openai::Failure::transport(timeout, connect);
        assert_eq!(failure.model_error().kind, kind);
        assert_eq!(failure.retryable(), retry);
    }
}

#[test]
fn billing_envelopes_never_admit_retries() {
    for code in [
        "subscription_required",
        "insufficient_credit",
        "insufficient_credits",
        "credit_exhausted",
        "credits_exhausted",
        "insufficient_quota",
        "credit_balance_too_low",
        "billing_hard_limit_reached",
    ] {
        for status in [400, 402, 429, 500, 529] {
            for body in [
                serde_json::json!({"error":{"code":code,"type":"rate_limit_error","message":"secret-never-echo"}}),
                serde_json::json!({"error":{"type":code,"code":"unknown-secret-never-echo"}}),
                serde_json::json!({"error":"secret-never-echo","code":code}),
            ] {
                let failure = openai::Failure::from_http(status, &body.to_string(), None);
                assert_eq!(
                    failure.model_error().kind,
                    ErrorKind::CreditsExhausted,
                    "{status} {code}"
                );
                assert!(!failure.retryable());
                assert!(!failure.refused_unprocessed(status));
                assert!(!failure.to_string().contains("secret-never-echo"));
                let response = Response::failure(failure.model_error());
                assert_eq!(
                    response.failure_error().model_error().kind,
                    ErrorKind::CreditsExhausted
                );
            }
        }
    }
}

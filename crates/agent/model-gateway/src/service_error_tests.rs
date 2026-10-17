use super::*;
use agent_provider_config::protocol::ErrorKind;

#[test]
fn provider_envelopes_agree_on_diagnostics_and_retry_admission() {
    for (status, code, kind, retry) in [
        (
            429,
            "credit_balance_too_low",
            ErrorKind::CreditsExhausted,
            false,
        ),
        (529, "credit_exhausted", ErrorKind::CreditsExhausted, false),
        (
            429,
            "insufficient_credits",
            ErrorKind::CreditsExhausted,
            false,
        ),
        (
            429,
            "billing_hard_limit_reached",
            ErrorKind::CreditsExhausted,
            false,
        ),
        (
            429,
            "subscription_required",
            ErrorKind::CreditsExhausted,
            false,
        ),
        (
            429,
            "insufficient_quota",
            ErrorKind::CreditsExhausted,
            false,
        ),
        (429, "rate_limit_error", ErrorKind::RateLimited, true),
        (529, "overloaded_error", ErrorKind::Provider, true),
        (
            400,
            "context_length_exceeded",
            ErrorKind::ContextLimit,
            false,
        ),
        (401, "invalid_api_key", ErrorKind::Authentication, false),
    ] {
        let body = serde_json::json!({"error": {"code": code, "message": "secret-never-echo"}})
            .to_string();
        let failure = crate::openai::Failure::from_http(status, &body, None);
        assert_eq!(failure.model_error().kind, kind, "{code}");
        assert_eq!(
            refused_unprocessed(status, body.as_bytes()),
            retry,
            "{code}"
        );
        assert_eq!(failure.refused_unprocessed(status), retry, "{code}");
        let advice = provider_error_advice(status, body.as_bytes());
        assert!(!advice.contains("secret-never-echo"));
        if kind == ErrorKind::CreditsExhausted {
            assert!(advice.contains("credits are exhausted"));
        }
        if kind == ErrorKind::ContextLimit {
            assert!(advice.contains("context limit exceeded"));
        }
    }
}

//! A server's login codes, asked over its HTTPS routes ([`tilo_server_protocol::login_code`]):
//! the one client of them, for the tab that makes a code and follows it, and the computer that
//! resolves it, reports its steps, claims it and certifies the tab. The page reaches it through
//! the runtime, the CLI natively.

use tilo_server_protocol::login_code::{
    CERTIFY_ROUTE, CLAIM_ROUTE, CREATE_ROUTE, Certify, Claim, ClaimRefused, Claimed, CreateCode,
    Created, FOLLOW_ROUTE, Follow, Offer, Polled, RESOLVE_ROUTE, Report, Resolve, Resolved,
    STEP_ROUTE, WAITING_ROUTE, Waiting, WaitingRequest,
};
use tilo_server_protocol::web::API_PREFIX;

use crate::https::HttpClient;
use crate::signer::DeviceSigner;

/// What a server answered instead of what a login code's route answers, or that it was not reached.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, thiserror::Error)]
#[error("{message}")]
pub struct CodeRefusal {
    /// The server's HTTP status; `0` when it was not reached, or answered something else.
    pub status: u16,
    /// What happened, the server's own sentence where it gave one.
    pub message: String,
    /// Why, as the server names it beside its sentence (a claim's
    /// [`ClaimRefused`] reason), where it names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl CodeRefusal {
    /// What kind of refusal it is, as the screens that show it tell them apart: `unreachable`,
    /// `not-a-device` (the server serves nobody for that person, or the device is not theirs),
    /// `unknown` (a code that is not there: never made, used or expired), `too-many` (asked too
    /// often from here), or `refused`.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self.status {
            0 => "unreachable",
            403 => "not-a-device",
            404 => "unknown",
            429 => "too-many",
            _ => "refused",
        }
    }

    /// Why a claim was refused, as the server named it.
    #[must_use]
    pub fn claim_refused(&self) -> Option<ClaimRefused> {
        let reason = self.reason.as_deref()?;
        serde_json::from_value(serde_json::Value::String(reason.to_owned())).ok()
    }

    fn unreached(why: impl std::fmt::Display) -> Self {
        Self {
            status: 0,
            reason: None,
            message: format!("The server could not be reached: {why}"),
        }
    }
}

/// `body` as the routes take it: JSON.
fn json(body: &impl serde::Serialize) -> Result<Vec<u8>, CodeRefusal> {
    serde_json::to_vec(body).map_err(CodeRefusal::unreached)
}

/// Post `body` (JSON) to `route` and read the answer: `None` for one with no body.
async fn post<T: serde::de::DeserializeOwned>(
    client: &dyn HttpClient,
    route: &str,
    body: Vec<u8>,
) -> Result<Option<T>, CodeRefusal> {
    let (status, answer) = client
        .post(&format!("{API_PREFIX}{route}"), body)
        .await
        .map_err(CodeRefusal::unreached)?;
    if !(200..300).contains(&status) {
        // The server says why in `{"error": "…"}`: that sentence is the reason.
        #[derive(serde::Deserialize)]
        struct Said {
            error: String,
            #[serde(default)]
            reason: Option<String>,
        }
        let said = serde_json::from_slice::<Said>(&answer).ok();
        let reason = said.as_ref().and_then(|said| said.reason.clone());
        return Err(CodeRefusal {
            status,
            reason,
            message: match said {
                Some(said) if !said.error.is_empty() => {
                    format!("The server answered {status}: {}", said.error)
                }
                _ => format!("The server answered {status}"),
            },
        });
    }
    if status == 204 || answer.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(&answer)
        .map(Some)
        .map_err(|error| CodeRefusal {
            status: 0,
            reason: None,
            message: format!("The server answered something else: {error}"),
        })
}

/// `route`'s answer, which it always gives.
async fn answered<T: serde::de::DeserializeOwned>(
    client: &dyn HttpClient,
    route: &str,
    body: Vec<u8>,
) -> Result<T, CodeRefusal> {
    post(client, route, body).await?.ok_or_else(|| CodeRefusal {
        status: 0,
        reason: None,
        message: "The server answered nothing".to_owned(),
    })
}

/// Make a code: naming `link`, the tab's pairing link (setting up through an agent), or nothing
/// (asking to be signed in by a device of the person).
///
/// # Errors
/// The server was not reached or refused.
pub async fn create(client: &dyn HttpClient, link: Option<String>) -> Result<Created, CodeRefusal> {
    answered(client, CREATE_ROUTE, json(&CreateCode { link })?).await
}

/// Where the code `follow` (the secret only its tab holds) is. With the `offer` the code was made
/// as ([`Created`] and the link it names), the server keeps the code if it does not yet: a code is
/// only offered until its tab first follows it.
///
/// # Errors
/// The server was not reached or refused.
pub async fn follow(
    client: &dyn HttpClient,
    follow: &str,
    offer: Option<Offer>,
) -> Result<Polled, CodeRefusal> {
    answered(
        client,
        FOLLOW_ROUTE,
        json(&Follow {
            follow: follow.to_owned(),
            offer,
        })?,
    )
    .await
}

/// What the code `code` names, spending it.
///
/// # Errors
/// The server was not reached, or the code is not there ([`CodeRefusal::code`] `unknown`).
pub async fn resolve(client: &dyn HttpClient, code: &str) -> Result<Resolved, CodeRefusal> {
    answered(
        client,
        RESOLVE_ROUTE,
        json(&Resolve {
            code: code.to_owned(),
        })?,
    )
    .await
}

/// Tell the code's tab a step the computer took.
///
/// # Errors
/// The server was not reached or refused.
pub async fn report(client: &dyn HttpClient, report: &Report) -> Result<(), CodeRefusal> {
    post::<serde_json::Value>(client, STEP_ROUTE, json(report)?)
        .await
        .map(drop)
}

/// Claim the sign-in code resolved as `report` for the person `cert` (postcard, as hex) certifies
/// this device of, `signer` holding its key. The device may admit devices: it certifies the tab's
/// request itself ([`waiting`], [`certify`]).
///
/// # Errors
/// The server was not reached, or refused it (`403` with a [`ClaimRefused`] reason in
/// [`CodeRefusal::claim_refused`]).
pub async fn claim(
    client: &dyn HttpClient,
    report: &str,
    cert: String,
    signer: &dyn DeviceSigner,
    signed_at_ns: u64,
) -> Result<Claimed, CodeRefusal> {
    let digest =
        tilo_server_protocol::login_code::claim_digest(report, signed_at_ns).map_err(|error| {
            CodeRefusal {
                status: 0,
                reason: None,
                message: format!("This device did not sign the claim: {error}"),
            }
        })?;
    let signature = signer
        .sign(digest.to_vec())
        .await
        .map_err(|error| CodeRefusal {
            status: 0,
            reason: None,
            message: format!("This device did not sign the claim: {}", error.0),
        })?;
    answered(
        client,
        CLAIM_ROUTE,
        json(&Claim {
            report: report.to_owned(),
            cert,
            signed_at_ns,
            signature: hex::encode(signature),
        })?,
    )
    .await
}

/// The tab's certification request (postcard, as hex) for the code this computer claimed as
/// `report` to certify itself, once the tab asked.
///
/// # Errors
/// The server was not reached or refused.
pub async fn waiting(client: &dyn HttpClient, report: &str) -> Result<Option<String>, CodeRefusal> {
    answered::<WaitingRequest>(
        client,
        WAITING_ROUTE,
        json(&Waiting {
            report: report.to_owned(),
        })?,
    )
    .await
    .map(|waiting| waiting.request)
}

/// Answer the tab's request for the code claimed as `report` with `cert`, the certificate of its
/// device the claiming device signed (postcard, as hex).
///
/// # Errors
/// The server was not reached or refused.
pub async fn certify(
    client: &dyn HttpClient,
    report: &str,
    cert: String,
) -> Result<(), CodeRefusal> {
    post::<serde_json::Value>(
        client,
        CERTIFY_ROUTE,
        json(&Certify {
            report: report.to_owned(),
            cert,
        })?,
    )
    .await
    .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signer::{SignError, SignFuture};

    /// A server that is never asked: the refusing signer answers first.
    struct NoAnswer;

    impl crate::https::HttpClient for NoAnswer {
        fn post(&self, _path: &str, _body: Vec<u8>) -> crate::https::HttpFuture<'_> {
            Box::pin(async { Err("unreachable".to_string()) })
        }
    }

    /// A device key that refuses: no claim is made.
    struct Refusing;

    impl DeviceSigner for Refusing {
        fn device_public_key(&self) -> [u8; 32] {
            [0; 32]
        }

        fn sign(&self, _message: Vec<u8>) -> SignFuture<'_> {
            Box::pin(async { Err(SignError("locked".into())) })
        }
    }

    #[tokio::test]
    async fn a_refused_signature_claims_nothing() {
        let claim = claim(&NoAnswer, "report", "00".to_string(), &Refusing, 1).await;
        assert!(matches!(claim, Err(refusal) if refusal.status == 0));
    }

    #[test]
    fn each_refusal_says_what_kind_it_is() {
        let refusal = |status| CodeRefusal {
            status,
            reason: None,
            message: String::new(),
        };
        assert_eq!(refusal(0).code(), "unreachable");
        assert_eq!(refusal(403).code(), "not-a-device");
        assert_eq!(refusal(404).code(), "unknown");
        assert_eq!(refusal(429).code(), "too-many");
        assert_eq!(refusal(500).code(), "refused");
    }
}

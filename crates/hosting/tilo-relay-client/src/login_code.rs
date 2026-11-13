//! A host's login codes and the person's stand-in approver, asked over its HTTPS routes
//! ([`tilo_relay_protocol::login_code`]): the one client of them, for the tab that makes a code
//! and follows it, the computer that resolves it, reports its steps and claims it, and the root
//! that keeps its stand-in. The page reaches it through the runtime, the CLI natively.

use tilo_relay_protocol::login_code::{
    CLAIM_ROUTE, CREATE_ROUTE, Claim, Claimed, CreateCode, Created, FOLLOW_ROUTE, Follow, Offer,
    Polled, RESOLVE_ROUTE, Report, Resolve, Resolved, STAND_IN_ROUTE, STEP_ROUTE, StandInReply,
    StandInRequest,
};
use tilo_relay_protocol::web::API_PREFIX;

use crate::https::HttpClient;
use crate::signer::DeviceSigner;

/// What a host answered instead of what a login code's route answers, or that it was not reached.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, thiserror::Error)]
#[error("{message}")]
pub struct CodeRefusal {
    /// The host's HTTP status; `0` when it was not reached, or answered something else.
    pub status: u16,
    /// What happened, the host's own sentence where it gave one.
    pub message: String,
}

impl CodeRefusal {
    /// What kind of refusal it is, as the screens that show it tell them apart: `unreachable`,
    /// `not-a-device` (the host serves nobody for that person, or the device is not theirs),
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

    fn unreached(why: impl std::fmt::Display) -> Self {
        Self {
            status: 0,
            message: format!("The host could not be reached: {why}"),
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
        // The host says why in `{"error": "…"}`: that sentence is the reason.
        #[derive(serde::Deserialize)]
        struct Said {
            error: String,
        }
        let said = serde_json::from_slice::<Said>(&answer).map(|said| said.error);
        return Err(CodeRefusal {
            status,
            message: match said {
                Ok(said) if !said.is_empty() => format!("The host answered {status}: {said}"),
                _ => format!("The host answered {status}"),
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
            message: format!("The host answered something else: {error}"),
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
        message: "The host answered nothing".to_owned(),
    })
}

/// Make a code: naming `link`, the tab's pairing link (setting up through an agent), or nothing
/// (asking to be signed in by a device of the person).
///
/// # Errors
/// The host was not reached or refused.
pub async fn create(client: &dyn HttpClient, link: Option<String>) -> Result<Created, CodeRefusal> {
    answered(client, CREATE_ROUTE, json(&CreateCode { link })?).await
}

/// Where the code `follow` (the secret only its tab holds) is. With the `offer` the code was made
/// as ([`Created`] and the link it names), the host keeps the code if it does not yet: a code is
/// only offered until its tab first follows it.
///
/// # Errors
/// The host was not reached or refused.
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
/// The host was not reached, or the code is not there ([`CodeRefusal::code`] `unknown`).
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
/// The host was not reached or refused.
pub async fn report(client: &dyn HttpClient, report: &Report) -> Result<(), CodeRefusal> {
    post::<serde_json::Value>(client, STEP_ROUTE, json(report)?)
        .await
        .map(drop)
}

/// Claim the sign-in code resolved as `report` for the person `cert` (postcard, as hex) certifies
/// this device of, `signer` holding its key.
///
/// # Errors
/// The host was not reached, serves nobody for that person ([`CodeRefusal::code`]
/// `not-a-device`), or refused.
pub async fn claim(
    client: &dyn HttpClient,
    report: &str,
    cert: String,
    signer: &dyn DeviceSigner,
    signed_at_ns: u64,
) -> Result<Claimed, CodeRefusal> {
    let signature = signer
        .sign(tilo_relay_protocol::login_code::claim_message(
            report,
            signed_at_ns,
        ))
        .await
        .map_err(|error| CodeRefusal {
            status: 0,
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

/// Ask the person's stand-in on the host for `request`.
///
/// # Errors
/// The host was not reached or refused.
pub async fn stand_in(
    client: &dyn HttpClient,
    request: &StandInRequest,
) -> Result<StandInReply, CodeRefusal> {
    answered(client, STAND_IN_ROUTE, json(request)?).await
}

/// What a stand-in operation the root signs asks of the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StandInOp {
    /// Make, or name again, the stand-in's key ([`StandInRequest::Key`]).
    Key,
    /// Forget it ([`StandInRequest::Retire`]).
    Retire,
}

/// `op`, signed by the person's `root` at `signed_at_ns`.
///
/// # Errors
/// The root did not sign.
pub async fn signed_stand_in(
    op: StandInOp,
    root: &dyn DeviceSigner,
    signed_at_ns: u64,
) -> Result<StandInRequest, crate::SignError> {
    let user = root.device_public_key();
    let name = match op {
        StandInOp::Key => "key",
        StandInOp::Retire => "retire",
    };
    let signature = hex::encode(
        root.sign(StandInRequest::signed_message(name, &user, signed_at_ns))
            .await?,
    );
    let user = hex::encode(user);
    Ok(match op {
        StandInOp::Key => StandInRequest::Key {
            user,
            signed_at_ns,
            signature,
        },
        StandInOp::Retire => StandInRequest::Retire {
            user,
            signed_at_ns,
            signature,
        },
    })
}

/// What a stand-in approver's certificate says it is.
const STAND_IN_LABEL: &str = "Stand-in approver";

/// Give the person whose root is `root` a stand-in on the host `client` reaches, so that losing
/// the one place their root is (a tab) does not lose them: the host makes a key, the root
/// certifies it as an approver of the person's new devices, for as long as a stand-in lasts, and
/// the host keeps the certificate. Resolves to the stand-in's key, hex.
///
/// # Errors
/// The host was not reached or refused, or the root did not certify the key.
pub async fn keep_stand_in(
    client: &dyn HttpClient,
    root: &dyn DeviceSigner,
) -> Result<String, CodeRefusal> {
    let unsigned = |why: String| CodeRefusal {
        status: 0,
        message: format!("The root did not certify its stand-in: {why}"),
    };
    let asked = signed_stand_in(StandInOp::Key, root, tilo_clock::unix_now_ns())
        .await
        .map_err(|error| unsigned(error.0))?;
    let StandInReply::Key { key } = stand_in(client, &asked).await? else {
        return Err(CodeRefusal {
            status: 0,
            message: "The host named no stand-in key".to_owned(),
        });
    };
    let stand_in_key: [u8; 32] = hex::decode(&key)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| unsigned("the host named no key".to_owned()))?;
    let cert = crate::issue_stand_in(
        root,
        stand_in_key,
        STAND_IN_LABEL,
        tilo_user_identity::STAND_IN_LIFETIME_NS,
    )
    .await
    .map_err(|error| unsigned(error.to_string()))?;
    let cert = tilo_user_identity::canonical::canonical_bytes(&cert)
        .map_err(|error| unsigned(error.to_string()))?;
    stand_in(
        client,
        &StandInRequest::Certify {
            cert: hex::encode(cert),
        },
    )
    .await?;
    Ok(key)
}

/// The person keeps their root another way now: the host forgets their stand-in's key. The root
/// revokes the key itself, as it revokes any of the person's devices.
///
/// # Errors
/// The host was not reached or refused, or the root did not sign.
pub async fn retire_stand_in(
    client: &dyn HttpClient,
    root: &dyn DeviceSigner,
) -> Result<(), CodeRefusal> {
    let asked = signed_stand_in(StandInOp::Retire, root, tilo_clock::unix_now_ns())
        .await
        .map_err(|error| CodeRefusal {
            status: 0,
            message: format!("The root did not sign: {}", error.0),
        })?;
    stand_in(client, &asked).await.map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_refusal_says_what_kind_it_is() {
        let refusal = |status| CodeRefusal {
            status,
            message: String::new(),
        };
        assert_eq!(refusal(0).code(), "unreachable");
        assert_eq!(refusal(403).code(), "not-a-device");
        assert_eq!(refusal(404).code(), "unknown");
        assert_eq!(refusal(429).code(), "too-many");
        assert_eq!(refusal(500).code(), "refused");
    }
}

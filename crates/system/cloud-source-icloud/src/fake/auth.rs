//! idmsa sign-in and setup.icloud.com, server side. The SRP half is
//! `tilo-crypto`'s test server, so a wrong password fails the proof exactly
//! as Apple's would.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};
use test_http::{Reply, Request};
use tilo_crypto::srp_apple::{ELEMENT_LEN, server};

use super::{
    APPLE_ID, DEVICE_CODE, PHONE_ID, SMS_CODE, SignInState, State, has_web_session, json_reply,
};

pub fn verifier(password: &str, salt: &[u8], iterations: u32, protocol: &str) -> [u8; ELEMENT_LEN] {
    server::verifier(password, salt, iterations, protocol).expect("protocol")
}

fn body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap_or(Value::Null)
}

fn service_error(status: u16, code: &str) -> Reply {
    json_reply(
        status,
        &json!({"serviceErrors": [{"code": code, "message": "refused"}]}),
    )
}

/// The sign-in a request belongs to, by its echoed session id.
fn sign_in_id(request: &Request) -> Option<String> {
    request
        .header("X-Apple-ID-Session-Id")
        .map(ToOwned::to_owned)
}

#[allow(clippy::too_many_lines, reason = "one match over idmsa's routes")]
pub fn idmsa(state: &mut State, request: &Request, path: &str) -> Reply {
    if path == "/appleauth/authorize/signin" {
        let id = state.next("sid-");
        let scnt = state.next("scnt-");
        let srp = server::ServerSession::new(&state.verifier).expect("randomness");
        state.sign_ins.insert(
            id.clone(),
            SignInState {
                srp,
                a_pub: None,
                session_token: None,
                code_verified: false,
                texted: None,
            },
        );
        return Reply::status(200)
            .header("scnt", &scnt)
            .header("X-Apple-ID-Session-Id", &id)
            .header(
                "Set-Cookie",
                "aasp=fake-aasp; Domain=idmsa.apple.com; Path=/; Secure",
            )
            .body("<html></html>");
    }
    let Some(route) = path.strip_prefix("/appleauth/auth") else {
        return Reply::status(404);
    };
    // Every step after /authorize/signin must echo scnt and the session id.
    let Some(id) = sign_in_id(request).filter(|id| state.sign_ins.contains_key(id)) else {
        return service_error(401, "-20101");
    };
    if request.header("scnt").is_none() || request.header("X-Apple-Widget-Key").is_none() {
        return service_error(401, "-20101");
    }
    let payload = body(request);
    match (request.method.as_str(), route) {
        ("POST", "/federate") => {
            if payload["accountName"].as_str() == Some(APPLE_ID) {
                json_reply(200, &json!({"hasSWP": false}))
            } else {
                service_error(400, "-20101")
            }
        }
        ("POST", "/signin/init") => signin_init(state, &id, &payload),
        ("POST", "/signin/complete") => signin_complete(state, &id, &payload),
        ("GET", "") => {
            let phone = json!({
                "id": PHONE_ID,
                "numberWithDialCode": "+1 (•••) •••-••12",
                "obfuscatedNumber": "(•••) •••-••12",
                "pushMode": "sms"
            });
            json_reply(
                200,
                &json!({
                    "trustedPhoneNumbers": [phone],
                    "noTrustedDevices": !state.options.trusted_devices,
                    "authenticationType": "hsa2",
                    "hsa2Account": true
                }),
            )
        }
        ("PUT", "/verify/trusteddevice/securitycode") => {
            if !state.options.trusted_devices {
                return service_error(412, "-21666");
            }
            state.stats.pushes = state.stats.pushes.saturating_add(1);
            Reply::status(202)
        }
        ("POST", "/verify/trusteddevice/securitycode") => {
            let accepted = state.options.trusted_devices
                && payload["securityCode"]["code"].as_str() == Some(DEVICE_CODE);
            code_verdict(state, &id, accepted)
        }
        ("PUT", "/verify/phone") => {
            if payload["phoneNumber"]["id"].as_u64() != Some(PHONE_ID) {
                return service_error(400, "-21667");
            }
            state.stats.texts = state.stats.texts.saturating_add(1);
            if let Some(sign_in) = state.sign_ins.get_mut(&id) {
                sign_in.texted = Some(PHONE_ID);
            }
            json_reply(200, &json!({"trustedPhoneNumber": {"id": PHONE_ID}}))
        }
        ("POST", "/verify/phone/securitycode") => {
            let texted = state.sign_ins.get(&id).and_then(|s| s.texted);
            let accepted = texted.is_some()
                && payload["phoneNumber"]["id"].as_u64() == texted
                && payload["securityCode"]["code"].as_str() == Some(SMS_CODE);
            code_verdict(state, &id, accepted)
        }
        ("GET", "/2sv/trust") => {
            let verified = state.sign_ins.get(&id).is_some_and(|s| s.code_verified);
            if !verified {
                return service_error(401, "-21669");
            }
            let trust = state.next("tt-");
            let token = state.next("st-");
            state.trust_tokens.insert(trust.clone());
            state.session_tokens.insert(token.clone(), true);
            if let Some(sign_in) = state.sign_ins.get_mut(&id) {
                sign_in.session_token = Some(token.clone());
            }
            Reply::status(204)
                .header("X-Apple-TwoSV-Trust-Token", &trust)
                .header("X-Apple-Session-Token", &token)
        }
        ("POST", "/repair/complete") => Reply::status(200),
        _ => Reply::status(404),
    }
}

fn signin_init(state: &mut State, id: &str, payload: &Value) -> Reply {
    if payload["accountName"].as_str() != Some(APPLE_ID) {
        return service_error(401, "-20101");
    }
    let Some(a_pub) = payload["a"].as_str().and_then(|a| BASE64.decode(a).ok()) else {
        return service_error(400, "-20101");
    };
    let salt = BASE64.encode(&state.salt);
    let iterations = state.iterations;
    let protocol = state.options.protocol;
    let Some(sign_in) = state.sign_ins.get_mut(id) else {
        return service_error(401, "-20101");
    };
    sign_in.a_pub = Some(a_pub);
    json_reply(
        200,
        &json!({
            "iteration": iterations,
            "salt": salt,
            "protocol": protocol,
            "b": BASE64.encode(sign_in.srp.public()),
            "c": format!("challenge-{id}"),
        }),
    )
}

fn signin_complete(state: &mut State, id: &str, payload: &Value) -> Reply {
    let Some(sign_in) = state.sign_ins.get(id) else {
        return service_error(401, "-20101");
    };
    let Some(a_pub) = sign_in.a_pub.clone() else {
        return service_error(400, "-20101");
    };
    if payload["c"].as_str() != Some(format!("challenge-{id}").as_str()) {
        return service_error(400, "-20101");
    }
    let Ok(expected) = sign_in.srp.expected(APPLE_ID, &state.salt, &a_pub) else {
        return service_error(400, "-20101");
    };
    let sent = payload["m1"].as_str().and_then(|m1| BASE64.decode(m1).ok());
    if sent.as_deref() != Some(expected.m1.as_slice()) {
        return service_error(401, "-20101");
    }
    let sent_m2 = payload["m2"].as_str().and_then(|m2| BASE64.decode(m2).ok());
    if sent_m2.as_deref() != Some(expected.m2.as_slice()) {
        return service_error(400, "-20101");
    }
    let trusted = payload["trustTokens"].as_array().is_some_and(|tokens| {
        tokens
            .iter()
            .filter_map(Value::as_str)
            .any(|token| state.trust_tokens.contains(token))
    });
    let token = state.next("st-");
    state.session_tokens.insert(token.clone(), trusted);
    if let Some(sign_in) = state.sign_ins.get_mut(id) {
        sign_in.session_token = Some(token.clone());
    }
    let reply = if trusted {
        json_reply(200, &json!({"authType": "hsa2"}))
    } else {
        json_reply(409, &json!({"authType": "hsa2"}))
    };
    reply
        .header("X-Apple-Session-Token", &token)
        .header("X-Apple-ID-Account-Country", "USA")
        .header("X-Apple-Auth-Attributes", "attributes")
}

fn code_verdict(state: &mut State, id: &str, accepted: bool) -> Reply {
    if !accepted {
        return service_error(400, "-21669");
    }
    let token = state
        .sign_ins
        .get(id)
        .and_then(|sign_in| sign_in.session_token.clone())
        .unwrap_or_default();
    if let Some(sign_in) = state.sign_ins.get_mut(id) {
        sign_in.code_verified = true;
    }
    if state.options.accept_code_with_conflict {
        json_reply(409, &json!({"securityCode": {"valid": true}}))
            .header("X-Apple-Session-Token", &token)
    } else {
        Reply::status(204)
    }
}

/// What `accountLogin` and `validate` answer for a trusted session.
pub(super) fn account(state: &State, trusted: bool) -> Value {
    let base = &state.base;
    let pcs = state.options.advanced_data_protection;
    json!({
        "dsInfo": {"dsid": "12345", "hsaVersion": 2, "appleId": APPLE_ID},
        "hsaChallengeRequired": !trusted,
        "hsaTrustedBrowser": trusted,
        "webservices": {
            "drivews": {"url": format!("{base}/drivews"), "status": "active", "pcsRequired": pcs},
            "docws": {"url": format!("{base}/docws"), "status": "active", "pcsRequired": pcs},
            "ckdatabasews": {"url": format!("{base}/ckdatabasews"), "status": "active", "pcsRequired": true}
        }
    })
}

pub fn setup(state: &mut State, request: &Request, route: &str) -> Reply {
    match route {
        "accountLogin" => {
            let payload = body(request);
            let Some(trusted_token) = payload["dsWebAuthToken"]
                .as_str()
                .and_then(|token| state.session_tokens.get(token).copied())
            else {
                return json_reply(421, &json!({"error": "Invalid global session"}));
            };
            let trust_ok = payload["trustToken"]
                .as_str()
                .is_some_and(|token| state.trust_tokens.contains(token));
            let trusted = trusted_token || trust_ok;
            let mut reply = json_reply(200, &account(state, trusted));
            if trusted {
                let web = state.next("wt-");
                let validate = state.next("val-");
                state.web_tokens.insert(web.clone());
                reply = reply
                    .header(
                        "Set-Cookie",
                        &format!("X-APPLE-WEBAUTH-TOKEN=\"v=2:t={web}\"; Domain=.icloud.com; Path=/; Secure; HttpOnly"),
                    )
                    .header("Set-Cookie", "X-APPLE-WEBAUTH-USER=\"v=1:s=0:d=12345\"; Domain=.icloud.com; Path=/; Secure")
                    .header(
                        "Set-Cookie",
                        &format!("X-APPLE-WEBAUTH-VALIDATE=\"v=1:t={validate}\"; Domain=.icloud.com; Path=/; Secure"),
                    )
                    .header("Set-Cookie", "X-APPLE-DS-WEB-SESSION-TOKEN=\"fake\"; Domain=.icloud.com; Path=/; Secure");
            }
            reply
        }
        "validate" => {
            state.stats.validates = state.stats.validates.saturating_add(1);
            if !has_web_session(state, request) {
                return json_reply(
                    421,
                    &json!({"error": "Missing X-APPLE-WEBAUTH-TOKEN cookie"}),
                );
            }
            json_reply(200, &account(state, true))
        }
        "requestWebAccessState" => json_reply(
            200,
            &json!({
                "isICDRSDisabled": state.options.advanced_data_protection,
                "isDeviceConsentedForPCS": false
            }),
        ),
        _ => Reply::status(404),
    }
}

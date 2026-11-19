//! Executable contract check for the loaded SDK; native CEF qualification is separate.
use super::{
    WebViewCommand, WebViewEvent, command_within_limits, decode_event, encode_command, protocol,
    validate_event, wipe_service_wire,
};
use browser_continuity_core::sessions::{Cookie, OriginStorage, SameSite, Service, SessionCookies};
use serde_json::{Value, json};

#[inline(never)]
pub fn service_session_capability() -> Result<Value, String> {
    let request = super::super::ServiceSessionRequest {
        request_id: 1,
        verify_only: false,
        session: SessionCookies {
            schema: 1,
            session_id: "protocol-probe".into(),
            service: Service::Slack,
            identity_public_key: [5; 32],
            setup_binding: "protocol-probe".into(),
            source_snapshot_digest: [6; 32],
            origin_storage: Vec::new(),
            cookies: vec![Cookie {
                host: "app.slack.com".into(),
                domain: "app.slack.com".into(),
                host_only: true,
                name: "__Host-session".into(),
                value: "synthetic-probe-cookie".into(),
                path: "/".into(),
                expires_unix_seconds: None,
                secure: true,
                http_only: true,
                same_site: SameSite::Lax,
            }],
        },
    };
    let command = WebViewCommand::ImportServiceSession(request.clone());
    if !command_within_limits(&command) || format!("{command:?}").contains("synthetic-probe-cookie")
    {
        return Err("Selected session SDK contract failed".into());
    }
    let mut wire = encode_command(command).ok_or("Selected session command unavailable")?;
    let mut packet = zeroize::Zeroizing::new(Vec::new());
    protocol::write_json(&mut *packet, &wire)
        .map_err(|_| "Selected session transport unavailable")?;
    let decoded = protocol::read_packet(&mut std::io::Cursor::new(packet.as_slice()))
        .map_err(|_| "Selected session transport unavailable")?;
    let Some(protocol::Packet::Json(mut decoded)) = decoded else {
        return Err("Selected session transport unavailable".into());
    };
    let roundtrip = wire == decoded
        && decoded["cookies"][0]["host_only"] == true
        && decoded["cookies"][0]["expires_unix_seconds"].is_null();
    wipe_service_wire(&mut wire);
    wipe_service_wire(&mut decoded);
    let mut foreign = request.clone();
    foreign.session.cookies[0].host = "foreign.example".into();
    let mut expired = request;
    expired.session.cookies[0].expires_unix_seconds = Some(1);
    let reply = json!({"event":"service_session_result","request_id":1,"session_id":"protocol-probe",
        "service":"slack","imported":true,"cookie_count":1});
    let mut leaked = reply.clone();
    leaked["value"] = json!("synthetic-probe-cookie");
    if validate_event(&leaked).is_ok() || decode_event(&leaked).is_some() {
        return Err("Selected session SDK public reply contract failed".into());
    }
    if !roundtrip
        || command_within_limits(&WebViewCommand::ImportServiceSession(foreign))
        || command_within_limits(&WebViewCommand::ImportServiceSession(expired))
        || validate_event(&reply).is_err()
        || !matches!(decode_event(&reply), Some(WebViewEvent::ServiceSessionResult(value)) if value.imported && value.cookie_count == 1)
    {
        return Err("Selected session SDK contract failed".into());
    }
    check_discord_origin_contract()?;
    Ok(json!({"schema":1,"service_session_protocol":2,"codec_verified":true}))
}

fn check_discord_origin_contract() -> Result<(), String> {
    let failure = || "Selected origin SDK contract failed".to_owned();
    let request = super::super::ServiceSessionRequest {
        request_id: 2,
        verify_only: false,
        session: SessionCookies {
            schema: 2,
            session_id: "origin-protocol-probe".into(),
            service: Service::Discord,
            identity_public_key: [5; 32],
            setup_binding: "origin-protocol-probe".into(),
            source_snapshot_digest: [6; 32],
            cookies: Vec::new(),
            origin_storage: vec![OriginStorage {
                origin: "https://discord.com".into(),
                key: "token".into(),
                value: "\"synthetic-origin-probe\"".into(),
                source_snapshot_digest: [7; 32],
            }],
        },
    };
    for verify_only in [false, true] {
        let mut selected = request.clone();
        selected.verify_only = verify_only;
        let command = WebViewCommand::ImportServiceSession(selected);
        if !command_within_limits(&command)
            || format!("{command:?}").contains("synthetic-origin-probe")
        {
            return Err(failure());
        }
        let mut wire = encode_command(command).ok_or_else(failure)?;
        let mut packet = zeroize::Zeroizing::new(Vec::new());
        protocol::write_json(&mut *packet, &wire).map_err(|_| failure())?;
        let Some(protocol::Packet::Json(mut decoded)) =
            protocol::read_packet(&mut std::io::Cursor::new(packet.as_slice()))
                .map_err(|_| failure())?
        else {
            return Err(failure());
        };
        let matched = wire == decoded
            && decoded["cookies"] == json!([])
            && decoded["verify_only"] == verify_only
            && decoded["origin_storage"][0]["value"] == "\"synthetic-origin-probe\""
            && decoded["origin_storage"][0]
                .get("source_snapshot_digest")
                .is_none()
            && decoded.get("identity_public_key").is_none();
        wipe_service_wire(&mut wire);
        wipe_service_wire(&mut decoded);
        if !matched
            || wire["origin_storage"][0]["value"] != ""
            || decoded["origin_storage"][0]["value"] != ""
        {
            return Err(failure());
        }
    }
    let mut foreign = request.clone();
    foreign.session.origin_storage[0].origin = "https://discord.com.evil.example".into();
    let mut wrong_service = request.clone();
    wrong_service.session.service = Service::Slack;
    let mut unproven = request;
    unproven.session.origin_storage[0].source_snapshot_digest = [0; 32];
    for rejected in [foreign, wrong_service, unproven] {
        if command_within_limits(&WebViewCommand::ImportServiceSession(rejected)) {
            return Err(failure());
        }
    }
    let reply = json!({"event":"service_session_result","request_id":2,"session_id":"origin-protocol-probe",
        "service":"discord","imported":true,"cookie_count":0,"origin_storage_count":1});
    if validate_event(&reply).is_err()
        || !matches!(decode_event(&reply),Some(WebViewEvent::ServiceSessionResult(value)) if value.imported && value.cookie_count==0 && value.origin_storage_count==1)
    {
        return Err(failure());
    }
    let mut leaked = reply.clone();
    leaked["origin_storage"] = json!([{"value":"synthetic-origin-probe"}]);
    let mut unsupported = reply.clone();
    unsupported["service"] = json!("slack");
    let mut empty = reply;
    empty["origin_storage_count"] = json!(0);
    for rejected in [leaked, unsupported, empty] {
        if validate_event(&rejected).is_ok() || decode_event(&rejected).is_some() {
            return Err(failure());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn service_session_protocol_two_probe_exercises_cookie_and_origin_codecs() {
        let proof = super::service_session_capability().unwrap();
        assert_eq!(proof["service_session_protocol"], 2);
        assert_eq!(proof["codec_verified"], true);
        assert!(!proof.to_string().contains("synthetic-"));
        assert!(proof.get("authenticated").is_none());
    }
}

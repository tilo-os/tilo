//! Native website-session codec qualification: this backend checks its cookie and
//! origin-storage contracts with bounded synthetic selections, opens no vault, hands
//! out only public metadata and claims no provider authenticated anything.

use browser_continuity::sessions::{Service, SessionCookies};

pub struct SessionSelection {
    pub id: String,
    pub service: Service,
    pub binding: String,
}

fn selection(session: &SessionCookies) -> SessionSelection {
    SessionSelection {
        id: session.session_id.clone(),
        service: session.service,
        binding: session.setup_binding.clone(),
    }
}

/// Verify cookie and raw origin-storage custody codecs before qualifying
/// consumers; no engine, account store, keychain, network or provider is touched.
pub fn probe_service_session_codecs() -> Result<Vec<SessionSelection>, String> {
    use browser_continuity::sessions::{Cookie, OriginStorage, SameSite};
    let failure = || "Selected account consumer contract failed".to_owned();
    let session = SessionCookies {
        schema: 1,
        session_id: "consumer-probe".into(),
        service: Service::Slack,
        identity_public_key: [5; 32],
        setup_binding: "consumer-probe".into(),
        source_snapshot_digest: [6; 32],
        origin_storage: Vec::new(),
        cookies: vec![Cookie {
            host: "app.slack.com".into(),
            domain: "app.slack.com".into(),
            host_only: true,
            name: "__Host-session".into(),
            value: "synthetic-consumer-cookie".into(),
            path: "/".into(),
            expires_unix_seconds: None,
            secure: true,
            http_only: true,
            same_site: SameSite::Lax,
        }],
    };
    account_custody::service_sessions::verify_service_session_codec(&session)
        .map_err(|_| failure())?;
    let first = selection(&session);
    let mut discord = session;
    discord.schema = 2;
    discord.session_id = "consumer-origin-probe".into();
    discord.service = Service::Discord;
    discord.cookies.clear();
    discord.origin_storage = vec![OriginStorage {
        origin: "https://discord.com".into(),
        key: "token".into(),
        value: "\"synthetic-consumer-origin\"".into(),
        source_snapshot_digest: [7; 32],
    }];
    account_custody::service_sessions::verify_service_session_codec(&discord)
        .map_err(|_| failure())?;
    let second = selection(&discord);
    let mut foreign = discord.clone();
    let entry = foreign.origin_storage.first_mut().ok_or_else(failure)?;
    entry.origin = "https://discord.com.evil.example".into();
    if account_custody::service_sessions::service_session_secret(&foreign).is_ok() {
        return Err(failure());
    }
    Ok(vec![first, second])
}

#[cfg(test)]
mod tests {
    #[test]
    fn both_supported_codecs_return_only_public_selection_metadata() {
        let selections = super::probe_service_session_codecs().unwrap();
        assert_eq!(selections.len(), 2);
        assert_eq!(
            selections[0].service,
            browser_continuity::sessions::Service::Slack
        );
        assert_eq!(
            selections[1].service,
            browser_continuity::sessions::Service::Discord
        );
        assert_eq!(selections[0].binding, "consumer-probe");
        assert_eq!(selections[1].binding, "consumer-probe");
        assert_ne!(selections[0].id, selections[1].id);
    }
}

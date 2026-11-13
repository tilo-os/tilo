//! What a failed connection says to the person: the IMAP and SMTP paths
//! funnel their transport errors through [`classify_conn_failure`], so a
//! "connection refused" reads the same wherever it came from.

pub fn classify_conn_failure(detail: &str) -> String {
    let d = detail.to_ascii_lowercase();
    // Auth first: a 535 / "authentication failed" is more specific than the
    // generic transport keywords that might co-occur in the message.
    if is_auth_failure(&d) {
        return "authentication failed — check the username and password".into();
    }
    if d.contains("certificate")
        || d.contains("tls")
        || d.contains("handshake")
        || d.contains("self-signed")
        || d.contains("ssl")
    {
        return "TLS/certificate error — the server's certificate wasn't trusted".into();
    }
    if d.contains("refused") {
        return "connection refused — check the host and port".into();
    }
    if d.contains("timed out") || d.contains("timeout") {
        return "connection timed out — the server didn't respond".into();
    }
    if d.contains("dns")
        || d.contains("lookup")
        || d.contains("resolve")
        || d.contains("name or service not known")
        || d.contains("not known")
        || d.contains("no such host")
    {
        return "host not found — check the server address".into();
    }
    if d.contains("unreachable") || d.contains("network is unreachable") {
        return "network unreachable — check your connection".into();
    }
    format!("connection failed: {detail}")
}

pub fn is_auth_failure(detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    d.contains("535")
        || d.contains("authenticationfailed")
        || d.contains("authentication failed")
        || d.contains("invalid credentials")
        || d.contains("auth")
            && (d.contains("fail")
                || d.contains("reject")
                || d.contains("denied")
                || d.contains("invalid"))
        || d.contains("login failed")
        || d.contains("enter a password")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn classify_maps_transport_errors_with_auth_first() {
        {
            assert!(
                classify_conn_failure("[AUTHENTICATIONFAILED] Invalid credentials")
                    .contains("authentication failed")
            );
            assert!(
                classify_conn_failure("535 5.7.8 Username and Password not accepted")
                    .contains("authentication failed")
            );
            assert!(
                classify_conn_failure("Connection refused (os error 111)")
                    .contains("connection refused")
            );
            assert!(
                classify_conn_failure(
                    "failed to lookup address information: Name or service not known"
                )
                .contains("host not found")
            );
            assert!(
                classify_conn_failure("invalid peer certificate: UnknownIssuer")
                    .contains("TLS/certificate")
            );
            assert!(classify_conn_failure("connection timed out").contains("timed out"));
            let unknown = classify_conn_failure("some novel failure mode");
            assert!(unknown.contains("some novel failure mode"), "{unknown}");
        }

        {
            // A message that mentions both a socket concept and auth must classify
            // as auth (the more actionable, specific cause).
            let msg = "AUTHENTICATE failed: login rejected";
            assert!(
                classify_conn_failure(msg).contains("authentication failed"),
                "{}",
                classify_conn_failure(msg)
            );
        }
    }
}

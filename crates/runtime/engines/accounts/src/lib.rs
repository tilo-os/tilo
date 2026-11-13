//! Online-account custody and the IMAP client, as its own library in the runtime
//! bundle (`libtilo_accounts_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use account_custody;
#[cfg(feature = "thin")]
pub use async_imap;
#[cfg(feature = "thin")]
pub use tilo_dav;
#[cfg(feature = "thin")]
pub use tilo_oauth;

/// Run the engine's own code without a network or an account: the `OAuth`
/// client's PKCE against RFC 7636's example, the committed client table's
/// providers and scopes, and the IMAP parser on a server's capabilities.
///
/// The engine's check binary and the app's `--probe-…` call it, so a release
/// proves the library loads and runs against its runtime.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use account_custody::{ClientTable, OAuthCloud, OAuthIssuer, OAuthUse};
    use async_imap::imap_proto::{Capability, Response};

    // RFC 7636, appendix B.
    let pkce = tilo_oauth::Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
    if pkce.challenge != "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM" {
        return Err(format!(
            "tilo-oauth derived the S256 challenge {} for RFC 7636's verifier",
            pkce.challenge
        ));
    }

    let table = ClientTable::defaults();
    let mut providers = Vec::new();
    for oauth_use in OAuthCloud::ALL
        .into_iter()
        .map(OAuthCloud::oauth_use)
        .chain([OAuthUse::mail(OAuthIssuer::Google)])
    {
        let descriptor = table.descriptor(oauth_use);
        if descriptor.scopes.is_empty() || !descriptor.token_endpoint.starts_with("https://") {
            return Err(format!(
                "the committed client table gives {} no scopes or no HTTPS token endpoint",
                oauth_use.label()
            ));
        }
        providers.push(oauth_use.label());
    }

    let greeting = b"* CAPABILITY IMAP4rev1 IDLE AUTH=XOAUTH2\r\n";
    let capabilities = match Response::from_bytes(greeting) {
        Ok(([], Response::Capabilities(capabilities))) => capabilities,
        other => return Err(format!("imap-proto misread a CAPABILITY line: {other:?}")),
    };
    let expected = [
        Capability::Imap4rev1,
        Capability::Atom("IDLE".into()),
        Capability::Auth("XOAUTH2".into()),
    ];
    if capabilities != expected {
        return Err(format!("imap-proto read the capabilities {capabilities:?}"));
    }

    Ok(format!(
        "PKCE matches RFC 7636, {} configured, IMAP capabilities {}",
        providers.join(", "),
        capabilities.len()
    ))
}

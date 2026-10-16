//! Online-account custody and the IMAP client, as an engine library beside
//! the shared runtime (`libtilo_accounts_engine.so`).
//!
//! Mail and Settings sign in to online accounts through `account-custody`
//! (`OAuth` through `tilo-oauth`, the Google Drive, `OneDrive`, Dropbox and
//! `iCloud` providers it resolves new accounts against) and talk IMAP through
//! `async-imap`; Music's Spotify sign-in reaches custody through its own
//! engine library, which chains on this one. Inside `libruntime_dylib.so`
//! every app process mapped and relocated them. Here they are a library of
//! their own in the runtime bundle that only those name `DT_NEEDED`, and no
//! tapp carries a copy; the runtime keeps what they share with other apps
//! (`accounts`, `cloud-source`, the HTTP stack).
//!
//! Like `runtime-dylib`, this crate re-exports the crates it provides, which
//! records them as provided by this library for the apps' `-C prefer-dynamic`
//! link. Outside the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides, which the app's
// link then refuses (each crate may show up once).
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use account_custody;
#[cfg(feature = "thin")]
pub use async_imap;
#[cfg(feature = "thin")]
pub use tilo_oauth;

/// Run the engine's own code without a network or an account: the `OAuth`
/// client's PKCE against RFC 7636's example, the committed client table's
/// providers and scopes, and the IMAP parser on a server's capabilities.
///
/// The engine's check binary and the apps' `--probe-accounts-engine` call
/// it, so a release proves the library loads and runs against the runtime
/// it ships with, and that the apps load it themselves.
///
/// # Errors
/// Names the part that answered wrongly.
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

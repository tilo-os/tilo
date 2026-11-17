//! Which addresses a host dials for its people's providers.
use super::*;

#[test]
fn only_a_public_address_over_tls_is_a_persons_provider() {
    for public in [
        "https://api.anthropic.com",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        "https://8.8.8.8/v1",
        "https://[2606:4700:4700::1111]/v1",
    ] {
        assert!(public_endpoint(public), "{public}");
    }
    for inside in [
        "http://api.example.com/v1",
        "https://localhost/v1",
        "https://models.lan/v1",
        "https://intranet/v1",
        "https://127.0.0.1/v1",
        "https://10.1.2.3/v1",
        "https://192.168.1.4/v1",
        // Where a cloud machine reads its own credentials.
        "https://169.254.169.254/latest",
        "https://100.64.0.1/v1",
        "https://0.0.0.0/v1",
        "https://[::1]/v1",
        "https://[::ffff:10.0.0.1]/v1",
        "https://[fd00::1]/v1",
        "not an address",
        "",
    ] {
        assert!(!public_endpoint(inside), "{inside}");
    }
}

/// A name is checked again when it resolves: one that leads to this machine
/// is not dialled, whatever it is called.
#[tokio::test(flavor = "current_thread")]
async fn a_name_that_resolves_inside_is_not_dialled() {
    use reqwest::dns::Resolve as _;
    let name: reqwest::dns::Name = "localhost".parse().unwrap();
    assert!(PublicNames.resolve(name).await.is_err());
}

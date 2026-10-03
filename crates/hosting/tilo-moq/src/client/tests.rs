use super::*;

fn connection() -> Connection {
    Connection {
        scope: ConnectionScope {
            endpoint: "https://example.test/moq/voice".into(),
            fingerprints: vec![],
            owner_epoch: [1; 32],
            carrier_generation: 1,
            publication: Some(format!("{}/{}", "01".repeat(32), "02".repeat(32))),
            subscriptions: vec![],
        },
        authorization: vec![1],
    }
}
#[test]
fn inert_connection_validates_raw_endpoint_and_closed_voice_scope_only() {
    assert!(validate(&connection()).is_ok()); // Structural validity confers no readiness.
    for endpoint in [
        "http://example.test/moq/voice",
        "wss://example.test/moq/voice",
        "moqt://example.test/moq/voice",
        "https://example.test:443/moq/voice",
        "https://EXAMPLE.test/moq/voice",
        "https://example.test/moq/../moq/voice",
        "https://example.test/moq/%76oice",
        "https://user@example.test/moq/voice",
        "https://example.test/moq/voice?q=1",
        "https://example.test/moq/voice#x",
    ] {
        let mut connection = connection();
        connection.scope.endpoint = endpoint.into();
        assert!(validate(&connection).is_err(), "{endpoint}");
    }
    for name in ["audio", "call/publisher", "", "01/02"] {
        let mut connection = connection();
        connection.scope.publication = Some(name.into());
        assert!(validate(&connection).is_err());
    }
    let mut altered = connection();
    altered
        .scope
        .subscriptions
        .push(altered.scope.publication.clone().unwrap());
    assert!(validate(&altered).is_err());
    altered.scope.subscriptions = vec![format!("{}/{}", "03".repeat(32), "04".repeat(32))];
    assert!(validate(&altered).is_err());
    altered = connection();
    altered.authorization = vec![1; crate::relay::MAX_AUTHORIZATION_BYTES + 1];
    assert!(validate(&altered).is_err());
    altered = connection();
    altered.scope.owner_epoch = [0; 32];
    assert!(validate(&altered).is_err());
}

struct DeniedOwner;
impl ClientAuthority for DeniedOwner {
    fn current(&self, _: &ConnectionScope, _: Direction, _: &str) -> bool {
        false
    }
}
#[tokio::test]
async fn inert_proof_input_cannot_mint_authority_or_readiness() {
    assert!(matches!(
        Client::start(connection(), Arc::new(DeniedOwner)),
        Err(ClientError::Denied)
    ));
}

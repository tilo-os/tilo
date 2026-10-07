#[tokio::test]
async fn proxy_no_in_or_out_params() {
    use serde::{Deserialize, Serialize};
    use zlink::{Connection, proxy, test_utils::mock_socket::MockSocket};

    #[proxy("org.example.Basic")]
    trait BasicProxy {
        async fn ping(&mut self) -> zlink::Result<Result<(), BasicError>>;
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(tag = "error")]
    enum BasicError {
        NotFound,
        InvalidKey,
    }

    // Test that methods returning `()` should accept empty {} response and empty parameters.
    let responses = [r#"{}"#, r#"{parameters: {}}"#];
    let socket = MockSocket::with_responses(&responses);

    let mut conn = Connection::new(socket);

    conn.ping().await.unwrap().unwrap();
}

#[tokio::test]
async fn proxy_oneway_method() {
    use zlink::{Connection, proxy, test_utils::mock_socket::MockSocket};

    #[proxy("org.example.Basic")]
    trait BasicProxy {
        #[zlink(oneway)]
        async fn notify(&mut self, message: String) -> zlink::Result<()>;
    }

    // Oneway methods don't expect a response, so we provide an empty response list
    let responses: &[&str] = &[];
    let socket = MockSocket::with_responses(responses);

    let mut conn = Connection::new(socket);

    // This should send the message but not wait for a response
    conn.notify("Hello World".to_string()).await.unwrap();
}

#[tokio::test]
async fn proxy_parameter_rename() {
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use zlink::{Connection, proxy, test_utils::mock_socket::MockSocket};

    #[proxy("org.example.Basic")]
    trait BasicProxy {
        async fn create_user(
            &mut self,
            #[zlink(rename = "user_name")] name: String,
            #[zlink(rename = "user_email")] email: String,
        ) -> zlink::Result<Result<UserId, BasicError>>;
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct UserId {
        id: u32,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(tag = "error")]
    enum BasicError {
        InvalidInput,
        UserExists,
    }

    // Mock response with the expected parameter names in the request
    let responses = json!({"parameters": {"id": 123}}).to_string();
    let socket = MockSocket::with_responses(&[&responses]);

    let mut conn = Connection::new(socket);

    let result = conn
        .create_user("John Doe".to_string(), "john@example.com".to_string())
        .await
        .unwrap()
        .unwrap();
    let bytes_written = conn.write().write_half().written_data();
    let written: serde_json::Value =
        serde_json::from_slice(&bytes_written[..bytes_written.len() - 1]).unwrap();
    let expected = json!({
        "method": "org.example.Basic.CreateUser",
        "parameters": {
            "user_name": "John Doe",
            "user_email": "john@example.com"
        }
    });
    assert_eq!(written, expected);

    assert_eq!(result.id, 123);
}

#[tokio::test]
async fn optional_params_test() {
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use zlink::{Connection, proxy, test_utils::mock_socket::MockSocket};

    #[proxy("org.example.Optional")]
    trait OptionalProxy {
        async fn with_optionals(
            &mut self,
            required: &str,
            optional_string: Option<&str>,
            optional_number: Option<i32>,
            optional_bool: Option<bool>,
        ) -> zlink::Result<Result<OptionalsReply<'_>, Error>>;

        async fn mixed_optionals(
            &mut self,
            first: &str,
            opt1: Option<String>,
            second: i32,
            opt2: std::option::Option<bool>,
            third: &str,
            opt3: core::option::Option<Vec<String>>,
        ) -> zlink::Result<Result<(), Error>>;
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Error;

    #[derive(Debug, Serialize, Deserialize)]
    struct OptionalsReply<'a> {
        #[serde(borrow)]
        message: &'a str,
    }

    // Test with_optionals
    let responses = json!({"parameters": {"message": "success with optionals"}}).to_string();
    let socket = MockSocket::with_responses(&[&responses]);
    let mut conn = Connection::new(socket);

    let result = conn
        .with_optionals("required", Some("optional"), Some(42), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.message, "success with optionals");

    // Test mixed_optionals
    let responses = json!({}).to_string();
    let socket = MockSocket::with_responses(&[&responses]);
    let mut conn = Connection::new(socket);

    conn.mixed_optionals("first", None, 100, Some(true), "third", None)
        .await
        .unwrap()
        .unwrap();
}

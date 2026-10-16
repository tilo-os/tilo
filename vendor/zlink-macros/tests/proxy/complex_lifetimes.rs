#[tokio::test]
async fn complex_lifetimes_test() {
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::collections::HashMap;
    use zlink::{Connection, proxy, test_utils::mock_socket::MockSocket};

    // The explicit lifetimes are the point of this test: the proxy macro must handle them.
    #[allow(clippy::needless_lifetimes)]
    #[proxy("org.example.Complex")]
    trait ComplexProxy {
        async fn process_array(
            &mut self,
            items: &[String],
        ) -> zlink::Result<Result<Vec<Item>, Error>>;

        async fn process_nested(
            &mut self,
            data: HashMap<String, Vec<Option<Item>>>,
        ) -> zlink::Result<Result<ProcessNestedReply<'_>, Error>>;

        async fn with_tuples(
            &mut self,
            pairs: Vec<(String, i32)>,
        ) -> zlink::Result<Result<TuplesReply, Error>>;

        async fn generic_result<'a>(
            &mut self,
            input: &'a str,
        ) -> zlink::Result<Result<Response<'_>, CustomError<'_>>>;
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Item {
        id: u32,
        name: String,
        tags: Vec<String>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Response<'a> {
        message: &'a str,
        success: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Error;

    #[derive(Debug, Serialize, Deserialize)]
    struct CustomError<'a> {
        code: u32,
        message: &'a str,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct ProcessNestedReply<'a> {
        #[serde(borrow)]
        items: Option<Vec<&'a str>>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct TuplesReply {
        success: bool,
        message: Option<String>,
    }

    // Test process_array
    let responses = json!({
        "parameters": [{
            "id": 1,
            "name": "Test Item",
            "tags": ["tag1", "tag2"]
        }]
    })
    .to_string();
    let socket = MockSocket::with_responses(&[&responses]);
    let mut conn = Connection::new(socket);

    let items = vec!["item1".to_string()];
    let result = conn.process_array(&items).await.unwrap().unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id, 1);

    // Test process_nested
    let responses = json!({
        "parameters": {
            "items": ["result1", "result2"]
        }
    })
    .to_string();
    let socket = MockSocket::with_responses(&[&responses]);
    let mut conn = Connection::new(socket);

    let mut data = HashMap::new();
    data.insert(
        "key1".to_string(),
        vec![Some(Item {
            id: 1,
            name: "test".to_string(),
            tags: vec!["tag".to_string()],
        })],
    );
    let result = conn.process_nested(data).await.unwrap().unwrap();
    assert_eq!(result.items, Some(vec!["result1", "result2"]));

    // Test with_tuples
    let responses = json!({
        "parameters": {
            "success": true,
            "message": "success"
        }
    })
    .to_string();
    let socket = MockSocket::with_responses(&[&responses]);
    let mut conn = Connection::new(socket);

    let pairs = vec![("key1".to_string(), 100), ("key2".to_string(), 200)];
    let result = conn.with_tuples(pairs).await.unwrap().unwrap();
    assert!(result.success);
    assert_eq!(result.message, Some("success".to_string()));

    // Test generic_result
    let responses = json!({
        "parameters": {
            "message": "test response",
            "success": true
        }
    })
    .to_string();
    let socket = MockSocket::with_responses(&[&responses]);
    let mut conn = Connection::new(socket);

    let result = conn.generic_result("test input").await.unwrap().unwrap();
    assert_eq!(result.message, "test response");
    assert!(result.success);
}

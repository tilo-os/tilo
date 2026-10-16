use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct Error;

#[tokio::test]
async fn generic_test() {
    use serde_json::json;
    use zlink::{Connection, proxy, test_utils::mock_socket::MockSocket};

    #[derive(Debug, Serialize, Deserialize)]
    struct ProcessReply<'a> {
        #[serde(borrow)]
        result: &'a str,
    }

    #[proxy("org.example.Generic")]
    trait GenericProxy {
        async fn process<T: Serialize + std::fmt::Debug>(
            &mut self,
            data: T,
        ) -> zlink::Result<Result<ProcessReply<'_>, Error>>;
        async fn process2<U: Serialize + std::fmt::Debug>(
            &mut self,
            data: U,
        ) -> zlink::Result<Result<ProcessReply<'_>, Error>>;
    }

    // Test process with String type parameter
    let responses = json!({"parameters": {"result": "success"}}).to_string();
    let socket = MockSocket::with_responses(&[&responses]);
    let mut conn = Connection::new(socket);

    let result = conn
        .process("test data".to_string())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.result, "success");

    // Test process2 with i32 type parameter
    let responses = json!({"parameters": {"result": "process2 result"}}).to_string();
    let socket = MockSocket::with_responses(&[&responses]);
    let mut conn = Connection::new(socket);

    let result = conn.process2(123).await.unwrap().unwrap();
    assert_eq!(result.result, "process2 result");
}

#[tokio::test]
async fn where_clause_test() {
    use serde_json::json;
    use zlink::{Connection, proxy, test_utils::mock_socket::MockSocket};

    #[derive(Debug, Serialize, Deserialize)]
    struct GetReply<'a> {
        #[serde(borrow)]
        value: &'a str,
    }

    #[proxy("org.example.WhereClause")]
    trait WhereProxy {
        async fn get<T>(&mut self, value: T) -> zlink::Result<Result<GetReply<'_>, Error>>
        where
            T: Serialize + std::fmt::Debug;
        async fn get2<U>(&mut self, value: U) -> zlink::Result<Result<GetReply<'_>, Error>>
        where
            U: Serialize + std::fmt::Debug;
    }

    // Test get with i32 type parameter
    let responses = json!({"parameters": {"value": "42"}}).to_string();
    let socket = MockSocket::with_responses(&[&responses]);
    let mut conn = Connection::new(socket);

    let result = conn.get(42).await.unwrap().unwrap();
    assert_eq!(result.value, "42");

    // Test get2 with bool type parameter
    let responses = json!({"parameters": {"value": "true"}}).to_string();
    let socket = MockSocket::with_responses(&[&responses]);
    let mut conn = Connection::new(socket);

    let result = conn.get2(true).await.unwrap().unwrap();
    assert_eq!(result.value, "true");
}

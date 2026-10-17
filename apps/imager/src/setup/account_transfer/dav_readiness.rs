//! Local test responders for frontend tests of native account custody.
#[cfg(test)]
pub(in crate::setup) fn test_server() -> test_http::Responder {
    test_http::Responder::handle(|request| {
        assert_eq!(request.method, "PROPFIND");
        assert_eq!(request.header("depth"), Some("0"));
        assert!(request.header("authorization").is_some());
        Some(test_http::Reply::status(207).body(collection(&request.target)))
    })
}
#[cfg(test)]
pub(in crate::setup) fn collection(href: &str) -> String {
    format!(
        "<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>{href}</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"
    )
}

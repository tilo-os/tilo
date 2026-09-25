use super::*;
fn fixture() -> (tempfile::TempDir, LocalEnrollment, Parts) {
    let temp = tempfile::tempdir().unwrap();
    let auth = Arc::new(Authorization::local(temp.path(), "127.0.0.1:4000".into()).unwrap());
    let credential = std::fs::read_to_string(temp.path().join("enrollment-capability")).unwrap();
    let (commands, _receiver) = mpsc::channel(1);
    let gateway = LocalEnrollment::new("https://127.0.0.1:9443", auth, commands).unwrap();
    let parts = Request::builder()
        .method("POST")
        .uri("/qos-peer/enrollment/challenge")
        .header("host", "127.0.0.1:9443")
        .header("origin", "https://127.0.0.1:9443")
        .header("content-type", "application/json")
        .header("cookie", format!("{COOKIE_NAME}={credential}"))
        .body(())
        .unwrap()
        .into_parts()
        .0;
    (temp, gateway, parts)
}
#[test]
fn local_owner_cookie_requires_private_capability_and_exact_origin_and_route() {
    let (_temp, gateway, parts) = fixture();
    assert!(gateway.authorize(&parts).is_ok());
    for case in 0..12 {
        let (_temp, gateway, mut parts) = fixture();
        match case {
            0 => {
                parts.headers.remove("cookie");
            }
            1 => {
                parts.headers.insert(
                    "cookie",
                    format!("{COOKIE_NAME}={}", "00".repeat(32))
                        .parse()
                        .unwrap(),
                );
            }
            2 => {
                let value = parts.headers["cookie"].clone();
                parts.headers.append("cookie", value);
            }
            3 => {
                let value = parts.headers["cookie"].to_str().unwrap();
                parts
                    .headers
                    .insert("cookie", format!("{value}; {value}").parse().unwrap());
            }
            4 => {
                parts.headers.remove("origin");
            }
            5 => {
                parts
                    .headers
                    .insert("origin", "https://other.test".parse().unwrap());
            }
            6 => {
                parts
                    .headers
                    .insert("host", "127.0.0.1:9444".parse().unwrap());
            }
            7 => {
                parts.method = Method::GET;
            }
            8 => {
                parts.uri = "/qos-peer/enrollment/challenge?x=1".parse().unwrap();
            }
            9 => {
                parts.uri = "/qos-peer/enrollment/challenge/".parse().unwrap();
            }
            10 => {
                parts
                    .headers
                    .insert("content-type", "text/plain".parse().unwrap());
            }
            _ => {
                parts.headers.insert(PROOF_HEADER, "00".parse().unwrap());
            }
        }
        assert!(gateway.authorize(&parts).is_err(), "case {case}");
    }
    let (commands, _receiver) = mpsc::channel(1);
    let temp = tempfile::tempdir().unwrap();
    let auth = Arc::new(Authorization::local(temp.path(), "127.0.0.1:4000".into()).unwrap());
    assert!(LocalEnrollment::new("https://hosted.example", auth, commands).is_err());
}

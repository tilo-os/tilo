//! [`HttpClient`] over `tilo_net`: the peer's HTTPS carrier on the platform
//! HTTP stack, the same request the page's `httpsPost` makes with `fetch`.

use tilo_net::header::CONTENT_TYPE;
use tilo_net::{Client, Redirects, Url};

use crate::carrier::{HttpClient, HttpFuture};

/// `POST`s to one host origin through a `tilo_net::Client`: an
/// `application/octet-stream` body, the session token as `x-tilo-session`,
/// no cookies and no redirects.
#[derive(Clone, Debug)]
pub struct NetHttp {
    client: Client,
    origin: Url,
}

impl NetHttp {
    /// A carrier to `origin`, which must be a bare `https` origin.
    ///
    /// # Errors
    ///
    /// When `origin` is not an `https` origin, or the client cannot start.
    pub fn new(origin: &str) -> Result<Self, String> {
        let origin = Url::parse(origin).map_err(|error| format!("{origin}: {error}"))?;
        if origin.scheme() != "https"
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err("host origin must be an https origin".into());
        }
        let client = Client::builder()
            .redirects(Redirects::None)
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self { client, origin })
    }
}

impl HttpClient for NetHttp {
    fn post(&self, path: &str, session: Option<&str>, body: Vec<u8>) -> HttpFuture<'_> {
        let url = self.origin.join(path).map_err(|error| error.to_string());
        let session = session.map(str::to_owned);
        Box::pin(async move {
            let mut request = self
                .client
                .post(url?.as_str())
                .header(CONTENT_TYPE, "application/octet-stream")
                .body(body);
            if let Some(session) = session {
                request = request.header("x-tilo-session", session);
            }
            let response = request.send().await.map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            if response.status().is_redirection() {
                return Err(format!("the host redirected ({status})"));
            }
            let body = response.bytes().await.map_err(|error| error.to_string())?;
            Ok((status, body.to_vec()))
        })
    }
}

#[cfg(test)]
mod tests {
    use test_http::{Reply, Responder};

    use super::*;

    #[test]
    fn only_https_origins_are_hosts() {
        assert!(NetHttp::new("https://host.example:8443").is_ok());
        assert!(NetHttp::new("http://host.example").is_err());
        assert!(NetHttp::new("https://host.example/base").is_err());
    }

    #[test]
    fn post_sends_the_body_and_session_and_refuses_redirects() {
        let server = Responder::start(vec![
            Reply::status(200).body(vec![7, 8]),
            Reply::status(302).header("Location", "https://elsewhere.example/"),
        ]);
        // A loopback responder speaks plain HTTP; `new` insists on https.
        let http = NetHttp {
            client: Client::builder()
                .redirects(Redirects::None)
                .build()
                .unwrap(),
            origin: Url::parse(&format!("{}/", server.url())).unwrap(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let answer = runtime
            .block_on(http.post("/peer/v1/send", Some("ab"), vec![1, 2]))
            .unwrap();
        assert_eq!(answer, (200, vec![7, 8]));
        let request = server.next_request();
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/peer/v1/send");
        assert_eq!(request.header("x-tilo-session"), Some("ab"));
        assert_eq!(
            request.header("content-type"),
            Some("application/octet-stream")
        );
        assert_eq!(request.body, vec![1, 2]);
        assert!(
            runtime
                .block_on(http.post("/peer/v1/events", None, Vec::new()))
                .is_err()
        );
    }
}

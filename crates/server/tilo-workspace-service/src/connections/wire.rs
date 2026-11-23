//! The one way this server sends a request to a service a person named.
//!
//! A person, or a model quoting a page, types an address into a chat and
//! the server calls it: the classic way to make a server call into its
//! operator's own network. So every request for a connection goes through
//! here and nowhere else, at the first contact and at every later call, for
//! the service itself and for every address its sign-in names: the client
//! the fetch proxy guards its own requests with. It follows no redirect,
//! takes no proxy from the environment, and dials only addresses that were
//! checked at the very lookup the connection uses, so a name that resolves
//! into the server's network is not called, and one that resolves there later
//! is not called then either. Nothing is remembered from an earlier check.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

// The one place the guard's own names are taken from; the rest of the
// connections take them from here.
pub(crate) use tilo_net::proxy::host::{
    Destinations, Lookup, check_destination, guarded_client, is_non_public_literal,
    is_withheld_request_header, refused_address, system_lookup,
};
/// Only a calendar a turn connects reads a name's addresses itself.
#[cfg(feature = "agents")]
pub(crate) use tilo_net::proxy::host::{is_public, written_out};
use tilo_net::{HeaderValue, Url};

/// What this server calls for its people's connections, and how it looks a
/// name up.
#[derive(Clone)]
pub(crate) struct Reach {
    pub(crate) destinations: Destinations,
    pub(crate) lookup: Lookup,
}

impl Reach {
    /// What a server calls for the people it serves: addresses on the
    /// internet, and with `inside` its own network too, where its operator
    /// lets it call the addresses people name.
    pub(crate) fn of(inside: bool) -> Self {
        Self {
            destinations: if inside {
                Destinations::Any
            } else {
                Destinations::Public
            },
            lookup: system_lookup(),
        }
    }
}

/// Why a service was not asked, or did not answer what was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Problem {
    /// The address is not one this server calls. Nothing was sent.
    NotCalled,
    /// The service did not answer.
    NoAnswer,
    /// The service answered with a redirect. It is not followed.
    SentOn,
    /// The service wants a sign-in. `challenge` is what it said of where.
    SignIn { challenge: Option<String> },
    /// The service turned the request away.
    TurnedAway,
    /// The service's answer does not read, or is larger than is taken.
    Unreadable,
    /// The service answered that what was asked failed, in its own words.
    Said,
}

/// One request.
pub(crate) struct Ask {
    pub(crate) url: Url,
    /// What to post; a request without it is a read.
    pub(crate) post: Option<Vec<u8>>,
    pub(crate) headers: Vec<(&'static str, HeaderValue)>,
    /// How long the whole answer may take.
    pub(crate) wait: Duration,
    /// The most of an answer that is read.
    pub(crate) most: usize,
    /// For an answer that streams: whether what was read so far holds all
    /// that was asked for, so the rest need not be waited for.
    pub(crate) whole: Option<Whole>,
}

/// Whether what was read of a streamed answer holds all that was asked for,
/// told the answer's `content-type` (empty when absent) and each arriving chunk.
pub(crate) type Whole = Box<dyn FnMut(&str, &[u8]) -> bool + Send>;

/// What a service answered.
#[derive(Debug)]
pub(crate) struct Sent {
    pub(crate) status: u16,
    /// The answer's headers this server reads, by their lowercase names.
    headers: Vec<(&'static str, String)>,
    pub(crate) body: Vec<u8>,
}

impl Sent {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(kept, _)| *kept == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The headers of an answer that are read.
const READ_HEADERS: [&str; 3] = ["content-type", "mcp-session-id", "www-authenticate"];

type Sending = Pin<Box<dyn Future<Output = Result<Sent, Problem>> + Send>>;

/// The guarded client for one piece of work with one service.
pub(crate) struct Wire {
    send: Box<dyn Fn(Ask) -> Sending + Send + Sync>,
}

impl Wire {
    /// A client that reaches what `reach` lets it and nothing else.
    pub(crate) fn new(reach: &Reach) -> Result<Self, Problem> {
        let destinations = reach.destinations;
        let client = guarded_client(destinations, Arc::clone(&reach.lookup)).map_err(|error| {
            tracing::warn!(%error, "no client to ask a connected service with");
            Problem::NoAnswer
        })?;
        Ok(Self {
            send: Box::new(move |mut ask| {
                let client = client.clone();
                Box::pin(async move {
                    allowed(&ask.url, destinations)?;
                    let mut request = match ask.post {
                        Some(body) => client.post(ask.url).body(body),
                        None => client.get(ask.url),
                    };
                    for (name, value) in ask.headers {
                        request = request.header(name, value);
                    }
                    // The address stays out of the log, and so does what the
                    // error quotes of it.
                    let mut response = request.timeout(ask.wait).send().await.map_err(|error| {
                        if refused_address(&error) {
                            return Problem::NotCalled;
                        }
                        let error = error.without_url();
                        tracing::info!(%error, "a connected service did not answer");
                        Problem::NoAnswer
                    })?;
                    let status = response.status().as_u16();
                    if (300..400).contains(&status) {
                        return Err(Problem::SentOn);
                    }
                    let headers: Vec<(&'static str, String)> = READ_HEADERS
                        .iter()
                        .filter_map(|name| {
                            let value = response.headers().get(*name)?.to_str().ok()?;
                            Some((*name, value.to_owned()))
                        })
                        .collect();
                    let kind = headers
                        .iter()
                        .find(|(kept, _)| *kept == "content-type")
                        .map_or("", |(_, value)| value.as_str());
                    let mut body = Vec::new();
                    while let Some(chunk) = response.chunk().await.map_err(|error| {
                        let error = error.without_url();
                        tracing::info!(%error, "a connected service's answer broke off");
                        Problem::NoAnswer
                    })? {
                        if body.len().saturating_add(chunk.len()) > ask.most {
                            return Err(Problem::Unreadable);
                        }
                        body.extend_from_slice(&chunk);
                        if ask.whole.as_mut().is_some_and(|whole| whole(kind, &chunk)) {
                            break;
                        }
                    }
                    Ok(Sent {
                        status,
                        headers,
                        body,
                    })
                })
            }),
        })
    }

    /// Send `ask` and read its answer.
    pub(crate) async fn send(&self, ask: Ask) -> Result<Sent, Problem> {
        (self.send)(ask).await
    }
}

/// Whether a request may be sent to `url`: an `https` address, or a plain
/// one only on the server's own network where its operator lets it call
/// there, on a port the web uses, with no sign-in in it.
fn allowed(url: &Url, destinations: Destinations) -> Result<(), Problem> {
    let plain = url.scheme() != "https";
    if (plain && !(destinations == Destinations::Any && is_non_public_literal(url)))
        || !url.username().is_empty()
        || url.password().is_some()
        || check_destination(url, destinations).is_err()
    {
        return Err(Problem::NotCalled);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use test_http::{Reply, Responder};

    use super::*;

    fn ask(url: &str) -> Ask {
        Ask {
            url: Url::parse(url).unwrap(),
            post: Some(b"{}".to_vec()),
            headers: Vec::new(),
            wait: Duration::from_secs(10),
            most: 1024,
            whole: None,
        }
    }

    fn any() -> Reach {
        Reach::of(true)
    }

    /// A lookup that answers `addresses` for every name and counts how
    /// often it was asked.
    fn resolving(addresses: Vec<IpAddr>) -> (Lookup, Arc<AtomicUsize>) {
        let asked = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&asked);
        let lookup: Lookup = Arc::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            let addresses = addresses.clone();
            Box::pin(async move { Ok(addresses) })
        });
        (lookup, asked)
    }

    /// Nothing is sent to an address on the server's own network, written as
    /// a literal or behind a name, and nothing to a plain address.
    #[tokio::test]
    async fn nothing_is_sent_into_the_servers_own_network() {
        let inside = Responder::handle(|_| Some(Reply::status(200).body("{}")));
        let port = inside.port();
        let (to_loopback, asked) = resolving(vec![IpAddr::from([127, 0, 0, 1])]);
        let public = Reach {
            destinations: Destinations::Public,
            lookup: to_loopback,
        };
        let wire = Wire::new(&public).unwrap();
        for address in [
            format!("http://127.0.0.1:{port}/mcp"),
            format!("https://127.0.0.1:{port}/mcp"),
            format!("https://[::1]:{port}/mcp"),
            format!("https://10.0.0.7:{port}/mcp"),
            format!("https://169.254.169.254:{port}/latest/meta-data"),
            format!("http://service.example:{port}/mcp"),
            format!("https://ada:secret@service.example:{port}/mcp"),
            "https://service.example:25/mcp".to_owned(),
            "blob:http://127.0.0.1/x".to_owned(),
            "redis://127.0.0.1/".to_owned(),
            "file://127.0.0.1/etc/passwd".to_owned(),
        ] {
            assert_eq!(
                wire.send(ask(&address)).await.unwrap_err(),
                Problem::NotCalled,
                "{address}"
            );
        }
        // None of those was looked up: each was refused as written.
        assert_eq!(asked.load(Ordering::SeqCst), 0);

        // A name is looked up once, by the connection itself, and what it
        // resolves to decides: inside is not dialled.
        let named = format!("https://service.example:{port}/mcp");
        assert_eq!(
            wire.send(ask(&named)).await.unwrap_err(),
            Problem::NotCalled
        );
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        // One address inside among several outside refuses the name whole.
        let (mixed, _) = resolving(vec![
            IpAddr::from([93, 184, 216, 34]),
            IpAddr::from([192, 168, 1, 10]),
        ]);
        let wire = Wire::new(&Reach {
            destinations: Destinations::Public,
            lookup: mixed,
        })
        .unwrap();
        assert_eq!(
            wire.send(ask(&named)).await.unwrap_err(),
            Problem::NotCalled
        );
        assert!(
            inside.drain_requests().is_empty(),
            "something was sent inside"
        );
    }

    /// What was called while the operator let the server call its own
    /// network is not called once that leave is gone: nothing is remembered
    /// from an earlier check. The address is refused as written, and a name
    /// that resolves to it is refused at each call's own lookup, the one
    /// its connection would dial, so there is no moment between a check and
    /// a call for an address to change in.
    #[tokio::test]
    async fn what_was_let_once_is_checked_again_at_every_call() {
        let inside = Responder::handle(|_| Some(Reply::status(200).body("{}")));
        let literal = format!("http://127.0.0.1:{}/mcp", inside.port());
        let let_in = Wire::new(&any()).unwrap();
        assert_eq!(let_in.send(ask(&literal)).await.unwrap().status, 200);
        assert_eq!(inside.drain_requests().len(), 1);
        // Even while let, a plain request goes to a literal address inside
        // and to no name: a name could be anywhere. Non-http(s) schemes that
        // carry a literal address are refused too.
        for refused in [
            format!("http://service.example:{}/mcp", inside.port()),
            "blob:http://127.0.0.1/x".to_owned(),
            "redis://127.0.0.1/".to_owned(),
            "file://127.0.0.1/etc/passwd".to_owned(),
        ] {
            assert_eq!(
                let_in.send(ask(&refused)).await.unwrap_err(),
                Problem::NotCalled,
                "{refused}"
            );
        }

        let (to_loopback, asked) = resolving(vec![IpAddr::from([127, 0, 0, 1])]);
        let wire = Wire::new(&Reach {
            destinations: Destinations::Public,
            lookup: to_loopback,
        })
        .unwrap();
        assert_eq!(
            wire.send(ask(&literal)).await.unwrap_err(),
            Problem::NotCalled
        );
        let named = format!("https://service.example:{}/mcp", inside.port());
        for call in 1..=3 {
            assert_eq!(
                wire.send(ask(&named)).await.unwrap_err(),
                Problem::NotCalled
            );
            assert!(
                asked.load(Ordering::SeqCst) >= call,
                "call {call} did not look the name up itself"
            );
        }
        assert!(inside.drain_requests().is_empty(), "the name was dialled");
    }

    /// A redirect is an answer, never a second request: the place it names
    /// is not asked, wherever that is.
    #[tokio::test]
    async fn a_redirect_is_not_followed() {
        let elsewhere = Responder::handle(|_| Some(Reply::status(200).body("{}")));
        let target = format!("{}/internal", elsewhere.url());
        for status in [301, 302, 303, 307, 308] {
            let to = target.clone();
            let service =
                Responder::handle(move |_| Some(Reply::status(status).header("Location", &to)));
            let wire = Wire::new(&any()).unwrap();
            let sent = wire.send(ask(&format!("{}/mcp", service.url()))).await;
            assert_eq!(sent.unwrap_err(), Problem::SentOn, "{status}");
            assert_eq!(service.drain_requests().len(), 1);
        }
        assert!(
            elsewhere.drain_requests().is_empty(),
            "a redirect was followed"
        );
    }

    /// An answer is read up to its bound and no further, the headers that
    /// are read are answered by name, and a service that goes away is one
    /// that did not answer.
    #[tokio::test]
    async fn an_answer_is_bounded_and_a_service_that_goes_away_did_not_answer() {
        let service = Responder::handle(|request| match request.target.as_str() {
            "/large" => Some(Reply::status(200).body(vec![b'a'; 4096])),
            "/gone" => None,
            _ => Some(
                Reply::status(401)
                    .header("WWW-Authenticate", "Bearer realm=\"orchard\"")
                    .header("Content-Type", "application/json")
                    .body("{}"),
            ),
        });
        let wire = Wire::new(&any()).unwrap();
        let large = wire.send(ask(&format!("{}/large", service.url()))).await;
        assert_eq!(large.unwrap_err(), Problem::Unreadable);
        let gone = wire.send(ask(&format!("{}/gone", service.url()))).await;
        assert_eq!(gone.unwrap_err(), Problem::NoAnswer);
        let turned = wire
            .send(ask(&format!("{}/mcp", service.url())))
            .await
            .unwrap();
        assert_eq!(turned.status, 401);
        assert_eq!(
            turned.header("www-authenticate"),
            Some("Bearer realm=\"orchard\"")
        );
        assert_eq!(turned.header("content-type"), Some("application/json"));
        assert_eq!(turned.header("mcp-session-id"), None);
        // A listener nobody answers at.
        let closed = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let nobody = wire
            .send(ask(&format!("http://127.0.0.1:{closed}/mcp")))
            .await;
        assert_eq!(nobody.unwrap_err(), Problem::NoAnswer);
    }
}

//! HTTP on the desktop: reqwest over rustls on the system trust bundle.

use bytes::Bytes;
use url::Url;

use super::chain;
use crate::client::{Config, Lookup, RawResponse, Redirects, RefusedLookup};
use crate::{NetError, Request};

pub(crate) struct Backend {
    client: reqwest::Client,
}

impl Backend {
    pub(crate) fn new(config: &Config) -> Result<Self, NetError> {
        let mut builder =
            crate::http::client()
                .gzip(true)
                .brotli(true)
                .redirect(match config.redirects {
                    Redirects::Follow(limit) => {
                        reqwest::redirect::Policy::limited(usize::from(limit))
                    }
                    Redirects::None => reqwest::redirect::Policy::none(),
                });
        if let Some(tls) = config.tls.clone() {
            builder = builder.tls_backend_preconfigured(tls);
        }
        if let Some(timeout) = config.connect_timeout {
            builder = builder.connect_timeout(timeout);
        }
        if let Some(looking) = config.lookup.clone() {
            builder = builder.dns_resolver(std::sync::Arc::new(Looked(looking.0)));
        }
        if config.direct {
            builder = builder.no_proxy();
        }
        let client = builder.build().map_err(|error| {
            // Built without authorities, it failed for this program's own file.
            let why = super::tls::lacks_authorities().unwrap_or_else(|| chain(&error));
            NetError::InvalidRequest(format!("HTTP client: {why}"))
        })?;
        Ok(Self { client })
    }

    pub(crate) fn send(
        &self,
        _config: &Config,
        request: Request,
    ) -> impl Future<Output = Result<RawResponse, NetError>> + Send + use<> {
        let client = self.client.clone();
        async move {
            let mut outgoing = reqwest::Request::new(request.method, request.url.clone());
            *outgoing.headers_mut() = request.headers;
            *outgoing.timeout_mut() = request.timeout;
            if let Some(body) = request.body {
                *outgoing.body_mut() = Some(reqwest::Body::from(body));
            }
            let response = client
                .execute(outgoing)
                .await
                .map_err(|error| map_error(&error))?;
            let url = Url::parse(response.url().as_str()).unwrap_or(request.url);
            Ok(RawResponse {
                status: response.status(),
                headers: response.headers().clone(),
                url,
                body: Body(response),
            })
        }
    }
}

pub(crate) struct Body(reqwest::Response);

impl Body {
    pub(crate) async fn chunk(&mut self) -> Result<Option<Bytes>, NetError> {
        self.0.chunk().await.map_err(|error| {
            if error.is_timeout() {
                NetError::Timeout
            } else {
                NetError::Body(chain(&error))
            }
        })
    }
}

/// What a failed lookup or a refusal travels as to the connection.
type Failure = Box<dyn std::error::Error + Send + Sync>;

/// The resolver of a client that was given its own lookup
/// ([`crate::ClientBuilder::lookup`]): one lookup for a connection, and the
/// addresses it answered are the ones dialled.
struct Looked(Lookup);

impl reqwest::dns::Resolve for Looked {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let lookup = std::sync::Arc::clone(&self.0);
        let name = name.as_str().to_owned();
        Box::pin(async move {
            let addresses = match lookup(name.clone()).await {
                Ok(addresses) => addresses,
                // A refusal travels as itself, so the request's error tells
                // it from a lookup that failed.
                Err(error) => {
                    return Err(match error.downcast::<RefusedLookup>() {
                        Ok(refused) => Box::new(refused) as Failure,
                        // The one judge's refusal of a name is a refusal too.
                        Err(error) => match error.downcast::<crate::address::NotPublic>() {
                            Ok(refused) => Box::new(RefusedLookup(refused.to_string())) as Failure,
                            Err(error) => Box::new(error) as Failure,
                        },
                    });
                }
            };
            if addresses.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("{name} does not resolve"),
                )) as Failure);
            }
            let addresses: reqwest::dns::Addrs = Box::new(
                addresses
                    .into_iter()
                    .map(|address| std::net::SocketAddr::new(address, 0)),
            );
            Ok::<reqwest::dns::Addrs, Failure>(addresses)
        })
    }
}

/// What the client's own lookup said when it refused the name a request was
/// for: nothing was sent.
fn refused(error: &(dyn std::error::Error + 'static)) -> Option<String> {
    let mut source = Some(error);
    while let Some(cause) = source {
        if let Some(refused) = cause.downcast_ref::<RefusedLookup>() {
            return Some(refused.0.clone());
        }
        source = cause.source();
    }
    None
}

fn map_error(error: &reqwest::Error) -> NetError {
    if let Some(why) = refused(error) {
        return NetError::Blocked(why);
    }
    if error.is_timeout() {
        NetError::Timeout
    } else if error.is_builder() {
        NetError::InvalidRequest(chain(error))
    } else if error.is_body() || error.is_decode() {
        NetError::Body(chain(error))
    } else {
        NetError::connect(chain(error))
    }
}

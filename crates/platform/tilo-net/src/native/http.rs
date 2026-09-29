//! HTTP on the desktop: reqwest over rustls on the system trust bundle.

use bytes::Bytes;
use url::Url;

use super::chain;
use crate::client::{Config, RawResponse, Redirects};
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
        if let Some(timeout) = config.connect_timeout {
            builder = builder.connect_timeout(timeout);
        }
        let client = builder
            .build()
            .map_err(|error| NetError::InvalidRequest(format!("HTTP client: {error}")))?;
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

fn map_error(error: &reqwest::Error) -> NetError {
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

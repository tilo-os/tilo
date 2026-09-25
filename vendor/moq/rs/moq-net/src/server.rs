use crate::origin;
use crate::{
	ALPN_14, ALPN_15, ALPN_16, ALPN_17, ALPN_18, ALPN_19, ALPN_20, ALPN_21, ALPN_LITE, ALPN_LITE_03, ALPN_LITE_04,
	ALPN_LITE_05, ALPN_LITE_06_WIP, Consume, Driver, Error, NEGOTIATED, Role, Session, Version, Versions,
	coding::{Decode, Encode, Stream},
	ietf, lite, setup, stats,
};

/// A MoQ server session builder.
#[derive(Default, Clone)]
pub struct Server {
	publish: Option<origin::Consumer>,
	subscribe: Option<origin::Producer>,
	stats: stats::Session,
	versions: Versions,
}

impl Server {
	/// A server that neither publishes nor subscribes until configured.
	pub fn new() -> Self {
		Default::default()
	}

	/// Publish to the connected client: the session reads from the given origin
	/// (pass an [`origin::Producer`] or [`origin::Consumer`] by reference) and forwards
	/// its announcements. Omit to publish nothing. Pre-scoped via
	/// [`origin::Producer::scope`] for token-gated relays.
	pub fn with_publisher(mut self, publish: impl Consume<origin::Consumer>) -> Self {
		self.publish = Some(publish.consume());
		self
	}

	/// Subscribe to the connected client: the session writes the broadcasts the
	/// client announces into this [`origin::Producer`]. Omit to subscribe to nothing.
	pub fn with_subscriber(mut self, subscribe: origin::Producer) -> Self {
		self.subscribe = Some(subscribe);
		self
	}

	/// Attach a per-connection [`stats::Session`] context. The session's publish
	/// (egress) and subscribe (ingress) origin handles are tagged with it, so all
	/// traffic counters are attributed through the model for this session's lifetime.
	/// Pass [`stats::Session::default`] (a no-op context) to opt out.
	pub fn with_stats(mut self, stats: stats::Session) -> Self {
		self.stats = stats;
		self
	}

	/// Set both publish and subscribe from one shared [`origin::Producer`].
	pub fn with_origin(self, origin: origin::Producer) -> Self {
		self.with_publisher(&origin).with_subscriber(origin)
	}

	/// Restrict which protocol versions to accept, in preference order.
	/// Defaults to every version this crate supports.
	pub fn with_versions(mut self, versions: Versions) -> Self {
		self.versions = versions;
		self
	}

	/// Perform the MoQ handshake as a server, returning the [`Session`] and the
	/// [`Driver`] that runs its protocol work.
	///
	/// Convenience wrapper over [`accept_request`](Self::accept_request) that
	/// completes the handshake immediately. Use `accept_request` when you need to
	/// inspect the client's advertised path before deciding what to serve.
	pub async fn accept<S: web_transport_trait::Session>(&self, session: S) -> Result<(Session, Driver), Error> {
		self.accept_request(session).await?.ok().await
	}

	/// Begin the MoQ handshake, pausing once the client's request path is known so
	/// the caller can authorize/scope before serving.
	///
	/// Reads the client's SETUP (the in-band path lives there on URL-less transports),
	/// then returns a [`Request`]: inspect [`path`](Request::path), set the origins to
	/// serve, and call [`ok`](Request::ok) or [`close`](Request::close). Session start
	/// is deferred to `ok()`, so origins set on the `Request` always take effect.
	///
	/// The path is surfaced for moq-lite-05 and every moq-transport draft we speak;
	/// it's empty on versions with no in-band request path (e.g. lite 01-04).
	pub async fn accept_request<S: web_transport_trait::Session>(&self, session: S) -> Result<Request<S>, Error> {
		// Regimes without a path to read defer to `ok()` without surfacing one, and
		// carry no role or origin hint, so authorization is unchanged for them.
		let deferred = |handshake| Request {
			authorization: Vec::new(),
			path: None,
			role: None,
			origin: None,
			assigned_origin: crate::Origin::random(),
			inner: Some(RequestInner {
				server: self.clone(),
				handshake,
			}),
		};

		let (encoding, supported) = match session.protocol() {
			Some(alpn @ (ALPN_21 | ALPN_20 | ALPN_19 | ALPN_18 | ALPN_17)) => {
				let draft = match alpn {
					ALPN_21 => ietf::Version::Draft21,
					ALPN_20 => ietf::Version::Draft20,
					ALPN_19 => ietf::Version::Draft19,
					ALPN_18 => ietf::Version::Draft18,
					_ => ietf::Version::Draft17,
				};

				self.versions.select(Version::Ietf(draft)).ok_or(Error::Version)?;
				return self.accept_ietf_modern(session, draft).await;
			}
			Some(ALPN_16) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft16))
					.ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(ALPN_15) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft15))
					.ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(ALPN_14) => {
				let v = self
					.versions
					.select(Version::Ietf(ietf::Version::Draft14))
					.ok_or(Error::Version)?;
				(v, v.into())
			}
			Some(alpn @ (ALPN_LITE_05 | ALPN_LITE_06_WIP)) => {
				let version = match alpn {
					ALPN_LITE_06_WIP => lite::Version::Lite06Wip,
					_ => lite::Version::Lite05,
				};
				self.versions.select(Version::Lite(version)).ok_or(Error::Version)?;

				// Gate on the client's SETUP: read it before serving so the caller can
				// scope by the advertised path. Seeded back into `start` on `ok()` so
				// PROBE gating resolves without re-reading the (consumed) Setup Stream.
				let client_setup = lite::accept_setup(&session, version).await?;
				return Ok(Request {
					authorization: Vec::new(),
					path: client_setup.path.clone(),
					role: client_setup.role,
					origin: client_setup.origin,
					assigned_origin: crate::Origin::random(),
					inner: Some(RequestInner {
						server: self.clone(),
						handshake: Handshake::LiteSetup {
							session,
							version,
							client_setup,
						},
					}),
				});
			}
			Some(ALPN_LITE_04) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite04))
					.ok_or(Error::Version)?;
				return Ok(deferred(Handshake::LiteBare {
					session,
					version: lite::Version::Lite04,
				}));
			}
			Some(ALPN_LITE_03) => {
				self.versions
					.select(Version::Lite(lite::Version::Lite03))
					.ok_or(Error::Version)?;
				return Ok(deferred(Handshake::LiteBare {
					session,
					version: lite::Version::Lite03,
				}));
			}
			Some(ALPN_LITE) | None => {
				let supported = self.versions.filter(&NEGOTIATED.into()).ok_or(Error::Version)?;
				(Version::Ietf(ietf::Version::Draft14), supported)
			}
			Some(p) => return Err(Error::UnknownAlpn(p.to_string())),
		};

		// Legacy bidi SETUP exchange (IETF 14-16, lite 01/02). Read the client's
		// SETUP to choose the version; `ok()` sends the server SETUP and starts.
		let mut stream = Stream::accept(&session, encoding).await?;
		let mut client: setup::Client = stream.reader.decode().await?;

		let version = client
			.versions
			.iter()
			.flat_map(|v| Version::try_from(*v).ok())
			.find(|v| supported.contains(v))
			.ok_or(Error::Version)?;

		// Pull the request path and max request ID out now (IETF only) so `ok()`
		// doesn't re-decode the consumed parameters. moq-transport carries the path
		// in its SETUP just like lite-05.
		let (path, request_id_max, peer_declared) = match version {
			Version::Ietf(v) => {
				let params = ietf::Parameters::decode(&mut client.parameters, v)?;
				let path = match params.get_bytes(ietf::ParameterBytes::Path) {
					Some(bytes) => Some(
						std::str::from_utf8(bytes)
							.map_err(|_| Error::Decode(crate::DecodeError::InvalidValue))?
							.to_owned(),
					),
					None => None,
				};
				let request_id_max = params
					.get_varint(ietf::ParameterVarInt::MaxRequestId)
					.map(ietf::RequestId);
				let peer_declared = ietf::peer::Peer {
					solicit: ietf::solicit::from_setup(&params, v)?,
					..Default::default()
				};
				(path, request_id_max, peer_declared)
			}
			Version::Lite(_) => (None, None, ietf::peer::Peer::default()),
		};

		Ok(Request {
			authorization: Vec::new(),
			path,
			role: None,
			origin: None,
			assigned_origin: crate::Origin::random(),
			inner: Some(RequestInner {
				server: self.clone(),
				handshake: Handshake::Legacy {
					session,
					stream,
					version,
					request_id_max,
					peer_declared,
				},
			}),
		})
	}

	/// Read a draft-17/18 client's SETUP (with its request path) off its uni stream,
	/// then pause. `ok()` starts the session and hands the stream back for GOAWAY.
	async fn accept_ietf_modern<S: web_transport_trait::Session>(
		&self,
		session: S,
		version: ietf::Version,
	) -> Result<Request<S>, Error> {
		let setup = if version == ietf::Version::Draft21 {
			ietf::accept_setup_with_authorization(&session, version).await
		} else {
			ietf::accept_setup(&session, version)
				.await
				.map(|setup| (setup, Vec::new()))
		};
		let (peer_setup, authorization) = match setup {
			Ok(setup) => setup,
			Err(err) => {
				// Draft 21 session registry, not moq-lite's internal error numbers.
				let code = match &err {
					Error::Decode(_) => 0x6,
					Error::Unauthorized => 0x2,
					_ => 0x3,
				};
				session.close(code, "invalid setup");
				return Err(err);
			}
		};
		Ok(Request {
			authorization,
			path: peer_setup.path.clone(),
			role: None,
			// A moq-transport peer only has an identity if it negotiated the MoQ
			// Cluster extension and declared a non-zero Hop ID.
			origin: peer_setup
				.declared
				.cluster
				.origin
				.filter(|o| *o != crate::Origin::UNKNOWN),
			assigned_origin: crate::Origin::random(),
			inner: Some(RequestInner {
				server: self.clone(),
				handshake: Handshake::IetfModern {
					session,
					version,
					peer_setup,
				},
			}),
		})
	}
}

/// A paused server-side handshake.
///
/// Returned by [`Server::accept_request`] once the peer's advertised
/// [`path`](Self::path) is known but before the session is granted anything. Set
/// the origins to serve, then call [`ok`](Self::ok) to complete the handshake, or
/// [`close`](Self::close) to reject it. Modeled on the WebTransport `Request` in
/// moq-native.
pub struct Request<S: web_transport_trait::Session> {
	authorization: Vec<crate::AuthorizationToken>,
	path: Option<String>,
	role: Option<Role>,
	origin: Option<crate::Origin>,
	/// The identity this session's routes are stamped with when the peer declares none
	/// on the wire. Fresh per request unless the caller overrides it
	/// ([`Request::with_peer_origin`]).
	assigned_origin: crate::Origin,
	// Taken by `ok`/`close`; `Drop` rejects the handshake if neither ran.
	inner: Option<RequestInner<S>>,
}

/// The parts of a [`Request`] consumed by [`Request::ok`] / [`Request::close`].
struct RequestInner<S: web_transport_trait::Session> {
	server: Server,
	handshake: Handshake<S>,
}

/// The handshake state captured at the pause point. Every variant defers its
/// session start to [`Request::ok`] so origins set on the Request still apply.
enum Handshake<S: web_transport_trait::Session> {
	/// Modern IETF (17/18): the client's SETUP (with its request path) has been read
	/// off its uni stream; `ok()` starts the session, handing that stream back for
	/// GOAWAY monitoring.
	IetfModern {
		session: S,
		version: ietf::Version,
		peer_setup: ietf::PeerSetup<S>,
	},
	/// moq-lite 03/04: no Setup Stream.
	LiteBare { session: S, version: lite::Version },
	/// Legacy IETF (draft 14-16) and lite 01/02: the client SETUP has been read off
	/// the bidi stream (including its request path) but the server SETUP hasn't been
	/// sent. `ok()` finishes it.
	Legacy {
		session: S,
		stream: Stream<S, Version>,
		version: Version,
		request_id_max: Option<ietf::RequestId>,
		/// What the client's SETUP declared, for the options `ok()` acts on.
		peer_declared: ietf::peer::Peer,
	},
	/// moq-lite 05+: the client's Setup Stream has been read. `ok()` starts the
	/// session, seeding the SETUP back so PROBE gating resolves.
	LiteSetup {
		session: S,
		version: lite::Version,
		client_setup: lite::Setup,
	},
}

impl<S: web_transport_trait::Session> Request<S> {
	/// Opaque proof from the standard Draft 21 authorization token, before admission.
	/// Returns a proof only for exactly one Type 0 token. Missing or ambiguous
	/// supported tokens yield `None`; the host must verify before attaching origins.
	pub fn authorization(&self) -> Option<&[u8]> {
		let mut supported = self.authorization.iter().filter(|token| token.token_type() == 0);
		let token = supported.next()?;
		if supported.next().is_some() {
			None
		} else {
			Some(token.value())
		}
	}

	/// All structurally valid standard tokens; no token is trusted until host verification.
	pub fn authorization_tokens(&self) -> &[crate::AuthorizationToken] {
		&self.authorization
	}

	/// The negotiated protocol, available before accepting this request.
	pub fn version(&self) -> Version {
		match &self.inner.as_ref().expect("request already responded").handshake {
			Handshake::IetfModern { version, .. } => (*version).into(),
			Handshake::LiteBare { version, .. } | Handshake::LiteSetup { version, .. } => (*version).into(),
			Handshake::Legacy { version, .. } => *version,
		}
	}

	/// The request path the client advertised in its SETUP.
	///
	/// Empty when the client advertised none: either it sent an empty path, or the
	/// version carries none in-band (lite 01-04). Those mean the same thing, so the
	/// wire distinction isn't surfaced. Populated for moq-lite-05 and every
	/// moq-transport draft we speak. See the note on [`Server::accept_request`].
	pub fn path(&self) -> &str {
		self.path.as_deref().unwrap_or("")
	}

	/// The single [`Role`] the client advertised in its SETUP, or `None` for a
	/// bidirectional session.
	///
	/// Only moq-lite-05 carries a role, so `None` covers three cases that the wire
	/// doesn't distinguish: an older version, a client that omitted the parameter, and a
	/// client that explicitly advertised both directions. All three mean the same thing
	/// (the client may publish and subscribe), so authorize on what the token grants.
	/// See the note on [`Server::accept_request`].
	pub fn role(&self) -> Option<Role> {
		self.role
	}

	/// The origin identity declared by the peer, when the negotiated protocol carries one.
	///
	/// A moq-lite-05+ endpoint declares this when it attaches a publish or subscribe
	/// origin; a `moqt-17`+ endpoint declares it via the MoQ Cluster extension. Older
	/// versions and endpoints without one return `None`.
	///
	/// Self-declared, so treat it as a correlation hint rather than an
	/// authenticated identity: authorize on the token or client certificate.
	pub fn peer_origin(&self) -> Option<crate::Origin> {
		self.origin
	}

	/// Publish to the connected client. Overrides any value from the [`Server`]
	/// builder; typically set after inspecting [`path`](Self::path).
	pub fn with_publisher(mut self, publish: impl Consume<origin::Consumer>) -> Self {
		self.inner_mut().server.publish = Some(publish.consume());
		self
	}

	/// Subscribe to the connected client. Overrides any value from the [`Server`] builder.
	pub fn with_subscriber(mut self, subscribe: origin::Producer) -> Self {
		self.inner_mut().server.subscribe = Some(subscribe);
		self
	}

	/// Assign the identity this peer's routes are attributed to, overriding the fresh
	/// per-session default.
	///
	/// Only for a peer whose identity the server has actually established, such as one
	/// authenticated by mTLS or a token ([`crate::Client::with_peer_origin`] is the
	/// dialing-side equivalent). An identity the peer declares on the wire still wins.
	///
	/// Two sessions given the same origin are treated as one endpoint: routes learned
	/// from either are kept off both, and content arriving on either is interchangeable
	/// with the other's. That is the point when they really are one peer reconnecting or
	/// running redundant links, and a bug otherwise. Derive it from the authenticated
	/// identity, never from something coarser like the remote address.
	pub fn with_peer_origin(mut self, origin: crate::Origin) -> Self {
		self.assigned_origin = origin;
		self
	}

	/// Set the per-connection [`stats::Session`] context. Overrides any value from the
	/// [`Server`] builder.
	pub fn with_stats(mut self, stats: stats::Session) -> Self {
		self.inner_mut().server.stats = stats;
		self
	}

	fn inner_mut(&mut self) -> &mut RequestInner<S> {
		self.inner.as_mut().expect("request already responded")
	}

	/// Accept the session, returning the [`Session`] and the [`Driver`] that runs
	/// its protocol work.
	pub async fn ok(mut self) -> Result<(Session, Driver), Error> {
		let peer_origin = Some(self.assigned_origin);
		let RequestInner { server, handshake } = self.inner.take().expect("request already responded");

		// Tag the origin pair with the stats context so the model attributes reads
		// (egress) and writes (ingress) for this session. One shared context across
		// both halves keeps presence and viewer counts from double-attributing.
		let publish = server.publish.map(|origin| origin.with_stats(server.stats.clone()));
		let subscribe = server.subscribe.map(|origin| origin.with_stats(server.stats.clone()));

		let (session, mut stream, version, request_id_max, peer_declared) = match handshake {
			Handshake::IetfModern {
				session,
				version,
				peer_setup,
			} => {
				// The client's SETUP was read in `accept_request`; hand the stream back
				// for GOAWAY. A server never advertises a path, hence `None`.
				let drain = ietf::Drain::default();
				let protocol = ietf::start_draining(ietf::Config {
					session: session.clone(),
					setup: None,
					request_id_max: None,
					client: false,
					publish,
					subscribe,
					peer_origin,
					// Only the dialing side prices a link.
					cost: None,
					version,
					path: None,
					peer_setup_stream: Some(peer_setup.stream),
					peer_declared: Some(peer_setup.declared),
				}, drain.clone())?;
				tracing::debug!(?version, "connected");
				let (session, driver) = Session::new(session, version.into(), None, protocol);
				return Ok((session.with_drain(drain), driver));
			}
			Handshake::LiteBare { session, version } => {
				let start = lite::start(lite::Config {
					session: session.clone(),
					setup_stream: None,
					publish,
					subscribe,
					peer_origin,
					version,
					our_setup: lite::Setup::default(),
					peer_setup: None,
				})?;
				return Ok(Session::new(
					session,
					version.into(),
					start.recv_bandwidth,
					start.driver,
				));
			}
			Handshake::LiteSetup {
				session,
				version,
				client_setup,
			} => {
				// We report what the transport actually measures; a server never
				// advertises a request Path or Role.
				let our_setup = lite::Setup {
					probe: lite::ProbeLevel::detect(&session),
					path: None,
					role: None,
					// The dialing side prices the link; we charge what its SETUP declared.
					cost: None,
					// Filled by `lite::start` from the attached origin handles.
					origin: None,
				};
				let start = lite::start(lite::Config {
					session: session.clone(),
					setup_stream: None,
					publish,
					subscribe,
					peer_origin,
					version,
					our_setup,
					peer_setup: Some(client_setup),
				})?;
				return Ok(Session::new(
					session,
					version.into(),
					start.recv_bandwidth,
					start.driver,
				));
			}
			Handshake::Legacy {
				session,
				stream,
				version,
				request_id_max,
				peer_declared,
			} => (session, stream, version, request_id_max, peer_declared),
		};

		// Encode parameters using the version-appropriate type.
		let parameters = match version {
			Version::Ietf(v) => {
				let mut parameters = ietf::Parameters::default();
				parameters.set_varint(ietf::ParameterVarInt::MaxRequestId, u32::MAX as u64);
				parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
				ietf::solicit::into_setup(&mut parameters, v);
				parameters.encode_bytes(v)?
			}
			Version::Lite(v) => lite::Parameters::default().encode_bytes(v)?,
		};

		let server_setup = setup::Server {
			version: version.into(),
			parameters,
		};
		stream.writer.encode(&server_setup).await?;

		let (recv_bw, protocol) = match version {
			Version::Lite(v) => {
				let stream = stream.with_version(v);
				// Pre-lite-05: no Setup Stream, so nothing to advertise or seed.
				let start = lite::start(lite::Config {
					session: session.clone(),
					setup_stream: Some(stream),
					publish,
					subscribe,
					peer_origin,
					version: v,
					our_setup: lite::Setup::default(),
					peer_setup: None,
				})?;
				(start.recv_bandwidth, start.driver)
			}
			Version::Ietf(v) => {
				let stream = stream.with_version(v);
				// Draft 14-16: path came in the bidi SETUP, no uni SETUP to hand back.
				let protocol = ietf::start(ietf::Config {
					session: session.clone(),
					setup: Some(stream),
					request_id_max,
					client: false,
					publish,
					subscribe,
					peer_origin,
					cost: None,
					version: v,
					path: None,
					peer_setup_stream: None,
					peer_declared: Some(peer_declared),
				})?;
				(None, protocol)
			}
		};

		Ok(Session::new(session, version, recv_bw, protocol))
	}

	/// Reject the session, closing the transport with `err`'s wire code.
	pub fn close(mut self, err: Error) {
		let inner = self.inner.take().expect("request already responded");
		inner.close(err);
	}
}

impl<S: web_transport_trait::Session> RequestInner<S> {
	fn close(self, err: Error) {
		let session = match self.handshake {
			Handshake::IetfModern {
				session,
				version: ietf::Version::Draft21,
				..
			} => {
				let code = match err {
					Error::Unauthorized => 0x2,
					Error::Decode(_) => 0x6,
					Error::ProtocolViolation => 0x3,
					_ => 0x1,
				};
				session.close(code, &err.to_string());
				return;
			}
			Handshake::IetfModern { session, .. } => session,
			Handshake::LiteBare { session, .. } => session,
			Handshake::Legacy { session, .. } => session,
			Handshake::LiteSetup { session, .. } => session,
		};
		session.close(err.to_code(), &err.to_string());
	}
}

impl<S: web_transport_trait::Session> Drop for Request<S> {
	// A dropped request would otherwise leave the client hanging until its idle
	// timeout: it already sent SETUP and is waiting on a response. Reject loudly.
	fn drop(&mut self) {
		if let Some(inner) = self.inner.take() {
			tracing::warn!("Request dropped without ok() or close(); rejecting the session");
			inner.close(Error::Cancel);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Origin;
	use std::{
		collections::VecDeque,
		sync::{Arc, Mutex},
	};

	use crate::ALPN_LITE_05;
	use bytes::Bytes;

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	#[derive(Debug, Clone, Default)]
	struct FakeError;
	impl std::fmt::Display for FakeError {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			write!(f, "fake transport error")
		}
	}
	impl std::error::Error for FakeError {}
	impl web_transport_trait::Error for FakeError {
		fn session_error(&self) -> Option<(u32, String)> {
			Some((0, "closed".to_string()))
		}
	}

	/// A session that replays a queue of unidirectional streams (each a `Vec<u8>`) in
	/// order from `accept_uni`; everything else is inert.
	#[derive(Clone)]
	struct FakeSession {
		protocol: Option<&'static str>,
		uni: Arc<Mutex<VecDeque<Vec<u8>>>>,
	}

	impl FakeSession {
		fn new(protocol: &'static str, uni: impl IntoIterator<Item = Vec<u8>>) -> Self {
			Self {
				protocol: Some(protocol),
				uni: Arc::new(Mutex::new(uni.into_iter().collect())),
			}
		}
	}

	impl web_transport_trait::Session for FakeSession {
		type SendStream = FakeSend;
		type RecvStream = FakeRecv;
		type Error = FakeError;

		async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
			// Drop the guard before any await so the future stays Send.
			let data = self.uni.lock().unwrap().pop_front();
			match data {
				Some(data) => Ok(FakeRecv { data: data.into() }),
				None => std::future::pending().await,
			}
		}
		async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
			std::future::pending().await
		}
		async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
			std::future::pending().await
		}
		async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
			std::future::pending().await
		}
		fn send_datagram(&self, _payload: Bytes) -> Result<(), Self::Error> {
			Ok(())
		}
		async fn recv_datagram(&self) -> Result<Bytes, Self::Error> {
			std::future::pending().await
		}
		fn max_datagram_size(&self) -> usize {
			1200
		}
		fn protocol(&self) -> Option<&str> {
			self.protocol
		}
		fn close(&self, _code: u32, _reason: &str) {}
		async fn closed(&self) -> Self::Error {
			std::future::pending().await
		}
	}

	#[derive(Clone, Default)]
	struct FakeSend;
	impl web_transport_trait::SendStream for FakeSend {
		type Error = FakeError;
		async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
			Ok(buf.len())
		}
		fn set_priority(&mut self, _order: u8) {}
		fn finish(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}
		fn reset(&mut self, _code: u32) {}
		async fn closed(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}
	}

	struct FakeRecv {
		data: VecDeque<u8>,
	}
	impl web_transport_trait::RecvStream for FakeRecv {
		type Error = FakeError;
		async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
			if self.data.is_empty() {
				return Ok(None);
			}
			let size = dst.len().min(self.data.len());
			for slot in dst.iter_mut().take(size) {
				*slot = self.data.pop_front().unwrap();
			}
			Ok(Some(size))
		}
		fn stop(&mut self, _code: u32) {}
		async fn closed(&mut self) -> Result<(), Self::Error> {
			Ok(())
		}
	}

	/// Encode a lite-05 Setup Stream: the `DataType::Setup` tag then the SETUP message.
	fn lite05_setup(path: Option<&str>, role: Option<Role>, origin: Option<Origin>) -> Vec<u8> {
		let v = lite::Version::Lite05;
		let mut buf = Vec::new();
		lite::DataType::Setup.encode(&mut buf, v).unwrap();
		lite::Setup {
			probe: lite::ProbeLevel::None,
			path: path.map(str::to_string),
			role,
			cost: None,
			origin,
		}
		.encode(&mut buf, v)
		.unwrap();
		buf
	}

	/// Encode a draft-17+ Setup Stream: the unified SETUP message, whose parameters
	/// carry the request path the same way lite-05's does.
	fn ietf_setup(version: ietf::Version, path: Option<&str>) -> Vec<u8> {
		let mut params = ietf::Parameters::default();
		if let Some(path) = path {
			params.set_bytes(ietf::ParameterBytes::Path, path.as_bytes().to_vec());
		}
		let parameters = params.encode_bytes(version).unwrap();

		let mut buf = Vec::new();
		setup::Setup { parameters }
			.encode(&mut buf, crate::Version::Ietf(version))
			.unwrap();
		buf
	}

	// These fixtures exercise the real parameter and SETUP parsers, while the
	// transport supplies bytes without opening a UDP socket.
	fn authorization_setup(parameters: Vec<u8>) -> Vec<u8> {
		let mut wire = Vec::new();
		setup::Setup {
			parameters: parameters.into(),
		}
		.encode(&mut wire, crate::Version::Ietf(ietf::Version::Draft21))
		.unwrap();
		wire
	}

	#[tokio::test]
	async fn review_setup_unknown_duplicates_and_multiple_tokens_reach_admission() {
		for options in [vec![13, 1, 10, 0, 1, 20], vec![12, 1, 0, 2]] {
			let request = Server::new()
				.accept_request(FakeSession::new(ALPN_21, [authorization_setup(options)]))
				.await
				.unwrap();
			assert!(request.authorization().is_none());
		}
		let request = Server::new()
			.accept_request(FakeSession::new(
				ALPN_21,
				[authorization_setup(vec![3, 3, 3, 1, 42, 0, 4, 3, 0, 104, 105])],
			))
			.await
			.unwrap();
		assert_eq!(request.authorization(), Some(b"hi".as_slice()));
		let request = Server::new()
			.accept_request(FakeSession::new(
				ALPN_21,
				[authorization_setup(vec![3, 3, 3, 0, 1, 0, 3, 3, 0, 2])],
			))
			.await
			.unwrap();
		assert!(
			request.authorization().is_none(),
			"multiple supported proofs require host denial"
		);
	}

	fn review_token_options(tokens: &[(u64, Vec<u8>)]) -> Vec<u8> {
		let version = ietf::Version::Draft21;
		let mut params = Vec::new();
		for (index, (kind, value)) in tokens.iter().enumerate() {
			(if index == 0 { 3u64 } else { 0 })
				.encode(&mut params, version)
				.unwrap();
			let mut token = vec![3];
			kind.encode(&mut token, version).unwrap();
			token.extend(value);
			token.encode(&mut params, version).unwrap();
		}
		params
	}

	#[tokio::test]
	async fn review_unknown_token_does_not_authorize_and_mixed_tokens_round_trip() {
		let request = Server::new()
			.accept_request(FakeSession::new(ALPN_21, [authorization_setup(vec![3, 3, 3, 42, 99])]))
			.await
			.unwrap();
		assert!(request.authorization().is_none());
		assert_eq!(request.authorization_tokens()[0].token_type(), 42);
		assert_eq!(request.authorization_tokens()[0].value(), &[99]);
		let options = review_token_options(&[(0, b"proof".to_vec()), (1, b"unknown".to_vec())]);
		let parsed = ietf::Parameters::decode(&mut options.as_slice(), ietf::Version::Draft21).unwrap();
		assert_eq!(parsed.encode_bytes(ietf::Version::Draft21).unwrap().as_ref(), options);
	}

	#[tokio::test]
	async fn review_token_count_aggregate_and_structural_boundaries() {
		let tokens: Vec<_> = (0..16).map(|kind| (kind, vec![42; 512])).collect();
		let request = Server::new()
			.accept_request(FakeSession::new(
				ALPN_21,
				[authorization_setup(review_token_options(&tokens))],
			))
			.await
			.unwrap();
		assert_eq!(request.authorization_tokens().len(), 16);
		assert_eq!(request.authorization().unwrap().len(), 512);
		assert_eq!(request.authorization_tokens()[1].token_type(), 1);
		assert_eq!(request.authorization_tokens()[1].value(), &[42; 512]);
		assert!(!format!("{:?}", request.authorization_tokens()[0]).contains("42, 42"));
		let mut oversized = tokens;
		oversized[15].1.push(42);
		assert!(matches!(
			Server::new()
				.accept_request(FakeSession::new(
					ALPN_21,
					[authorization_setup(review_token_options(&oversized))]
				))
				.await,
			Err(Error::Decode(crate::DecodeError::MessageTooLarge { .. }))
		));
		let too_many: Vec<_> = (0..17).map(|kind| (kind, vec![])).collect();
		assert!(matches!(
			Server::new()
				.accept_request(FakeSession::new(
					ALPN_21,
					[authorization_setup(review_token_options(&too_many))]
				))
				.await,
			Err(Error::Decode(crate::DecodeError::TooMany))
		));
		for malformed in [vec![3, 3, 3, 0, 1, 0, 1, 3], vec![4, 0, 0, 0]] {
			assert!(
				Server::new()
					.accept_request(FakeSession::new(ALPN_21, [authorization_setup(malformed)]))
					.await
					.is_err()
			);
		}
	}

	#[tokio::test(start_paused = true)]
	async fn authorization_draft21_visible_before_origins_and_version_unchanged() {
		// Independent golden SETUP option fixture: option 3, length 4, USE_VALUE,
		// Type 0, then the literal opaque value. No token-value length prefix.
		let session = FakeSession::new(ALPN_21, [authorization_setup(vec![3, 4, 3, 0, 0x68, 0x69])]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.authorization(), Some(b"hi".as_slice()));
		assert_eq!(request.version().alpn(), "moqt-21");
		let server = &request.inner.as_ref().unwrap().server;
		assert!(server.publish.is_none());
		assert!(server.subscribe.is_none());
		request.close(Error::Unauthorized);
	}

	#[tokio::test(start_paused = true)]
	async fn authorization_draft21_missing_is_rejectable_before_origins() {
		let session = FakeSession::new(ALPN_21, [authorization_setup(vec![])]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.authorization(), None);
		assert!(request.inner.as_ref().unwrap().server.publish.is_none());
		assert!(request.inner.as_ref().unwrap().server.subscribe.is_none());
		request.close(Error::Unauthorized);
	}

	#[tokio::test(start_paused = true)]
	async fn authorization_draft21_boundary_preserves_all_bytes() {
		let payload: Vec<u8> = (0..8192).map(|i| (i % 256) as u8).collect();
		let mut token = vec![3, 0];
		token.extend_from_slice(&payload);
		let mut params = ietf::Parameters::default();
		params.set_bytes(ietf::ParameterBytes::AuthorizationToken, token);
		let wire = authorization_setup(params.encode_bytes(ietf::Version::Draft21).unwrap().to_vec());
		let request = Server::new()
			.accept_request(FakeSession::new(ALPN_21, [wire]))
			.await
			.unwrap();
		assert_eq!(request.authorization(), Some(payload.as_slice()));
	}

	#[tokio::test(start_paused = true)]
	async fn authorization_draft21_duplicate_option_is_ambiguous_for_host() {
		let wire = authorization_setup(vec![3, 4, 3, 0, 104, 105, 0, 4, 3, 0, 104, 105]);
		let request = Server::new()
			.accept_request(FakeSession::new(ALPN_21, [wire]))
			.await
			.unwrap();
		assert_eq!(request.authorization_tokens().len(), 2);
		assert!(request.authorization().is_none());
	}

	#[tokio::test(start_paused = true)]
	async fn authorization_draft21_rejects_malformed_and_forbidden_alias() {
		for token in [vec![2, 0], vec![99], vec![]] {
			let mut parameters = vec![3, token.len() as u8];
			parameters.extend(token);
			let wire = authorization_setup(parameters);
			assert!(
				Server::new()
					.accept_request(FakeSession::new(ALPN_21, [wire]))
					.await
					.is_err()
			);
		}
	}

	#[tokio::test(start_paused = true)]
	async fn authorization_draft21_alias_register_uses_value_with_zero_cache() {
		let wire = authorization_setup(vec![3, 5, 1, 9, 0, 0x68, 0x69]);
		let request = Server::new()
			.accept_request(FakeSession::new(ALPN_21, [wire]))
			.await
			.unwrap();
		assert_eq!(request.authorization(), Some(b"hi".as_slice()));
	}

	#[tokio::test(start_paused = true)]
	async fn authorization_draft21_alias_forbidden_setup_operations_are_protocol_errors() {
		for alias_type in [0, 2] {
			let wire = authorization_setup(vec![3, 2, alias_type, 9]);
			assert!(matches!(
				Server::new().accept_request(FakeSession::new(ALPN_21, [wire])).await,
				Err(Error::ProtocolViolation)
			));
		}
	}

	#[tokio::test(start_paused = true)]
	async fn authorization_draft21_client_emits_standard_option() {
		use crate::lite::test_transport::{Log, SinkSession};
		use futures::FutureExt;
		let log = Log::default();
		let transport = SinkSession::new(log.clone()).with_protocol(ALPN_21);
		let client = crate::Client::new().with_authorization(b"hi".to_vec()).unwrap();
		let (session, driver) = client.connect(transport).await.unwrap();
		let mut driver = std::pin::pin!(driver);
		assert!(driver.as_mut().now_or_never().is_none());
		let writes = log.writes.lock().unwrap().clone();
		// 0x2f00 is a leading-ones varint, followed by a 16-bit body length.
		assert_eq!(&writes[..2], &[0xaf, 0]);
		assert_eq!(&writes[4..10], &[3, 4, 3, 0, 0x68, 0x69]);
		assert_eq!(session.version().alpn(), ALPN_21);
		assert_eq!(log.bi_opens(), 0, "no requests before peer SETUP");
	}

	#[tokio::test(start_paused = true)]
	async fn authorization_draft21_client_rejects_oversize_and_fallback() {
		use crate::lite::test_transport::{Log, SinkSession};
		assert!(crate::Client::new().with_authorization(vec![0; 8193]).is_err());
		let client = crate::Client::new().with_authorization(vec![0; 8192]).unwrap();
		for alpn in [ALPN_20, ALPN_LITE_05] {
			let log = Log::default();
			let transport = SinkSession::new(log.clone()).with_protocol(alpn);
			assert!(matches!(client.connect(transport).await, Err(Error::Version)));
			assert!(log.writes.lock().unwrap().is_empty());
		}
	}

	#[tokio::test(start_paused = true)]
	async fn authorization_draft21_rejects_malformed_before_admission() {
		// Option=3, length=1, USE_VALUE=3, but no Token Type.
		let session = FakeSession::new(ALPN_21, [authorization_setup(vec![3, 1, 3])]);
		assert!(Server::new().accept_request(session).await.is_err());
	}

	#[tokio::test(start_paused = true)]
	async fn authorization_draft21_rejects_oversized_before_admission() {
		let mut value = vec![3, 0]; // USE_VALUE, out-of-band token type.
		value.extend(vec![0xa5; 8193]);
		let mut params = ietf::Parameters::default();
		params.set_bytes(ietf::ParameterBytes::AuthorizationToken, value);
		let wire = authorization_setup(params.encode_bytes(ietf::Version::Draft21).unwrap().to_vec());
		let session = FakeSession::new(ALPN_21, [wire]);
		assert!(Server::new().accept_request(session).await.is_err());
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_reads_ietf_path() {
		// Every draft-17+ version gates on the SETUP stream before starting, so the
		// path is known at authorization time just like lite-05.
		for (alpn, version) in [
			(ALPN_17, ietf::Version::Draft17),
			(ALPN_18, ietf::Version::Draft18),
			(ALPN_19, ietf::Version::Draft19),
		] {
			let session = FakeSession::new(alpn, [ietf_setup(version, Some("/team/room"))]);
			let request = Server::new().accept_request(session).await.unwrap();
			assert_eq!(request.path(), "/team/room", "{alpn}");
		}
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_ietf_without_path_is_empty() {
		let session = FakeSession::new(ALPN_19, [ietf_setup(ietf::Version::Draft19, None)]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_ietf_empty_path_is_accepted() {
		let session = FakeSession::new(ALPN_19, [ietf_setup(ietf::Version::Draft19, Some(""))]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	/// Encode a lite-05 GROUP uni stream header (just the `DataType::Group` tag).
	fn lite05_group() -> Vec<u8> {
		let mut buf = Vec::new();
		lite::DataType::Group.encode(&mut buf, lite::Version::Lite05).unwrap();
		buf
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_reads_lite05_path() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some("/team/room"), None, None)]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.path(), "/team/room");
		assert_eq!(request.role(), None, "a client that omits the role is bidirectional");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_lite05_without_path_is_empty() {
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, None)]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_lite05_empty_path_is_accepted() {
		// An empty path is valid on the wire and means the same as omitting it, so a
		// client that wants the root doesn't have to special-case the parameter.
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(Some(""), None, None)]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.path(), "");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_reads_lite05_role() {
		let session = FakeSession::new(
			ALPN_LITE_05,
			[lite05_setup(Some("/team/room"), Some(Role::Publisher), None)],
		);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.role(), Some(Role::Publisher));
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_skips_uni_stream_before_setup() {
		// A GROUP racing ahead of the SETUP is STOP_SENDING-ed and skipped; the gate
		// keeps reading until it finds the SETUP.
		let session = FakeSession::new(
			ALPN_LITE_05,
			[lite05_group(), lite05_setup(Some("/team/room"), None, None)],
		);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.path(), "/team/room");
	}

	#[tokio::test(start_paused = true)]
	async fn accept_request_reads_lite05_peer_origin() {
		let origin = Origin::new(42).unwrap();
		let session = FakeSession::new(ALPN_LITE_05, [lite05_setup(None, None, Some(origin))]);
		let request = Server::new().accept_request(session).await.unwrap();
		assert_eq!(request.peer_origin(), Some(origin));
	}

	#[tokio::test(start_paused = true)]
	async fn anonymous_peer_origin_filters_routes_from_server_session() {
		let other = Origin::new(778).unwrap();
		let origin = crate::origin::Info::new(Origin::new(1).unwrap()).produce();

		let gate = kio::Producer::new(true);
		let transport = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = transport.log.clone();
		let version = ietf::Version::Draft18;
		let request = Request {
			authorization: Vec::new(),
			path: None,
			role: None,
			origin: None,
			assigned_origin: Origin::random(),
			inner: Some(RequestInner {
				server: Server::new().with_publisher(&origin),
				handshake: Handshake::IetfModern {
					session: transport,
					version,
					peer_setup: ietf::PeerSetup {
						stream: crate::coding::Reader::new(
							crate::lite::test_transport::PendingRecv,
							Version::Ietf(version),
						),
						path: None,
						declared: ietf::peer::Peer::default(),
					},
				},
			}),
		};
		let assigned = request.assigned_origin;

		let mut echoed_hops = crate::OriginList::new();
		echoed_hops.push(assigned).unwrap();
		let _echoed = origin
			.create_broadcast(
				"echoed-route",
				crate::broadcast::Route::new()
					.with_hops(echoed_hops)
					.with_announce(true),
			)
			.unwrap();

		let mut local_hops = crate::OriginList::new();
		local_hops.push(other).unwrap();
		let _local = origin
			.create_broadcast(
				"local-route",
				crate::broadcast::Route::new().with_hops(local_hops).with_announce(true),
			)
			.unwrap();

		let (session, driver) = request.ok().await.unwrap();
		let _driver = tokio::spawn(driver);

		for _ in 0..100 {
			if occurrences(&log, b"local-route") > 0 {
				break;
			}
			tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		}

		assert_eq!(occurrences(&log, b"echoed-route"), 0);
		assert_eq!(occurrences(&log, b"local-route"), 1);
		drop(session);
	}
}

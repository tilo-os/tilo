use crate::origin;
use crate::{
	Error, Origin,
	coding::{Decode, DecodeError, Encode, Reader, Stream, Writer},
	ietf::{self, FetchHeader, RequestId},
	setup,
	util::{MaybeBoxedExt, MaybeSendBox, TaskSet, err_only},
};

use super::{
	Control, Message, Publisher, Subscriber, Version,
	adapter::ControlStreamAdapter,
	cluster,
	drain::{Drain, Gated},
	peer, solicit,
	subscriber::is_protocol_violation,
};

/// Everything one moq-transport session needs to start.
pub struct Config<S: web_transport_trait::Session> {
	pub session: S,

	/// The bidi SETUP stream (draft-14 through draft-16 only). Draft-17+ passes `None`
	/// and exchanges SETUP on uni streams instead.
	pub setup: Option<Stream<S, Version>>,

	pub request_id_max: Option<RequestId>,

	/// Whether we dialed, which sets the request-id parity.
	pub client: bool,

	/// Traffic stats are attributed through these origin handles: tag them with
	/// `origin::{Consumer, Producer}::with_stats` before calling [`start`].
	pub publish: Option<origin::Consumer>,
	pub subscribe: Option<origin::Producer>,

	/// The origin (hop) id to assign the peer when it declares none itself. See
	/// `Client::with_peer_origin`; a peer that negotiates the MoQ Cluster extension
	/// declares its own, which wins.
	pub peer_origin: Option<Origin>,

	/// What crossing this link costs. Declared in our SETUP (see
	/// [`cluster::RELAY_COST`]) for the peer to charge, and charged locally on what the
	/// peer sends us. `None` declares nothing and charges whatever the peer declared,
	/// falling back to 1; that is what a server accepting a connection passes, since it
	/// has no per-peer configuration of its own.
	///
	/// Only `moqt-17`+ negotiates the MoQ Cluster extension. Earlier drafts carry no
	/// cost at all, so nothing is charged and their routes rank on hop count alone.
	pub cost: Option<u64>,

	pub version: Version,

	/// The request path we advertise in our SETUP (draft-17+ clients on URL-less
	/// transports). A server passes `None`.
	pub path: Option<String>,

	/// The peer's SETUP stream, when it was already read before [`start`] (a draft-17+
	/// server that gated on the client's path via [`accept_setup`]). It becomes the
	/// GOAWAY channel; `None` lets the uni loop read the SETUP itself.
	pub peer_setup_stream: Option<Reader<S::RecvStream, crate::Version>>,

	/// What that pre-read SETUP declared, so the session does not have to parse it
	/// twice. `None` when [`Self::peer_setup_stream`] is.
	pub peer_declared: Option<peer::Peer>,
}

pub fn start<S: web_transport_trait::Session>(
	config: Config<S>,
) -> Result<MaybeSendBox<'static, Result<(), Error>>, Error> {
	start_with_authorization(config, None, None)
}

/// `setup_signal` (local patch) is filled when the peer's SETUP is read, so a client
/// can tell an admitted session (a gated server sends SETUP only after admission)
/// from one whose transport merely opened.
pub(crate) fn start_with_authorization<S: web_transport_trait::Session>(
	config: Config<S>,
	authorization: Option<Vec<u8>>,
	setup_signal: Option<peer::PeerSetup>,
) -> Result<MaybeSendBox<'static, Result<(), Error>>, Error> {
	start_inner(config, authorization, setup_signal, None)
}

/// Local patch: like [`start`], with a [`Drain`] the caller can start to stop all
/// MoQ traffic and send GOAWAY (draft-17+), so the peer closes the session first.
pub(crate) fn start_draining<S: web_transport_trait::Session>(
	config: Config<S>,
	drain: Drain,
) -> Result<MaybeSendBox<'static, Result<(), Error>>, Error> {
	start_inner(config, None, None, Some(drain))
}

fn start_inner<S: web_transport_trait::Session>(
	config: Config<S>,
	authorization: Option<Vec<u8>>,
	setup_signal: Option<peer::PeerSetup>,
	drain: Option<Drain>,
) -> Result<MaybeSendBox<'static, Result<(), Error>>, Error> {
	let Config {
		session,
		setup,
		request_id_max,
		client,
		publish,
		subscribe,
		peer_origin,
		cost,
		version,
		path,
		peer_setup_stream,
		peer_declared,
	} = config;

	let bounded = subscribe.as_ref().is_some_and(|origin| origin.info().limits.is_some())
		|| publish.as_ref().is_some_and(|origin| origin.has_limits());
	let request_budget = if bounded {
		super::budget::Budget::bounded(32)
	} else {
		Default::default()
	};
	let group_budget = if bounded {
		super::budget::Budget::bounded(32)
	} else {
		Default::default()
	};
	let driver = async move {
		// Our own Hop ID, taken from whichever origin the caller actually supplied so
		// every session out of this process stamps the same one and cross-session loop
		// detection works. Read BEFORE the placeholders below: their ids are random and
		// identify nothing, so declaring one would compare incoming paths against an
		// identity no other session shares.
		let self_origin = self_origin(publish.as_ref(), subscribe.as_ref());

		// moq-transport threads concrete origins through the publisher/subscriber.
		// An unset half gets an empty origin: an empty publish origin announces
		// nothing, and an empty subscribe origin issues no SUBSCRIBE_NAMESPACE.
		let publish = publish.unwrap_or_else(|| origin::Producer::empty(Origin::random()).consume());
		let subscribe = subscribe.unwrap_or_else(|| origin::Producer::empty(Origin::random()));

		// What the peer declared in its SETUP. Seeded now when that stream was already
		// read (the legacy handshake, or a gated server accept), and filled by the uni
		// loop otherwise.
		let peer_setup = setup_signal.unwrap_or_default();
		let setup_read = peer_declared.is_some();
		match peer_declared {
			Some(declared) => peer_setup.set(declared),
			// A legacy caller that passed nothing (our own tests, and the lite paths):
			// settle the slot rather than leave the announce loops waiting on a value
			// that is never coming.
			None if !cluster::supported(version) => peer_setup.set(peer::Peer::default()),
			None => {}
		}

		let res = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				let Some(setup) = setup else {
					let err = Error::ProtocolViolation;
					session.close(err.to_code(), "setup stream required");
					return Err(err);
				};
				let control = Control::new(request_id_max, client);
				let adapter = ControlStreamAdapter::new(session.clone(), control.clone(), version);

				let publisher = Publisher::new(
					adapter.clone(),
					publish,
					control.clone(),
					peer_origin,
					peer_setup.clone(),
					version,
				)
				.with_budgets(request_budget.clone(), group_budget.clone());
				let (tasks, mut task_set) = TaskSet::new();
				let subscriber = Subscriber::new(
					adapter.clone(),
					subscribe,
					control,
					peer_origin,
					peer_setup.clone(),
					self_origin,
					cost,
					version,
					tasks,
				)
				.with_budgets(request_budget.clone(), group_budget.clone());

				let dispatch_session = adapter.clone();
				let sub_ns = subscriber.clone();
				let sub_ns_adapter = adapter.clone();

				// Every half only ends the session on error (err_only parks on clean
				// completion); the task set draining is the one clean exit.
				let mut adapter_run = std::pin::pin!(err_only(adapter.run(setup.reader, setup.writer)));
				let mut unis = std::pin::pin!(err_only(run_unis(
					adapter.clone(),
					subscriber.clone(),
					None,
					false,
					version
				)));
				let mut dispatch = std::pin::pin!(err_only(run_dispatch(
					dispatch_session,
					publisher.clone(),
					subscriber.clone(),
					version
				)));
				// Unsolicited PUBLISH_NAMESPACE unless the peer requires solicitation;
				// see `Publisher::run_publish_namespaces`.
				let mut pub_ns_run = std::pin::pin!(err_only(publisher.clone().run_publish_namespaces()));
				// One SUBSCRIBE_NAMESPACE per permitted prefix, like `lite::Subscriber`:
				// the scope is what we may ask for, and it is not the origin's root.
				let mut sub_ns_run = std::pin::pin!(err_only(async {
					let mut prefixes = futures::stream::FuturesUnordered::new();
					for prefix in sub_ns.subscribe_prefixes() {
						let permit = request_budget.acquire()?;
						let mut sub_ns = sub_ns.clone();
						let sub_ns_adapter = sub_ns_adapter.clone();
						prefixes.push(async move {
							let _permit = permit;
							let stream = match version {
								Version::Draft16 => {
									let (send, recv) = sub_ns_adapter.open_native_bi().await?;
									Stream {
										writer: crate::coding::Writer::new(send, version),
										reader: crate::coding::Reader::new(recv, version),
									}
								}
								_ => super::open_control(&sub_ns_adapter, version).await?,
							};
							if let Err(err) = sub_ns.run_subscribe_namespace(stream, prefix).await {
								// The peer breaking the protocol is fatal, and the driver
								// below turns this into the session close the draft wants.
								if is_protocol_violation(&err) {
									return Err(err);
								}
								tracing::warn!(%err, "subscribe_namespace failed, continuing without");
							}
							Ok::<(), Error>(())
						});
					}
					while let Some(result) = futures::StreamExt::next(&mut prefixes).await {
						result?;
					}
					Ok(())
				}));

				kio::wait(|waiter| {
					use std::task::Poll;
					if let Poll::Ready(err) = waiter.poll_future(adapter_run.as_mut()) {
						return Poll::Ready(Err::<(), Error>(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(unis.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(dispatch.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if task_set.poll(waiter).is_ready() {
						return Poll::Ready(Ok(()));
					}
					if let Poll::Ready(err) = waiter.poll_future(sub_ns_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(pub_ns_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					Poll::Pending
				})
				.await
			}
			_ => {
				// Local patch: every MoQ stream and datagram goes through the drain gate;
				// only our SETUP stream (which carries GOAWAY) bypasses it. A session
				// started without a drain has one that never starts.
				let drain = drain.unwrap_or_default();
				let raw = session.clone();
				let session = Gated::new(session.clone(), drain.clone());

				// Send SETUP and keep the stream alive for GOAWAY.
				let setup = {
					let session = raw.clone();
					async move {
						if let Err(err) = run_setup(session, version, path, self_origin, cost, authorization, drain).await {
							tracing::warn!(%err, "setup send error");
						}
						std::future::pending::<()>().await;
					}
				};

				let control = Control::new(None, client);
				let publisher = Publisher::new(
					session.clone(),
					publish,
					control.clone(),
					peer_origin,
					peer_setup.clone(),
					version,
				)
				.with_budgets(request_budget.clone(), group_budget.clone());
				let (tasks, mut task_set) = TaskSet::new();
				let subscriber = Subscriber::new(
					session.clone(),
					subscribe,
					control,
					peer_origin,
					peer_setup.clone(),
					self_origin,
					cost,
					version,
					tasks,
				)
				.with_budgets(request_budget.clone(), group_budget.clone());

				let sub_ns_session = session.clone();
				let sub_ns = subscriber.clone();

				// When the peer's SETUP was pre-read (a gated server accept), monitor
				// GOAWAY on that stream here; otherwise `run_unis` does it when the SETUP
				// arrives on the wire.
				let goaway = async move {
					match peer_setup_stream {
						Some(reader) => run_goaway(reader.with_version(version), version).await,
						None => std::future::pending().await,
					}
				};

				// Every half only ends the session on error (err_only parks on clean
				// completion); `setup` never resolves (it holds the stream open) and the
				// task set draining is the one clean exit.
				let mut unis = std::pin::pin!(err_only(run_unis(
					session.clone(),
					subscriber.clone(),
					Some(peer_setup.clone()),
					setup_read,
					version
				)));
				let mut dispatch = std::pin::pin!(err_only(run_dispatch(
					session.clone(),
					publisher.clone(),
					subscriber.clone(),
					version
				)));
				let mut goaway = std::pin::pin!(err_only(goaway));
				// Local patch: best-effort OBJECT_DATAGRAM receive; parks when done.
				let mut datagrams = std::pin::pin!(err_only(subscriber.clone().run_datagrams()));
				let mut setup = std::pin::pin!(setup);
				// Unsolicited PUBLISH_NAMESPACE unless the peer requires solicitation;
				// see `Publisher::run_publish_namespaces`.
				let mut pub_ns_run = std::pin::pin!(err_only(publisher.clone().run_publish_namespaces()));
				// One SUBSCRIBE_NAMESPACE per permitted prefix; see the draft-16 arm.
				let mut sub_ns_run = std::pin::pin!(err_only(async {
					let mut prefixes = futures::stream::FuturesUnordered::new();
					for prefix in sub_ns.subscribe_prefixes() {
						let permit = request_budget.acquire()?;
						let mut sub_ns = sub_ns.clone();
						let sub_ns_session = sub_ns_session.clone();
						prefixes.push(async move {
							let _permit = permit;
							let stream = super::open_control(&sub_ns_session, version).await?;
							if let Err(err) = sub_ns.run_subscribe_namespace(stream, prefix).await {
								// The peer breaking the protocol is fatal, and the driver
								// below turns this into the session close the draft wants.
								if is_protocol_violation(&err) {
									return Err(err);
								}
								tracing::warn!(%err, "subscribe_namespace failed, continuing without");
							}
							Ok::<(), Error>(())
						});
					}
					while let Some(result) = futures::StreamExt::next(&mut prefixes).await {
						result?;
					}
					Ok(())
				}));

				kio::wait(|waiter| {
					use std::task::Poll;
					if let Poll::Ready(err) = waiter.poll_future(unis.as_mut()) {
						return Poll::Ready(Err::<(), Error>(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(dispatch.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(goaway.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(datagrams.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if waiter.poll_future(setup.as_mut()).is_ready() {
						return Poll::Ready(Ok(()));
					}
					if task_set.poll(waiter).is_ready() {
						return Poll::Ready(Ok(()));
					}
					if let Poll::Ready(err) = waiter.poll_future(sub_ns_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(pub_ns_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					Poll::Pending
				})
				.await
			}
		};

		match &res {
			Err(Error::Transport(_)) => {
				tracing::info!("session terminated");
				session.close(1, "");
			}
			Err(err) => {
				tracing::warn!(%err, "session error");
				if version == Version::Draft21 {
					session.close(super::error::session_code(err, version), "session error");
				} else {
					session.close(err.to_code(), err.to_string().as_ref());
				}
			}
			_ => {
				tracing::info!("session closed");
				session.close(0, "");
			}
		}

		res
	}
	.maybe_boxed();

	Ok(driver)
}

/// What a peer's SETUP told us, beyond the stream it arrived on.
pub struct PeerSetup<S: web_transport_trait::Session> {
	/// The SETUP stream, which becomes the GOAWAY channel.
	pub stream: Reader<S::RecvStream, crate::Version>,

	/// The request path the peer advertised, for URL-less transports.
	pub path: Option<String>,

	/// The Setup Options it declared (see [`cluster`] and [`solicit`]).
	pub declared: peer::Peer,
}

/// The Hop ID this session declares and detects loops against.
///
/// Both halves of a session share the process's origin identity, so either one names
/// it; the publish half is just the usual one to be set. A session with neither half
/// has no content to route, so a throwaway id is all it can offer.
fn self_origin(publish: Option<&origin::Consumer>, subscribe: Option<&origin::Producer>) -> Origin {
	publish
		.map(|origin| **origin)
		.or_else(|| subscribe.map(|origin| **origin))
		.unwrap_or_else(Origin::random)
}

/// Server (draft-17+): read the peer's SETUP off its uni stream before starting the
/// session, returning that stream plus what it declared.
///
/// Blocks on the peer's Setup Stream; any other uni stream racing ahead of it is
/// `STOP_SENDING`-ed and skipped (group data needs a prior subscribe, so nothing
/// legitimate precedes the SETUP at connect). Pass the returned reader to [`start`]
/// as its `peer_setup_stream` so GOAWAY monitoring continues without re-reading it.
pub async fn accept_setup<S: web_transport_trait::Session>(
	session: &S,
	version: Version,
) -> Result<PeerSetup<S>, Error> {
	accept_setup_with_authorization(session, version)
		.await
		.map(|(setup, _)| setup)
}

pub(crate) async fn accept_setup_with_authorization<S: web_transport_trait::Session>(
	session: &S,
	version: Version,
) -> Result<(PeerSetup<S>, Vec<crate::AuthorizationToken>), Error> {
	let outer_version = crate::Version::Ietf(version);

	loop {
		let recv = session.accept_uni().await.map_err(Error::from_transport)?;
		let mut reader: Reader<S::RecvStream, crate::Version> =
			Reader::new(recv, outer_version).with_decode_limit(super::MAX_SETUP_DECODE_BYTES);

		if reader.decode_peek::<u64>().await? != setup::SETUP_V17 {
			// Not the SETUP (group data this early is unexpected). Reject and keep waiting.
			reader.abort(&Error::UnexpectedStream);
			continue;
		}

		let setup: setup::Setup = reader.decode().await?;
		let mut bytes = setup.parameters.clone();
		let params = ietf::Parameters::decode(&mut bytes, version)?;
		let path = match params.get_bytes(ietf::ParameterBytes::Path) {
			Some(bytes) => Some(
				std::str::from_utf8(bytes)
					.map_err(|_| Error::Decode(crate::DecodeError::InvalidValue))?
					.to_owned(),
			),
			None => None,
		};
		let declared = peer_from_params(&params, version)?;
		let authorization = decode_authorizations(&params, version)?;

		return Ok((
			PeerSetup {
				stream: reader,
				path,
				declared,
			},
			authorization,
		));
	}
}

fn decode_authorizations(params: &ietf::Parameters, version: Version) -> Result<Vec<crate::AuthorizationToken>, Error> {
	let mut authorization = Vec::new();
	let mut remaining = crate::MAX_AUTHORIZATION_BYTES;
	for token in params.authorization_values() {
		let token = crate::authorization::decode(token, version, remaining)?;
		remaining -= token.value().len();
		authorization.push(token);
	}
	Ok(authorization)
}

/// Parse the Setup Options we act on out of a raw SETUP parameter block.
fn decode_peer_setup(parameters: bytes::Bytes, version: Version) -> Result<peer::Peer, Error> {
	let mut bytes = parameters;
	let params = ietf::Parameters::decode(&mut bytes, version)?;
	if version == Version::Draft21 {
		let _ = decode_authorizations(&params, version)?;
	}
	Ok(peer_from_params(&params, version)?)
}

/// The Setup Options we act on, out of an already-decoded parameter block. One place, so
/// a future option reaches both the pre-read accept path and the uni loop.
fn peer_from_params(params: &ietf::Parameters, version: Version) -> Result<peer::Peer, crate::DecodeError> {
	Ok(peer::Peer {
		cluster: cluster::peer_from_setup(params, version)?,
		solicit: solicit::from_setup(params, version)?,
	})
}

/// Send our SETUP on a uni stream and keep it alive for potential GOAWAY.
///
/// `path` is the request path we advertise (clients on URL-less transports); a
/// server passes `None`. `self_origin` and `cost` are the MoQ Cluster options, which
/// declare our identity and (client-only) what this link costs to cross. The MoQ Solicit
/// declaration is unconditional, so it takes no argument.
async fn run_setup<S: web_transport_trait::Session>(
	session: S,
	version: Version,
	path: Option<String>,
	self_origin: Origin,
	cost: Option<u64>,
	authorization: Option<Vec<u8>>,
	drain: Drain,
) -> Result<(), Error> {
	let outer_version = crate::Version::Ietf(version);

	let send = session.open_uni().await.map_err(Error::from_transport)?;
	let mut writer: Writer<S::SendStream, crate::Version> = Writer::new(send, outer_version);

	let mut parameters = ietf::Parameters::default();
	parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
	if let Some(token) = authorization {
		parameters.set_bytes(ietf::ParameterBytes::AuthorizationToken, token);
	}
	if let Some(path) = path {
		parameters.set_bytes(ietf::ParameterBytes::Path, path.into_bytes());
	}
	cluster::peer_into_setup(&mut parameters, self_origin, cost, version);
	solicit::into_setup(&mut parameters, version);
	let parameters = parameters.encode_bytes(version)?;

	writer.encode(&setup::Setup { parameters }).await?;

	// Hold the writer alive until the session closes, or send GOAWAY on a drain
	// (local patch) and keep holding it so the peer can close first.
	let drained = {
		let closed = std::pin::pin!(session.closed());
		let drained = std::pin::pin!(drain.wait());
		match futures::future::select(closed, drained).await {
			futures::future::Either::Left(_) => None,
			futures::future::Either::Right((timeout, _)) => Some(timeout),
		}
	};
	let mut writer = writer.with_version(version);
	if let Some(timeout) = drained {
		let goaway = ietf::GoAway {
			// Empty: the peer reconnects to the current URI, if at all.
			new_session_uri: "".into(),
			timeout,
		};
		match writer.encode_message(&goaway).await {
			Ok(()) => tracing::debug!(timeout, "sent GOAWAY"),
			Err(err) => tracing::debug!(%err, "GOAWAY send failed"),
		}
		session.closed().await;
	}
	writer.finish().ok();

	Ok(())
}

/// Accept incoming uni streams and dispatch each to a handler.
///
/// For v17, this also handles the SETUP stream (0x2F00) and GOAWAY.
/// For v14-16, all uni streams are group data.
async fn run_unis<S: web_transport_trait::Session>(
	session: S,
	subscriber: Subscriber<S>,
	// Where to record the peer's MoQ Cluster options once its SETUP arrives. `None`
	// for draft-14..16, whose SETUP rides the control stream instead.
	peer_setup: Option<peer::PeerSetup>,
	// Whether the peer's SETUP was already consumed before this loop started.
	setup_read: bool,
	version: Version,
) -> Result<(), Error> {
	let outer_version = crate::Version::Ietf(version);
	let mut tasks = TaskSet::owned();
	// A gated server accept already read the peer's one SETUP off its own uni stream,
	// so anything arriving here is a second one.
	let mut seen_setup = setup_read;

	loop {
		let recv = tasks.drive(session.accept_uni()).await.map_err(Error::from_transport)?;
		let permit = subscriber.group_budget.acquire()?;
		let mut reader: Reader<S::RecvStream, crate::Version> =
			Reader::new(recv, outer_version).with_decode_limit(super::MAX_SETUP_DECODE_BYTES);
		// A stream that dies before its type varint is that stream's failure, not the
		// session's. RESET_STREAM is how a peer drops a group, and QUIC does not order
		// the reset behind the data, so one can beat the first byte even of a stream
		// the peer wrote to. Failing the loop here would tear down the whole session
		// over a single stream the peer had already given up on. Only death is
		// tolerated: bytes that arrive and do not parse stay session-fatal.
		let kind: u64 = match tasks.drive(reader.decode_peek()).await {
			Ok(kind) => kind,
			Err(err @ Error::Cancel) | Err(err @ Error::Remote(_)) | Err(err @ Error::Decode(DecodeError::Short)) => {
				tracing::debug!(%err, "dropping uni stream that died before its type");
				continue;
			}
			Err(err) => return Err(err),
		};

		// v17+: SETUP arrives on a uni stream, then becomes the GOAWAY channel.
		// We accept it in the background without blocking; the one thing that does
		// need it (the MoQ Cluster negotiation) waits on `peer_setup` instead, so a
		// slow SETUP delays announcements rather than the whole session.
		if kind == setup::SETUP_V17 {
			// Exactly one SETUP per endpoint. A second would let a peer restate its
			// declared identity mid-session, silently re-attributing every route
			// already built from the first.
			if std::mem::replace(&mut seen_setup, true) {
				return Err(Error::ProtocolViolation);
			}

			let peer_setup = peer_setup.clone();
			let session = session.clone();
			tasks.push(async move {
				let _permit = permit;
				// The negotiation gates the announce and dispatch loops, so a SETUP we
				// cannot read must end the session rather than leave them parked on a
				// slot nothing will ever fill.
				let msg = match reader.decode::<setup::Setup>().await {
					Ok(msg) => msg,
					Err(err) => {
						tracing::warn!(%err, "setup decode error");
						session.close(Error::ProtocolViolation.to_code(), "invalid setup");
						return;
					}
				};

				if let Some(peer_setup) = peer_setup {
					let peer = match decode_peer_setup(msg.parameters, version) {
						Ok(peer) => peer,
						Err(err) => {
							tracing::warn!(%err, "setup parameter decode error");
							session.close(super::error::session_code(&err, version), "invalid setup parameters");
							return;
						}
					};
					peer_setup.set(peer);
				}

				// Monitor for GOAWAY after setup completes.
				match run_goaway(reader.with_version(version), version).await {
					Ok(()) => {}
					// Local patch: `run_goaway` reports a received GOAWAY as Unsupported.
					// Close the session ourselves, as the draft asks and as the JS client
					// does, so a draining server never has to close it first.
					Err(Error::Unsupported) => session.close(0, "going away"),
					Err(err) => tracing::warn!(%err, "goaway error"),
				}
			});

			continue;
		}

		// Poll one child handler for each group stream.
		let mut sub = subscriber.clone();
		let group_session = session.clone();
		tasks.push(async move {
			let _permit = permit;
			let mut reader = reader
				.with_version(version)
				.with_decode_limit(super::MAX_OBJECT_EXTENSION_BYTES + 64);
			if let Err(err) = run_uni_group(&mut sub, &mut reader).await {
				tracing::debug!(%err, "uni stream error");
				// A moq-transport code, not `Error::to_code`'s moq-lite one. A group arriving
				// for an alias we retired is the expected tail of our own cancellation, and
				// moq-lite's cancel encodes to 0, which on this wire means the stream died of
				// an internal failure on our side.
				reader.stop(super::error::to_stream_code(&err));
				if is_protocol_violation(&err) {
					group_session.close(0x3, "invalid media stream");
				}
			}
		});
	}
}

async fn run_uni_group<S: web_transport_trait::Session>(
	subscriber: &mut Subscriber<S>,
	stream: &mut Reader<S::RecvStream, Version>,
) -> Result<(), Error> {
	let kind: u64 = stream.decode_peek().await?;

	// SUBGROUP_HEADER type bytes match the form 0b0XX1XXXX (spec §11.4.2):
	// draft-14-17 use 0x10-0x1D and 0x30-0x3D, draft-18 adds 0x40 (FIRST_OBJECT)
	// extending the form to also cover 0x50-0x5D and 0x70-0x7D. Per-version and
	// per-bit validation (e.g., FIRST_OBJECT must be 0 on draft-17) is done in
	// `GroupFlags::decode`.
	if kind <= 0xff && (kind & 0x90) == 0x10 {
		return subscriber.recv_group(stream).await;
	}

	match kind {
		// A fill fetch stream carries the head of the group a draft-20 subscription joined
		// part way through. One answering no fill of ours is refused inside.
		FetchHeader::TYPE => subscriber.recv_fill(stream).await,
		_ => Err(Error::UnexpectedStream),
	}
}

/// Accept incoming bidi streams and dispatch to the correct handler based on message type.
async fn run_dispatch<S: web_transport_trait::Session>(
	session: S,
	publisher: Publisher<S>,
	mut subscriber: Subscriber<S>,
	version: Version,
) -> Result<(), Error> {
	// PUBLISH_NAMESPACE decodes differently once the MoQ Cluster extension is
	// negotiated, so the whole dispatch loop waits for the peer's SETUP first. The peer
	// must send it before anything else, and `run_unis` reads it independently, so this
	// costs a handshake round rather than blocking.
	let peer = subscriber.peer().await;

	// From the same slot, so this costs nothing extra: it decides whether an unsolicited
	// advertisement is the peer ignoring our own SETUP (MoQ Solicit).
	let declared = subscriber.solicit().await;

	let mut tasks = TaskSet::owned();
	loop {
		let mut stream = tasks.drive(Stream::accept(&session, version)).await?;
		let permit = publisher.request_budget.acquire()?;
		stream.reader = stream.reader.with_decode_limit(super::MAX_CONTROL_DECODE_BYTES);

		let header = tasks
			.drive(async {
				let id: u64 = stream.reader.decode().await?;
				let size: u16 = stream.reader.decode().await?;
				let data = stream
					.reader
					.read_exact_bounded(size as usize, super::control_body_limit(id))
					.await?;
				Ok::<_, Error>((id, data))
			})
			.await;
		// Same tolerance as `run_unis`: a request stream that dies before its header
		// is the peer abandoning that request, not the session. Anything else, a
		// header that does not parse included, still fails the session.
		let (id, data) = match header {
			Ok(header) => header,
			Err(err @ Error::Cancel) | Err(err @ Error::Remote(_)) | Err(err @ Error::Decode(DecodeError::Short)) => {
				tracing::debug!(%err, "dropping bidi stream that died before its header");
				continue;
			}
			Err(err) => return Err(err),
		};

		match id {
			// Publisher handles: Subscribe, Fetch, SubscribeNamespace (0x50 modern /
			// 0x11 legacy), TrackStatus
			ietf::Subscribe::ID
			| ietf::Fetch::ID
			| ietf::SubscribeNamespace::ID
			| ietf::SubscribeNamespaceLegacy::ID
			| ietf::TrackStatus::ID => {
				let task = publisher.handle_stream(id, data, stream)?;
				tasks.push(async move {
					let _permit = permit;
					task.await;
				});
			}
			// Subscriber handles: Publish, PublishNamespace
			ietf::Publish::ID | ietf::PublishNamespace::ID => {
				let task = subscriber.handle_stream(id, data, stream, peer, declared)?;
				tasks.push(async move {
					let _permit = permit;
					task.await;
				});
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type");
				return Err(Error::UnexpectedStream);
			}
		}
	}
}

/// Block until GOAWAY or stream close.
async fn run_goaway<R: web_transport_trait::RecvStream>(
	mut reader: Reader<R, Version>,
	version: Version,
) -> Result<(), Error> {
	reader = reader.with_decode_limit(super::MAX_CONTROL_DECODE_BYTES);
	let id: u64 = match reader.decode_maybe().await? {
		Some(id) => id,
		None => return Ok(()),
	};

	let size: u16 = reader.decode::<u16>().await?;
	let mut data = reader
		.read_exact_bounded(size as usize, super::control_body_limit(id))
		.await?;

	if id == ietf::GoAway::ID {
		let msg = ietf::GoAway::decode_msg(&mut data, version)?;
		tracing::debug!(message = ?msg, "received GOAWAY");
		Err(Error::Unsupported)
	} else {
		Err(Error::UnexpectedMessage)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Supplies infinitely many media streams whose type is known but whose
	/// header never completes. This exercises actual admission before parsing.
	#[derive(Clone)]
	struct PendingGroups(crate::lite::test_transport::ScriptedSession, bool);
	impl web_transport_trait::Session for PendingGroups {
		type SendStream = crate::lite::test_transport::SinkSend;
		type RecvStream = crate::lite::test_transport::ScriptedRecv;
		type Error = crate::lite::test_transport::SinkError;
		async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
			if self.1 {
				return std::future::pending().await;
			}
			let peer = crate::lite::test_transport::ScriptedSession::new(vec![0x10]);
			Ok(peer.open_bi().await?.1)
		}
		async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
			if !self.1 {
				return std::future::pending().await;
			}
			let mut wire = Vec::new();
			ietf::SubscribeNamespace::ID
				.encode(&mut wire, Version::Draft21)
				.unwrap();
			ietf::SubscribeNamespace {
				request_id: RequestId(0),
				namespace: crate::Path::new(""),
			}
			.encode(&mut wire, Version::Draft21)
			.unwrap();
			crate::lite::test_transport::ScriptedSession::new(wire).open_bi().await
		}
		async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
			self.0.open_bi().await
		}
		async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
			self.0.open_uni().await
		}
		fn send_datagram(&self, bytes: bytes::Bytes) -> Result<(), Self::Error> {
			self.0.send_datagram(bytes)
		}
		async fn recv_datagram(&self) -> Result<bytes::Bytes, Self::Error> {
			self.0.recv_datagram().await
		}
		fn max_datagram_size(&self) -> usize {
			0
		}
		fn protocol(&self) -> Option<&str> {
			Some("moqt-21")
		}
		fn close(&self, code: u32, reason: &str) {
			self.0.close(code, reason);
		}
		async fn closed(&self) -> Self::Error {
			self.0.closed().await
		}
		fn stats(&self) -> impl web_transport_trait::Stats {
			self.0.stats()
		}
	}

	#[tokio::test]
	async fn task_budget_admits_only_32_pending_media_parsers_and_releases_on_cancel() {
		let session = PendingGroups(crate::lite::test_transport::ScriptedSession::new(vec![]), false);
		let (tasks, _driver) = TaskSet::new();
		let groups = super::super::budget::Budget::bounded(32);
		let subscriber = Subscriber::new(
			session.clone(),
			origin::Producer::empty(Origin::random()),
			Control::new(None, false),
			None,
			peer::PeerSetup::default(),
			Origin::random(),
			None,
			Version::Draft21,
			tasks,
		)
		.with_budgets(Default::default(), groups.clone());
		let result = run_unis(session, subscriber, None, true, Version::Draft21).await;
		assert!(matches!(result, Err(Error::ProtocolViolation)));
		// Returning from the driver drops every child and its permit, even though
		// none of the malformed/stalled media headers ever finished parsing.
		let permits: Vec<_> = (0..32).map(|_| groups.acquire().unwrap()).collect();
		assert!(groups.acquire().is_err());
		drop(permits);
		assert!(groups.acquire().is_ok());
	}

	#[tokio::test]
	async fn task_budget_admits_only_32_pending_requests_and_releases_on_cancel() {
		let session = PendingGroups(crate::lite::test_transport::ScriptedSession::new(vec![]), true);
		let (tasks, _driver) = TaskSet::new();
		let requests = super::super::budget::Budget::bounded(32);
		let peer = peer::PeerSetup::default();
		peer.set(Default::default());
		let origin = origin::Producer::empty(Origin::random());
		let control = Control::new(None, false);
		let subscriber = Subscriber::new(
			session.clone(),
			origin.clone(),
			control.clone(),
			None,
			peer.clone(),
			Origin::random(),
			None,
			Version::Draft21,
			tasks,
		)
		.with_budgets(requests.clone(), Default::default());
		let publisher = Publisher::new(session.clone(), origin.consume(), control, None, peer, Version::Draft21)
			.with_budgets(requests.clone(), Default::default());
		assert!(matches!(
			run_dispatch(session, publisher, subscriber, Version::Draft21).await,
			Err(Error::ProtocolViolation)
		));
		let permits: Vec<_> = (0..32).map(|_| requests.acquire().unwrap()).collect();
		assert!(requests.acquire().is_err());
		drop(permits);
		assert!(requests.acquire().is_ok());
	}

	fn occurrences(log: &crate::lite::test_transport::Log, needle: &[u8]) -> usize {
		let writes = log.writes.lock().unwrap();
		writes.windows(needle.len()).filter(|window| *window == needle).count()
	}

	/// The peer's REQUEST_OK followed by a NAMESPACE in the base form: no cluster
	/// parameters, so no HOP_PATH. Built with the crate's own writer so the framing
	/// can't drift from the encoder under test.
	async fn namespace_without_hop_path(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);

		writer.encode(&ietf::RequestOk::ID).await.unwrap();
		writer.encode(&ietf::RequestOk { request_id: None }).await.unwrap();
		writer.encode(&ietf::Namespace::ID).await.unwrap();
		writer
			.encode(&ietf::Namespace {
				suffix: crate::Path::new("cam"),
				cluster: None,
			})
			.await
			.unwrap();

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// A session that negotiated the MoQ Cluster extension requires HOP_PATH on every
	/// NAMESPACE, and the draft answers a missing one by closing the session.
	///
	/// Driven through `start` rather than `run_subscribe_namespace` directly: the
	/// stream surfaces the error either way, so only this loop's handling of it decides
	/// between a close and a warning, and a test below the loop would pass regardless.
	#[tokio::test]
	async fn a_namespace_without_a_hop_path_closes_the_session() {
		const VERSION: Version = Version::Draft19;

		// A driver that swallows the violation parks forever instead of failing, so
		// bound it: paused time makes the deadline fire the moment nothing else can run.
		tokio::time::pause();

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let session = crate::lite::test_transport::ScriptedSession::new(namespace_without_hop_path(VERSION).await);
		let log = session.log.clone();

		let driver = start(Config {
			session,
			setup: None,
			request_id_max: None,
			client: true,
			publish: None,
			subscribe: Some(origin),
			peer_origin: None,
			cost: None,
			version: VERSION,
			path: None,
			peer_setup_stream: None,
			// A peer that declared its Hop ID negotiated the extension, which is what
			// makes the cluster parameters mandatory in both directions.
			peer_declared: Some(peer::Peer {
				cluster: cluster::Peer {
					origin: Some(crate::Origin::new(2).unwrap()),
					cost: None,
				},
				..Default::default()
			}),
		})
		.expect("start the session");

		let err = tokio::time::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.expect("the session ended rather than carrying on")
			.expect_err("a malformed NAMESPACE fails the session");

		assert!(is_protocol_violation(&err), "not treated as the peer's fault: {err}");
		assert_eq!(log.closes(), vec![(err.to_code(), err.to_string())]);
	}

	/// A subscriber issues one SUBSCRIBE_NAMESPACE per PERMITTED PREFIX, and asks for
	/// those prefixes rather than the root it mounts replies under. Driven through
	/// `start` so the per-prefix fan-out is exercised, not just one stream in
	/// isolation: a loop that opened a single stream would still satisfy a test that
	/// called `run_subscribe_namespace` itself.
	#[tokio::test]
	async fn every_permitted_prefix_gets_its_own_subscribe_namespace() {
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let scoped = origin
			.with_root("rootns")
			.and_then(|rooted| rooted.scope(&[crate::Path::new("cam"), crate::Path::new("mic")]))
			.expect("scope the origin to two prefixes");

		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let driver = start(Config {
			session,
			setup: None,
			request_id_max: None,
			client: true,
			publish: None,
			subscribe: Some(scoped),
			peer_origin: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			peer_setup_stream: None,
			peer_declared: None,
		})
		.expect("start the session");
		let _driver = tokio::spawn(driver);

		// Both requests are written before either peer response, which never comes.
		for _ in 0..100 {
			if occurrences(&log, b"cam") > 0 && occurrences(&log, b"mic") > 0 {
				break;
			}
			tokio::time::sleep(std::time::Duration::from_millis(5)).await;
		}

		assert_eq!(occurrences(&log, b"cam"), 1, "one SUBSCRIBE_NAMESPACE for cam");
		assert_eq!(occurrences(&log, b"mic"), 1, "one SUBSCRIBE_NAMESPACE for mic");
		assert_eq!(occurrences(&log, b"rootns"), 0, "asked the peer for our local root");
	}

	/// How many scheduling turns an advertisement gets before the count is taken. Time is
	/// paused in these tests, so each turn costs nothing and only runs the driver until it
	/// parks again; a busy machine cannot turn a slow announce into a passing silence.
	const ANNOUNCE_TURNS: usize = 100;

	/// Run a publish-only session against a peer that declared `peer_declared`, returning
	/// how many times the namespace reached the wire.
	///
	/// The scripted peer answers nothing, so an advertisement parks after writing. That is
	/// enough: the question is only whether the bytes went out unasked.
	async fn announce_occurrences(peer_declared: Option<peer::Peer>) -> usize {
		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let _cam = origin
			.create_broadcast("solo-cam", crate::broadcast::Route::new().with_announce(true))
			.unwrap();

		// An open gate: an unsolicited PUBLISH_NAMESPACE can reach the wire.
		let gate = kio::Producer::new(true);
		let session = crate::lite::test_transport::SinkSession::gated_bi(gate.consume());
		let log = session.log.clone();

		let driver = start(Config {
			session,
			setup: None,
			request_id_max: None,
			client: true,
			publish: Some(origin.consume()),
			subscribe: None,
			peer_origin: None,
			cost: None,
			version: Version::Draft18,
			path: None,
			peer_setup_stream: None,
			peer_declared,
		})
		.expect("start the session");
		let _driver = tokio::spawn(driver);

		// Drive until the announce lands, rather than betting on one fixed window.
		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(&log, b"solo-cam") > 0 {
				break;
			}
			tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		}

		occurrences(&log, b"solo-cam")
	}

	/// The peer's SETUP decides whether an advertisement may go out unasked, so nothing
	/// can be sent before it arrives.
	#[tokio::test(start_paused = true)]
	async fn no_announce_before_the_peer_setup() {
		assert_eq!(
			announce_occurrences(None).await,
			0,
			"advertised before knowing what the peer wants"
		);
	}

	/// A peer that requires solicitation hears nothing until it asks, which is the
	/// behavior the IETF draft describes for a relay.
	#[tokio::test(start_paused = true)]
	async fn a_peer_requiring_solicitation_is_not_told_unasked() {
		let declared = peer::Peer {
			solicit: Some(true),
			..Default::default()
		};

		assert_eq!(
			announce_occurrences(Some(declared)).await,
			0,
			"PUBLISH_NAMESPACE despite the peer asking to be told on request"
		);
	}

	/// A peer that declared nothing is told without being asked. Every relay that never
	/// sends SUBSCRIBE_NAMESPACE depends on this, and the session is what wires the
	/// unsolicited loop up at all.
	#[tokio::test(start_paused = true)]
	async fn a_peer_declaring_nothing_is_told_unasked() {
		assert_eq!(
			announce_occurrences(Some(peer::Peer::default())).await,
			1,
			"no unsolicited PUBLISH_NAMESPACE"
		);
	}

	/// The declared Hop ID must be the caller's own origin, whichever half carries it.
	///
	/// A subscribe-only session (an ingest that publishes nothing) still routes, so
	/// declaring the placeholder's random id would compare incoming paths against an
	/// identity no other session out of this process shares, and a route returning
	/// here would never be recognized as a loop.
	#[test]
	fn the_hop_id_comes_from_whichever_origin_the_caller_set() {
		let ours = crate::Origin::new(42).unwrap();

		let publish = crate::origin::Info::new(ours).produce();
		assert_eq!(self_origin(Some(&publish.consume()), None), ours, "the publish half");

		let subscribe = crate::origin::Info::new(ours).produce();
		assert_eq!(self_origin(None, Some(&subscribe)), ours, "the subscribe half alone");

		// Neither half: nothing to route, so any id will do as long as it is ours.
		let publish = crate::origin::Info::new(ours).produce();
		assert_eq!(self_origin(Some(&publish.consume()), Some(&subscribe)), ours);
	}

	/// Drive `start` against a peer whose incoming streams die before their first
	/// byte, and assert the session shrugs them off: the driver keeps running and
	/// nothing closes the transport.
	///
	/// The dead stream is not exotic. RESET_STREAM is how a peer drops a group, and
	/// QUIC does not order the reset behind the data, so it can beat the first byte
	/// even when the peer wrote one. Erroring an accept loop on it tears down the
	/// whole session over a single stream the peer had already given up on.
	async fn a_dead_incoming_stream_is_not_fatal(session: crate::lite::test_transport::DeadStreamSession) {
		const VERSION: Version = Version::Draft19;

		// A driver that survives parks forever, so bound it: paused time makes the
		// deadline fire the moment nothing else can run.
		tokio::time::pause();

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let log = session.log.clone();

		let driver = start(Config {
			session,
			setup: None,
			request_id_max: None,
			client: true,
			publish: None,
			subscribe: Some(origin),
			peer_origin: None,
			cost: None,
			version: VERSION,
			path: None,
			peer_setup_stream: None,
			// Pre-settled, so nothing waits on a SETUP the dead stream will never
			// carry and the dispatch loop actually runs.
			peer_declared: Some(peer::Peer::default()),
		})
		.expect("start the session");

		tokio::time::timeout(std::time::Duration::from_secs(10), driver)
			.await
			.expect_err("the session ended over one dead stream");

		assert_eq!(log.closes(), vec![], "nothing may close the transport");
	}

	#[tokio::test]
	async fn a_uni_stream_dead_before_its_type_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::unis(1)).await;
	}

	#[tokio::test]
	async fn a_bidi_stream_dead_before_its_header_does_not_end_the_session() {
		a_dead_incoming_stream_is_not_fatal(crate::lite::test_transport::DeadStreamSession::bis(1)).await;
	}

	/// A peer's advertisement of `room/host`, then two namespace-keyed withdrawals of it.
	/// The second has no advertisement left to name.
	async fn publish_namespace_then_two_withdrawals(version: Version) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(crate::lite::test_transport::SinkSend::new(log.clone()), version);

		writer.encode(&ietf::PublishNamespace::ID).await.unwrap();
		writer
			.encode(&ietf::PublishNamespace {
				request_id: RequestId(1),
				track_namespace: crate::Path::new("room/host"),
				cluster: None,
			})
			.await
			.unwrap();

		for _ in 0..2 {
			writer.encode(&ietf::PublishNamespaceDone::ID).await.unwrap();
			writer
				.encode(&ietf::PublishNamespaceDone {
					track_namespace: crate::Path::new("room/host"),
					request_id: RequestId(0),
				})
				.await
				.unwrap();
		}

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// The acknowledgement draft-14 answers a PUBLISH_NAMESPACE with, as bytes to look
	/// for in what we wrote.
	async fn publish_namespace_ok(request_id: RequestId) -> Vec<u8> {
		let log = crate::lite::test_transport::Log::default();
		let mut writer = crate::coding::Writer::new(
			crate::lite::test_transport::SinkSend::new(log.clone()),
			Version::Draft14,
		);

		writer.encode(&ietf::PublishNamespaceOk::ID).await.unwrap();
		writer.encode(&ietf::PublishNamespaceOk { request_id }).await.unwrap();

		let writes = log.writes.lock().unwrap();
		writes.clone()
	}

	/// draft-14 names its withdrawals rather than numbering them, so a repeat has no
	/// advertisement left to resolve to. Dropping it is what keeps the session up: failing
	/// the lookup takes the whole connection down over a message with nothing left to do.
	///
	/// Driven through `start` because only the full loop shows the consequence, the read
	/// task propagating the classifier's error.
	#[tokio::test(start_paused = true)]
	async fn a_repeated_publish_namespace_done_does_not_end_the_session() {
		const VERSION: Version = Version::Draft14;

		let origin = crate::origin::Info::new(crate::Origin::new(1).unwrap()).produce();
		let consumer = origin.consume();

		let session =
			crate::lite::test_transport::ScriptedSession::new(publish_namespace_then_two_withdrawals(VERSION).await);
		let log = session.log.clone();
		let setup = Stream::open(&session, VERSION).await.expect("open the control stream");

		let driver = start(Config {
			session,
			setup: Some(setup),
			request_id_max: None,
			client: true,
			publish: None,
			subscribe: Some(origin),
			peer_origin: None,
			cost: None,
			version: VERSION,
			path: None,
			peer_setup_stream: None,
			peer_declared: None,
		})
		.expect("start the session");
		let driver = tokio::spawn(driver);

		let accepted = publish_namespace_ok(RequestId(1)).await;
		for _ in 0..ANNOUNCE_TURNS {
			if occurrences(&log, &accepted) > 0 && consumer.get_broadcast("room/host").is_none() {
				break;
			}
			tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		}

		assert_eq!(occurrences(&log, &accepted), 1, "the advertisement was not accepted");
		assert!(
			consumer.get_broadcast("room/host").is_none(),
			"the withdrawal did not reach the request that advertised it"
		);
		assert!(!driver.is_finished(), "the repeated withdrawal ended the session");
		assert!(log.closes().is_empty(), "closed the session: {:?}", log.closes());
	}
}

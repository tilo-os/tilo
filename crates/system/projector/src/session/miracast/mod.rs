//! Miracast sessions. The `ScreenCast` portal grants the screen as a `PipeWire`
//! node, `NetworkManager` forms the Wi-Fi Direct group with the receiver, and
//! the `miracast` crate negotiates over RTSP and streams the node to it.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};

use ashpd::desktop::screencast::{
    CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
    StartCastOptions,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode, Session};
use miracast::media::{AudioInput, GstMediaConfig, GstMediaFactory, VideoInput};
use miracast::modes::ModePreference;
use miracast::p2p::{self, ConnectOptions, P2pGroup};
use miracast::source::establish;
use miracast::{AudioPreference, SessionEnd, Source, SourceEvent, SourceOptions};
use tilo_projector::control::ConnectStage;
use tilo_projector::{ProjectionDevice, ProjectionOptions};

use super::context::SessionContext;
use super::receiver_address;
use crate::error::ProjectorError;

/// The status line a playing session logs: the stream reached the receiver.
pub(crate) const PLAYING: &str = "PLAY accepted; media stream started.";

/// Most receivers connect to the source within a few seconds of the group
/// forming; the rest wait to be connected to.
const PASSIVE_GRACE: Duration = Duration::from_secs(4);
/// How long the receiver may take to open the RTSP session at all.
const RTSP_DEADLINE: Duration = Duration::from_secs(30);

/// What a Miracast session talks to. Production uses the system and session
/// buses and the standard RTSP port; tests substitute private ones.
pub(crate) struct Environment {
    /// Where `NetworkManager` is.
    pub(crate) system: zbus::Connection,
    /// Where the `ScreenCast` portal is.
    pub(crate) session: zbus::Connection,
    /// Where to listen for the receiver's RTSP connection.
    pub(crate) rtsp: SocketAddr,
    /// What audio to capture.
    pub(crate) audio: AudioInput,
}

impl Environment {
    fn system_default() -> Result<Self, ProjectorError> {
        let buses = tilo_rt::app()
            .block_on(async {
                let system = zbus::Connection::system().await?;
                let session = zbus::Connection::session().await?;
                Ok::<_, zbus::Error>((system, session))
            })
            .map_err(|error| ProjectorError::Unavailable(error.to_string()))?
            .map_err(|error| {
                ProjectorError::Unavailable(format!("cannot reach the system services: {error}"))
            })?;
        Ok(Self {
            system: buses.0,
            session: buses.1,
            rtsp: SocketAddr::from((Ipv4Addr::UNSPECIFIED, miracast::ie::DEFAULT_RTSP_PORT)),
            audio: AudioInput::DefaultMonitor,
        })
    }
}

pub(crate) fn run_miracast_session(
    device: &ProjectionDevice,
    options: &ProjectionOptions,
    context: &SessionContext,
) -> Result<(), ProjectorError> {
    run(device, options, context, &Environment::system_default()?)
}

/// The screen the portal granted. The cast lasts as long as the session.
struct ScreenGrant {
    session: Session<Screencast>,
    remote: OwnedFd,
    node: u32,
}

async fn grant_screen(bus: &zbus::Connection) -> Result<ScreenGrant, ProjectorError> {
    let failed = |error: ashpd::Error| match error {
        ashpd::Error::Response(_) => {
            ProjectorError::Receiver("screen sharing was not allowed".to_owned())
        }
        other => ProjectorError::Unavailable(format!("the screen-capture portal failed: {other}")),
    };
    let portal = Screencast::with_connection(bus.clone())
        .await
        .map_err(failed)?;
    let session = portal
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(failed)?;
    portal
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                .set_sources(SourceType::Monitor | SourceType::Virtual)
                .set_multiple(false)
                .set_persist_mode(PersistMode::DoNot),
        )
        .await
        .and_then(|request| request.response())
        .map_err(failed)?;
    let streams = portal
        .start(&session, None, StartCastOptions::default())
        .await
        .and_then(|request| request.response())
        .map_err(failed)?;
    let node = streams
        .streams()
        .first()
        .map(ashpd::desktop::screencast::Stream::pipe_wire_node_id)
        .ok_or_else(|| ProjectorError::Receiver("no screen was chosen".to_owned()))?;
    let remote = portal
        .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
        .await
        .map_err(failed)?;
    Ok(ScreenGrant {
        session,
        remote,
        node,
    })
}

fn block_on<T>(future: impl Future<Output = T>) -> Result<T, ProjectorError> {
    tilo_rt::app()
        .block_on(future)
        .map_err(|error| ProjectorError::Unavailable(error.to_string()))
}

/// Forms the group, giving up when the session is stopped first.
fn form_group(
    environment: &Environment,
    peer: &str,
    rtsp_port: u16,
    context: &SessionContext,
) -> Result<Option<P2pGroup>, ProjectorError> {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    context.control().on_stop(move || {
        let _ = stop_tx.send(());
    });
    let options = ConnectOptions {
        rtsp_port,
        ..ConnectOptions::default()
    };
    block_on(async {
        tokio::select! {
            group = p2p::connect(&environment.system, peer, &options) => group.map(Some),
            _ = stop_rx => Ok(None),
        }
    })?
    .map_err(|error| match error {
        p2p::P2pError::Bus(_) | p2p::P2pError::NoDevice => {
            ProjectorError::Unavailable(error.to_string())
        }
        _ => ProjectorError::Receiver(error.to_string()),
    })
}

fn run(
    device: &ProjectionDevice,
    options: &ProjectionOptions,
    context: &SessionContext,
    environment: &Environment,
) -> Result<(), ProjectorError> {
    let peer = receiver_address(device)?.to_owned();
    // The person chooses what to show before anything reaches the receiver.
    let grant = block_on(grant_screen(&environment.session))??;
    let result = stream(
        device,
        options,
        context,
        environment,
        &peer,
        grant.remote,
        grant.node,
    );
    let _ = block_on(grant.session.close());
    result
}

fn stream(
    device: &ProjectionDevice,
    options: &ProjectionOptions,
    context: &SessionContext,
    environment: &Environment,
    peer: &str,
    remote: OwnedFd,
    node: u32,
) -> Result<(), ProjectorError> {
    if context.wait_stop(Duration::ZERO) {
        return Ok(());
    }
    context.stage(ConnectStage::Handshake);
    let listener = TcpListener::bind(environment.rtsp).map_err(ProjectorError::io(
        "cannot listen for the Miracast receiver",
    ))?;
    let rtsp_port = listener
        .local_addr()
        .map_err(ProjectorError::io(
            "cannot listen for the Miracast receiver",
        ))?
        .port();
    let Some(group) = form_group(environment, peer, rtsp_port, context)? else {
        return Ok(());
    };
    let streamed = negotiate_and_stream(
        device,
        options,
        context,
        environment,
        &listener,
        &group,
        remote,
        node,
    );
    if let Err(error) = block_on(group.close())
        .and_then(|closed| closed.map_err(|error| ProjectorError::Unavailable(error.to_string())))
    {
        eprintln!("tilo-projector: cannot end the Wi-Fi Direct group: {error}");
    }
    streamed
}

fn negotiate_and_stream(
    device: &ProjectionDevice,
    options: &ProjectionOptions,
    context: &SessionContext,
    environment: &Environment,
    listener: &TcpListener,
    group: &P2pGroup,
    remote: OwnedFd,
    node: u32,
) -> Result<(), ProjectorError> {
    let stopped = || context.wait_stop(Duration::ZERO);
    let connection = establish(
        listener,
        &|address| group.contains(address.ip()),
        &|| {
            group
                .sink_address()
                .map(|address| SocketAddr::from((address, group.sink_rtsp_port)))
        },
        PASSIVE_GRACE,
        Instant::now()
            .checked_add(RTSP_DEADLINE)
            .ok_or(ProjectorError::Timeout(
                "the clock cannot represent the RTSP deadline",
            ))?,
        &stopped,
    )
    .map_err(ProjectorError::io("cannot accept the Miracast receiver"))?;
    let Some(connection) = connection else {
        return if stopped() {
            Ok(())
        } else {
            Err(ProjectorError::Timeout(
                "the Miracast receiver did not start a session",
            ))
        };
    };
    let profile = options.stream_policy.ceiling(None);
    // Microsoft's Wireless Display Adapter plays only LPCM, and tears the
    // session down when it receives a keep-alive.
    let microsoft = [device.name.as_str(), group.sink_name.as_str()]
        .iter()
        .any(|name| name.to_ascii_lowercase().contains("microsoft"));
    let source_options = SourceOptions {
        preference: ModePreference {
            max_size: options.stream_policy.resolution_limit.dimensions(),
            max_fps: profile.fps,
        },
        audio: match (options.audio, microsoft) {
            (false, _) => AudioPreference::Off,
            (true, true) => AudioPreference::PreferLpcm,
            (true, false) => AudioPreference::Auto,
        },
        keepalive: (!microsoft).then_some(Duration::from_secs(20)),
        ..SourceOptions::default()
    };
    let source = Source::new(connection, source_options)
        .map_err(ProjectorError::io("cannot set up the Miracast session"))?;
    let stop = source.stop_handle();
    context.control().on_stop(move || stop.stop());
    let mut media = GstMediaFactory::new(GstMediaConfig::new(
        VideoInput::PipeWire { remote, node },
        environment.audio.clone(),
        profile.bitrate_kbps,
    ))
    .map_err(|error| ProjectorError::Unavailable(error.to_string()))?;
    let ended = source.run(&mut media, &mut |event| {
        if let SourceEvent::Playing(plan) = event {
            eprintln!(
                "tilo-projector: {PLAYING} {} to {}",
                plan.mode.name(),
                plan.sink_rtp
            );
            context.ready();
        }
    });
    match ended {
        Ok(SessionEnd::Stopped) => Ok(()),
        Ok(_) if stopped() => Ok(()),
        Ok(SessionEnd::SinkTeardown | SessionEnd::Closed) => Err(ProjectorError::Receiver(
            "the receiver ended the Miracast session".to_owned(),
        )),
        Err(error) => Err(ProjectorError::Receiver(error.to_string())),
    }
}

#[cfg(test)]
mod tests;

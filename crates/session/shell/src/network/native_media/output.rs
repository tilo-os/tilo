//! Output preparation reuses the call device observer; it never joins a call or opens capture.
use super::observer;
use media::pw_stream::{ObservedPlayback, PlaybackConfig, PlaybackSource, PlaybackStream};

pub(crate) async fn prepare_playback(
    app: String,
    source: Box<dyn PlaybackSource>,
) -> Result<Box<dyn PlaybackStream>, String> {
    tokio::task::spawn_blocking(move || {
        let observed = observer::observe()?;
        let sink = observed
            .graph
            .sinks
            .iter()
            .find(|sink| sink.is_default)
            .or_else(|| {
                (observed.graph.sinks.len() == 1)
                    .then(|| observed.graph.sinks.first())
                    .flatten()
            })
            .ok_or("No observed system audio output is available")?;
        let config = PlaybackConfig {
            node_name: format!("tilo-app-output-{app}"),
            description: app,
            rate: runtime_collab::audio_output::RATE,
            channels: runtime_collab::audio_output::CHANNELS,
            latency_frames: 480,
            target_serial: Some(sink.serial),
        };
        media::pw_stream::open_observed_playback(
            &config,
            source,
            ObservedPlayback {
                cookie: observed.cookie,
                serial: sink.serial,
                socket: observed.socket,
            },
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "Audio output preparation worker stopped".to_string())?
}

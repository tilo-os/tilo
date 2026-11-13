//! Follows a system service by its signals instead of asking it again on a timer.

use futures_util::StreamExt as _;
use zbus::{MatchRule, MessageStream};

/// Signals queued for a follower before the oldest is dropped: a burst only needs to say that something
/// changed.
const SIGNAL_QUEUE: usize = 64;

#[derive(Debug)]
pub struct ServiceFollow {
    task: tilo_rt::JoinHandle<()>,
}

impl Drop for ServiceFollow {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Calls `on_change` on the app runtime whenever one of `services` sends a signal or one of their names
/// changes hands.
pub fn follow_system_services(
    services: &'static [&'static str],
    on_change: impl Fn() + Send + Sync + 'static,
) -> Result<ServiceFollow, String> {
    follow_boxed(services, Box::new(on_change))
}

fn follow_boxed(
    services: &'static [&'static str],
    on_change: Box<dyn Fn() + Send + Sync>,
) -> Result<ServiceFollow, String> {
    let runtime = tilo_rt::try_app().map_err(|error| error.to_string())?;
    let task = runtime.spawn(async move {
        let mut signals = match signals(services).await {
            Ok(signals) => signals,
            Err(error) => {
                tracing::warn!(?services, %error, "could not follow system services");
                return;
            }
        };
        while signals.next().await.is_some() {
            on_change();
        }
    });
    Ok(ServiceFollow { task })
}

async fn signals(
    services: &'static [&'static str],
) -> zbus::Result<impl futures_util::Stream<Item = zbus::Result<zbus::Message>>> {
    let connection = zbus::Connection::system().await?;
    let mut streams = Vec::with_capacity(services.len().saturating_mul(2));
    for service in services {
        streams.push(
            MessageStream::for_match_rule(
                MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .sender(*service)?
                    .build(),
                &connection,
                Some(SIGNAL_QUEUE),
            )
            .await?,
        );
        streams.push(
            MessageStream::for_match_rule(
                MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .sender("org.freedesktop.DBus")?
                    .path("/org/freedesktop/DBus")?
                    .interface("org.freedesktop.DBus")?
                    .member("NameOwnerChanged")?
                    .arg(0, *service)?
                    .build(),
                &connection,
                Some(SIGNAL_QUEUE),
            )
            .await?,
        );
    }
    Ok(futures_util::stream::select_all(streams))
}

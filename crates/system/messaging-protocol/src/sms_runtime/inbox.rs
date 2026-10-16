//! Read-only modem inbox observation, subscribed before initial enumeration.
//! Signals coalesce into a wake; the retained native object list is reread.
use super::transport::{DbusTransport, MESSAGING, SMS};
use crate::{Error, journal::Owner};
use futures_util::StreamExt as _;
use zbus::zvariant::OwnedObjectPath;

/// Read-only handle derived from one exact native runtime. No public constructor
/// or ambient discovery. It cannot create, send, store, delete or acknowledge SMS.
pub struct SmsInbox {
    owner: Owner,
    transport: DbusTransport,
}
impl SmsInbox {
    pub(super) fn new(owner: Owner, transport: DbusTransport) -> Self {
        Self { owner, transport }
    }
    pub fn owner(&self) -> &Owner {
        &self.owner
    }
    pub(crate) async fn subscribe(&self) -> Result<Signals, Error> {
        self.transport.check_binding().await?;
        let added = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(self.transport.binding.daemon.clone())
            .map_err(unavailable)?
            .path(self.transport.binding.modem.clone())
            .map_err(unavailable)?
            .interface(MESSAGING)
            .map_err(unavailable)?
            .member("Added")
            .map_err(unavailable)?
            .build();
        let changed = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(self.transport.binding.daemon.clone())
            .map_err(unavailable)?
            .path_namespace("/org/freedesktop/ModemManager1/SMS")
            .map_err(unavailable)?
            .interface("org.freedesktop.DBus.Properties")
            .map_err(unavailable)?
            .member("PropertiesChanged")
            .map_err(unavailable)?
            .add_arg(SMS)
            .map_err(unavailable)?
            .build();
        let mut added =
            zbus::MessageStream::for_match_rule(added, &self.transport.connection, Some(64))
                .await
                .map_err(unavailable)?;
        let mut changed =
            zbus::MessageStream::for_match_rule(changed, &self.transport.connection, Some(64))
                .await
                .map_err(unavailable)?;
        self.transport.check_binding().await?;
        let (wake, next) = tokio::sync::watch::channel(Ok(()));
        let task = tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    event = added.next() => event,
                    event = changed.next() => event,
                };
                if !matches!(event, Some(Ok(_))) {
                    let _ = wake.send(Err(Error::ConnectionUnavailable));
                    break;
                }
                // Continuously drain D-Bus streams while a bounded inbox scan is
                // in flight. Coalescing wakes does not discard native objects.
                if wake.send(Ok(())).is_err() {
                    break;
                }
            }
        });
        Ok(Signals { task, next })
    }
    pub(crate) async fn objects(&self) -> Result<Vec<String>, Error> {
        self.transport.check_binding().await?;
        let modem = self
            .transport
            .proxy(self.transport.binding.modem.as_str(), MESSAGING)
            .await?;
        let paths: Vec<OwnedObjectPath> =
            modem.get_property("Messages").await.map_err(unavailable)?;
        // Refuse excessive or malformed native lists instead of silently omitting
        // messages. Duplicate paths are harmless and scanned only once.
        if paths.len() > 512
            || paths
                .iter()
                .any(|path| !super::transport::object(path.as_str(), "SMS"))
        {
            return Err(Error::InvalidFrame);
        }
        self.transport.check_binding().await?;
        Ok(paths
            .into_iter()
            .map(|path| path.to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect())
    }
    pub(crate) async fn recipient(&self, path: &str) -> Result<Option<String>, Error> {
        let message: OwnedObjectPath = path.try_into().map_err(unavailable)?;
        self.transport.check_binding().await?;
        self.transport.received_member(&message).await?;
        let sms = self.transport.proxy(path, SMS).await?;
        let state: u32 = sms.get_property("State").await.map_err(unavailable)?;
        let pdu: u32 = sms.get_property("PduType").await.map_err(unavailable)?;
        if pdu != 1
            || crate::sms::observe(state, pdu, 0, 0)? != crate::sms::Observation::IncomingComplete
        {
            return Ok(None);
        }
        let number: String = sms.get_property("Number").await.map_err(unavailable)?;
        if !crate::identifier(&number) {
            return Err(Error::InvalidIdentity);
        }
        let class: i32 = sms.get_property("Class").await.map_err(unavailable)?;
        if !matches!(class, -1 | 1..=3) {
            return Ok(None);
        }
        self.transport.check_binding().await?;
        Ok(Some(number))
    }
}
pub(crate) struct Signals {
    task: tokio::task::JoinHandle<()>,
    next: tokio::sync::watch::Receiver<Result<(), Error>>,
}
impl Signals {
    pub(crate) async fn changed(&mut self) -> Result<(), Error> {
        self.next.changed().await.map_err(unavailable)?;
        *self.next.borrow_and_update()
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn unavailable<T>(_: T) -> Error {
    Error::ConnectionUnavailable
}

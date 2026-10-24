//! Where a room stands for the apps on this device
//! (`CollabService.roomStatus`, `CollabService.awaitRoom`).

#[cfg(feature = "client")]
use crate::collab_capnp;

/// Where a room stands for the apps on this device: whether an app can open
/// documents in it now, and if not, why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RoomStatus {
    /// Apps open documents in it.
    #[default]
    Ready,
    /// Not yet: why, in the person's words, and what last held it up when
    /// something did. A join made meanwhile is held until it is ready.
    Waiting {
        reason: String,
        detail: Option<String>,
    },
    /// It will not be ready on this device, and why.
    Refused(String),
}

#[cfg(feature = "client")]
impl RoomStatus {
    /// The status `reader` carries.
    ///
    /// # Errors
    ///
    /// A field that is not text.
    pub fn from_reader(reader: collab_capnp::room_status::Reader<'_>) -> capnp::Result<Self> {
        let text = |field: capnp::Result<capnp::text::Reader<'_>>| -> capnp::Result<String> {
            field?
                .to_str()
                .map(str::to_owned)
                .map_err(|error| capnp::Error::failed(format!("room status not utf8: {error}")))
        };
        let refusal = text(reader.get_refusal())?;
        if !refusal.is_empty() {
            return Ok(Self::Refused(refusal));
        }
        let reason = text(reader.get_waiting())?;
        if reason.is_empty() {
            return Ok(Self::Ready);
        }
        let detail = text(reader.get_detail())?;
        Ok(Self::Waiting {
            reason,
            detail: (!detail.is_empty()).then_some(detail),
        })
    }

    /// Write this status into `builder`.
    pub fn write(&self, mut builder: collab_capnp::room_status::Builder<'_>) {
        match self {
            Self::Ready => {}
            Self::Waiting { reason, detail } => {
                builder.set_waiting(reason);
                builder.set_detail(detail.as_deref().unwrap_or(""));
            }
            Self::Refused(reason) => builder.set_refusal(reason),
        }
    }
}

#[cfg(feature = "client")]
impl crate::CollabClient {
    /// Where the room `channel_id` names stands for apps on this device. A
    /// broker from before rooms could wait answers that it is ready: it
    /// takes or refuses a join as it comes.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn room_status(
        &self,
        channel_id: &str,
    ) -> Result<RoomStatus, runtime_portal::NotAvailable> {
        let mut req = self.inner.room_status_request();
        req.get().set_channel_id(channel_id);
        match req.send().promise.await {
            Ok(reply) => Ok(RoomStatus::from_reader(reply.get()?.get_status()?)?),
            Err(error) if error.kind == capnp::ErrorKind::Unimplemented => Ok(RoomStatus::Ready),
            Err(error) => Err(error.into()),
        }
    }

    /// The status of the room `channel_id` names once it is no longer
    /// `seen`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn await_room(
        &self,
        channel_id: &str,
        seen: &RoomStatus,
    ) -> Result<RoomStatus, runtime_portal::NotAvailable> {
        let mut req = self.inner.await_room_request();
        {
            let mut builder = req.get();
            builder.set_channel_id(channel_id);
            seen.write(builder.init_seen());
        }
        match req.send().promise.await {
            Ok(reply) => Ok(RoomStatus::from_reader(reply.get()?.get_status()?)?),
            Err(error) if error.kind == capnp::ErrorKind::Unimplemented => Ok(RoomStatus::Ready),
            Err(error) => Err(error.into()),
        }
    }
}

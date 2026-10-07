use std::collections::HashMap;

use serde::Serialize;
use zbus::names::OwnedUniqueName;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedObjectPath, Value};
use zeroize::Zeroizing;

use crate::sms::CreateText;
use crate::{Error, ProtocolReference};

const SERVICE: &str = "org.freedesktop.ModemManager1";
const MODEM: &str = "org.freedesktop.ModemManager1.Modem";
const SIM: &str = "org.freedesktop.ModemManager1.Sim";
pub(super) const MESSAGING: &str = "org.freedesktop.ModemManager1.Modem.Messaging";
pub(super) const SMS: &str = "org.freedesktop.ModemManager1.Sms";

/// Verified host inputs, not values accepted from an app's send parameters.
/// Account/device/link generation is held separately by the runtime owner.
#[derive(Clone)]
pub struct SimBinding {
    pub(super) daemon: OwnedUniqueName,
    pub(super) modem: OwnedObjectPath,
    sim: OwnedObjectPath,
    iccid: Zeroizing<String>,
    epoch: u64,
}

impl SimBinding {
    /// Capture one authorized SIM without discovery, fallback or normalization.
    ///
    /// # Errors
    /// Refuses invalid daemon/object identities, absent ICCID or zero epoch.
    pub fn new(
        daemon: String,
        modem: String,
        sim: String,
        iccid: String,
        epoch: u64,
    ) -> Result<Self, Error> {
        if !object(&modem, "Modem")
            || !object(&sim, "SIM")
            || epoch == 0
            || iccid.is_empty()
            || iccid.len() > 32
            || !iccid.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(Error::InvalidIdentity);
        }
        Ok(Self {
            daemon: daemon.try_into().map_err(|_| Error::InvalidIdentity)?,
            modem: modem.try_into().map_err(|_| Error::InvalidIdentity)?,
            sim: sim.try_into().map_err(|_| Error::InvalidIdentity)?,
            iccid: Zeroizing::new(iccid),
            epoch,
        })
    }
}

/// Only the supervisor supplies this bus; the UI never receives its handle.
#[derive(Clone)]
pub struct DbusTransport {
    pub(super) connection: zbus::Connection,
    pub(super) binding: SimBinding,
}

impl DbusTransport {
    pub fn new(connection: zbus::Connection, binding: SimBinding) -> Self {
        Self {
            connection,
            binding,
        }
    }

    pub(super) async fn check_binding(&self) -> Result<(), Error> {
        let bus = zbus::fdo::DBusProxy::new(&self.connection)
            .await
            .map_err(unavailable)?;
        let owner = bus
            .get_name_owner(SERVICE.try_into().map_err(unavailable)?)
            .await
            .map_err(unavailable)?;
        if owner != self.binding.daemon {
            return Err(Error::WrongAccount);
        }
        let modem = self.proxy(self.binding.modem.as_str(), MODEM).await?;
        let sim: OwnedObjectPath = modem.get_property("Sim").await.map_err(unavailable)?;
        if sim != self.binding.sim {
            return Err(Error::WrongAccount);
        }
        let sim = self.proxy(self.binding.sim.as_str(), SIM).await?;
        let iccid: Zeroizing<String> = Zeroizing::new(
            sim.get_property("SimIdentifier")
                .await
                .map_err(unavailable)?,
        );
        if iccid != self.binding.iccid {
            return Err(Error::WrongAccount);
        }
        Ok(())
    }

    pub(super) fn freeze(&self, properties: &CreateText) -> Result<Zeroizing<Vec<u8>>, Error> {
        #[derive(Serialize)]
        struct Frozen<'a> {
            number: &'a str,
            text: &'a str,
            daemon: &'a str,
            modem: &'a str,
            sim: &'a str,
            iccid: &'a str,
            epoch: u64,
            delivery_report_request: bool,
        }
        serde_json::to_vec(&Frozen {
            number: &properties.number,
            text: &properties.text,
            daemon: self.binding.daemon.as_str(),
            modem: self.binding.modem.as_str(),
            sim: self.binding.sim.as_str(),
            iccid: &self.binding.iccid,
            epoch: self.binding.epoch,
            delivery_report_request: properties.delivery_report_request,
        })
        .map(Zeroizing::new)
        .map_err(|_| Error::InvalidRequest)
    }

    pub(super) async fn create(&self, properties: &CreateText) -> Result<OwnedObjectPath, Error> {
        let dictionary = HashMap::from([
            ("number", Value::from(properties.number.as_str())),
            ("text", Value::from(properties.text.as_str())),
            (
                "delivery-report-request",
                Value::from(properties.delivery_report_request),
            ),
        ]);
        let proxy = self.proxy(self.binding.modem.as_str(), MESSAGING).await?;
        let message: OwnedObjectPath = proxy
            .call("Create", &(dictionary,))
            .await
            .map_err(unavailable)?;
        if !object(message.as_str(), "SMS") {
            return Err(Error::InvalidFrame);
        }
        Ok(message)
    }

    pub(super) async fn verify_created(
        &self,
        message: &OwnedObjectPath,
        properties: &CreateText,
    ) -> Result<(), Error> {
        let modem = self.proxy(self.binding.modem.as_str(), MESSAGING).await?;
        let members: Vec<OwnedObjectPath> =
            modem.get_property("Messages").await.map_err(unavailable)?;
        if members.len() > 16_384 || !members.contains(message) {
            return Err(Error::InvalidFrame);
        }
        let sms = self.proxy(message.as_str(), SMS).await?;
        let number: String = sms.get_property("Number").await.map_err(unavailable)?;
        let text: Zeroizing<String> =
            Zeroizing::new(sms.get_property("Text").await.map_err(unavailable)?);
        let state: u32 = sms.get_property("State").await.map_err(unavailable)?;
        let pdu: u32 = sms.get_property("PduType").await.map_err(unavailable)?;
        let report: bool = sms
            .get_property("DeliveryReportRequest")
            .await
            .map_err(unavailable)?;
        // Create initializes an unstored outgoing SMS as Unknown/Submit. Stored,
        // received or already sent objects cannot be this freshly created request.
        if number != properties.number
            || text != properties.text
            || state != 0
            || pdu != 2
            || !report
        {
            return Err(Error::InvalidFrame);
        }
        Ok(())
    }

    pub(super) async fn send(&self, message: &OwnedObjectPath) -> Result<(), Error> {
        self.proxy(message.as_str(), SMS)
            .await?
            .call("Send", &())
            .await
            .map_err(unavailable)
    }

    pub(super) fn reference(&self, message: &OwnedObjectPath) -> ProtocolReference {
        ProtocolReference::Sms {
            modem_object: self.binding.modem.to_string(),
            message_object: message.to_string(),
            daemon_epoch: self.binding.epoch,
        }
    }

    pub(super) async fn read_received(
        &self,
        route: &crate::Route,
        path: &str,
    ) -> Result<super::ReceivedSms, Error> {
        if !object(path, "SMS") || route.service() != crate::Service::Sms {
            return Err(Error::InvalidIdentity);
        }
        self.check_binding().await?;
        let message: OwnedObjectPath = path.try_into().map_err(unavailable)?;
        self.received_member(&message).await?;
        let sms = self.proxy(path, SMS).await?;
        let state: u32 = sms.get_property("State").await.map_err(unavailable)?;
        let pdu: u32 = sms.get_property("PduType").await.map_err(unavailable)?;
        // CDMA teleservices and their timestamps need a separate admission policy.
        if pdu != 1
            || crate::sms::observe(state, pdu, 0, 0)? != crate::sms::Observation::IncomingComplete
        {
            return Err(Error::Unsupported);
        }
        let number: String = sms.get_property("Number").await.map_err(unavailable)?;
        if number != route.recipient() {
            return Err(Error::InvalidIdentity);
        }
        let class: i32 = sms.get_property("Class").await.map_err(unavailable)?;
        // Flash SMS has different display/retention semantics; never turn it
        // into an ordinary durable inbox record while that UI is unsupported.
        if !matches!(class, -1 | 1..=3) {
            return Err(Error::Unsupported);
        }
        let text = Zeroizing::new(
            sms.get_property::<String>("Text")
                .await
                .map_err(unavailable)?,
        );
        let prepared = crate::sms::create_text(route, &text)?;
        let data: Zeroizing<Vec<u8>> =
            Zeroizing::new(sms.get_property("Data").await.map_err(unavailable)?);
        if !data.is_empty() {
            return Err(Error::Unsupported);
        }
        let timestamp: String = sms.get_property("Timestamp").await.map_err(unavailable)?;
        if timestamp.len() > 64 {
            return Err(Error::InvalidFrame);
        }
        let time =
            chrono::DateTime::parse_from_rfc3339(&timestamp).map_err(|_| Error::InvalidFrame)?;
        let timestamp_ms = u64::try_from(time.timestamp_millis())
            .ok()
            .filter(|time| *time > 0)
            .ok_or(Error::InvalidFrame)?;
        // Refuse a changing object rather than projecting a mixture of reads.
        // The supervisor also retains the SIM lease across this read-only RPC.
        let checked_text: Zeroizing<String> =
            Zeroizing::new(sms.get_property("Text").await.map_err(unavailable)?);
        let checked_data: Zeroizing<Vec<u8>> =
            Zeroizing::new(sms.get_property("Data").await.map_err(unavailable)?);
        if sms
            .get_property::<u32>("State")
            .await
            .map_err(unavailable)?
            != state
            || sms
                .get_property::<u32>("PduType")
                .await
                .map_err(unavailable)?
                != pdu
            || sms
                .get_property::<String>("Number")
                .await
                .map_err(unavailable)?
                != number
            || sms
                .get_property::<i32>("Class")
                .await
                .map_err(unavailable)?
                != class
            || sms
                .get_property::<String>("Timestamp")
                .await
                .map_err(unavailable)?
                != timestamp
            || checked_text != text
            || checked_data != data
        {
            return Err(Error::InvalidFrame);
        }
        self.received_member(&message).await?;
        self.check_binding().await?;
        let id = serde_json::to_string(&(
            self.binding.daemon.as_str(),
            self.binding.epoch,
            self.binding.modem.as_str(),
            message.as_str(),
        ))
        .map_err(|_| Error::InvalidFrame)?;
        if id.len() > 509 {
            return Err(Error::InvalidFrame);
        }
        Ok(super::ReceivedSms {
            id,
            number: prepared.number,
            text: prepared.text,
            timestamp_ms,
        })
    }
    pub(super) async fn received_member(&self, message: &OwnedObjectPath) -> Result<(), Error> {
        let modem = self.proxy(self.binding.modem.as_str(), MESSAGING).await?;
        let members: Vec<OwnedObjectPath> =
            modem.get_property("Messages").await.map_err(unavailable)?;
        if members.len() > 16_384 || !members.contains(message) {
            return Err(Error::InvalidIdentity);
        }
        Ok(())
    }

    pub(super) async fn proxy<'a>(
        &'a self,
        path: &'a str,
        interface: &'a str,
    ) -> Result<zbus::Proxy<'a>, Error> {
        zbus::proxy::Builder::new(&self.connection)
            .destination(self.binding.daemon.as_str())
            .map_err(unavailable)?
            .path(path)
            .map_err(unavailable)?
            .interface(interface)
            .map_err(unavailable)?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .map_err(unavailable)
    }
}

pub(super) fn object(value: &str, kind: &str) -> bool {
    value
        .strip_prefix(&format!("/org/freedesktop/ModemManager1/{kind}/"))
        .is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.len() <= 32
                && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}
fn unavailable<T>(_: T) -> Error {
    Error::ConnectionUnavailable
}

//! Using an action on another of the person's own devices and hearing its
//! answer.
//!
//! An app on one device names one of the person's enrolled devices, an
//! application and one of that application's actions; the host on this
//! device admits the request, carries it to the other device as an ordinary
//! [`InvokeRequest`](super::InvokeRequest) on a workspace both devices
//! share, follows it there, and records the other device's outcome as this
//! invocation's. Nothing here is a new wire format: the other device sees
//! the same request any of the person's devices sends it, and decides it
//! for the device the transport authenticated.
//!
//! Only the person at this device reaches it, for an application's own
//! actions: an agent, a bot, a Shortcut or another device gets no reach
//! across devices it lacks here.

use serde::{Deserialize, Serialize};

use super::{ActionHostError, InvokeArgument, InvokeRequest, RemoteActionTarget};
use crate::door::ActionAddress;

/// The provider the host answers own-device invocations under.
pub const PROVIDER: &str = "tilo.own-device";
/// The resource the invocation is an action of.
pub const RESOURCE: &str = "identity.device";
/// Invoke an action on one of the person's devices; answers its outcome.
pub const INVOKE: &str = "invoke";

/// The target of an own-device invocation.
#[must_use]
pub fn target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, INVOKE),
    }
}

/// What an own-device invocation asks for: application `app`'s action
/// `action` on its resource `resource`, on the device whose public key is
/// `device`, with `arguments`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invocation {
    /// The other device's public key, as 64 lowercase hex digits.
    pub device: String,
    /// The application whose action it is, by package id.
    pub app: String,
    /// The application's resource the action is on.
    pub resource: String,
    /// The action's id on that resource.
    pub action: String,
    /// The action's typed arguments; absent for an action that takes none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<serde_json::Map<String, serde_json::Value>>,
}

/// One admitted own-device invocation, checked: the device's key, the
/// application, the action and what the action is given.
#[derive(Clone, Debug, PartialEq)]
pub struct Checked {
    pub device: [u8; 32],
    pub app: String,
    pub address: ActionAddress,
    pub argument: InvokeArgument,
}

fn invalid(reason: &str) -> ActionHostError {
    ActionHostError::InvalidRequest {
        reason: reason.to_owned(),
    }
}

impl Invocation {
    /// The argument an invocation of [`target`] carries.
    #[must_use]
    pub fn argument(&self) -> InvokeArgument {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::Object(map)) => InvokeArgument::Json(map),
            _ => InvokeArgument::None,
        }
    }

    /// Read an own-device invocation from what a request carries.
    ///
    /// # Errors
    ///
    /// [`ActionHostError::InvalidRequest`] when the argument is not one.
    pub fn from_argument(argument: &InvokeArgument) -> Result<Self, ActionHostError> {
        let InvokeArgument::Json(map) = argument else {
            return Err(invalid("an own-device invocation takes JSON arguments"));
        };
        serde_json::from_value(serde_json::Value::Object(map.clone()))
            .map_err(|_| invalid("the own-device invocation is malformed"))
    }

    /// The invocation, checked: a device key, an application id an
    /// application provider can name, and an action address.
    ///
    /// # Errors
    ///
    /// [`ActionHostError::InvalidRequest`] naming the part that is not.
    pub fn check(&self) -> Result<Checked, ActionHostError> {
        let device = decode_device(&self.device).ok_or_else(|| invalid("not a device key"))?;
        if self.app.is_empty() || self.app.contains(':') {
            return Err(invalid("not an application id"));
        }
        if self.resource.is_empty() || self.action.is_empty() {
            return Err(invalid("not an action"));
        }
        // A separator in either half would name another address than the
        // one checked here once it is carried as one string.
        let address = ActionAddress::new(self.resource.clone(), self.action.clone());
        if ActionAddress::parse(&address.encode()).as_ref() != Some(&address) {
            return Err(invalid("not an action"));
        }
        Ok(Checked {
            device,
            app: self.app.clone(),
            address,
            argument: self
                .arguments
                .clone()
                .map_or(InvokeArgument::None, InvokeArgument::Json),
        })
    }
}

impl Checked {
    /// The request the other device is sent: the application's action in
    /// workspace `room`, which both devices share, under the original
    /// invocation's id so a retry there observes the first run. A
    /// confirmation this device witnessed names an answer only this device
    /// knows, so it is not carried; a person's own word is.
    #[must_use]
    pub fn forwarded(&self, room: &str, original: &InvokeRequest) -> InvokeRequest {
        let confirmation = original
            .confirmation
            .clone()
            .filter(|confirmation| !matches!(confirmation, super::Confirmation::Witnessed(_)));
        InvokeRequest {
            on_behalf_of: None,
            invocation: original.invocation,
            target: super::application_action_target(
                room,
                &self.app,
                self.address.resource.clone(),
                self.address.action.clone(),
            ),
            argument: self.argument.clone(),
            context: None,
            confirmation,
            revision: None,
            wait_ms: None,
            appearance: None,
        }
    }
}

/// A device key from its 64 lowercase hex digits.
#[must_use]
pub fn decode_device(hex_key: &str) -> Option<[u8; 32]> {
    if hex_key.len() != 64 || hex_key.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    let mut key = [0_u8; 32];
    hex::decode_to_slice(hex_key, &mut key).ok()?;
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::{Invocation, PROVIDER, decode_device, target};
    use crate::remote::{Confirmation, InvokeArgument, InvokeRequest, application_provider};

    fn invocation() -> Invocation {
        Invocation {
            device: hex::encode([7_u8; 32]),
            app: "so.tilo.settings".into(),
            resource: "settings.root".into(),
            action: "storage-summary".into(),
            arguments: None,
        }
    }

    #[test]
    fn an_own_device_invocation_round_trips_through_its_argument() {
        let asked = invocation();
        let read = Invocation::from_argument(&asked.argument()).expect("reads back");
        assert_eq!(read, asked);
        let checked = read.check().expect("valid");
        assert_eq!(checked.device, [7; 32]);
        assert_eq!(checked.argument, InvokeArgument::None);
        assert_eq!(target().provider, PROVIDER);
    }

    #[test]
    fn a_malformed_own_device_invocation_is_refused() {
        assert!(Invocation::from_argument(&InvokeArgument::Text("x".into())).is_err());
        let mut extra = invocation().argument();
        if let InvokeArgument::Json(map) = &mut extra {
            map.insert("caller".into(), serde_json::json!("someone else"));
        }
        assert!(
            Invocation::from_argument(&extra).is_err(),
            "nothing beyond the declared fields is read, least of all a caller"
        );
        for broken in [
            Invocation {
                device: "AB".repeat(32),
                ..invocation()
            },
            Invocation {
                device: "ab".into(),
                ..invocation()
            },
            Invocation {
                app: "room:app".into(),
                ..invocation()
            },
            Invocation {
                action: String::new(),
                ..invocation()
            },
            Invocation {
                resource: format!("settings.root{}other", crate::KEY_SEP),
                ..invocation()
            },
        ] {
            assert!(broken.check().is_err(), "{broken:?}");
        }
        assert_eq!(decode_device(&hex::encode([1; 32])), Some([1; 32]));
    }

    #[test]
    fn the_forwarded_request_names_the_app_in_the_shared_room_under_the_same_id() {
        let checked = Invocation {
            arguments: Some(serde_json::Map::from_iter([(
                "workspace".into(),
                serde_json::json!("w"),
            )])),
            ..invocation()
        }
        .check()
        .unwrap();
        let original = InvokeRequest {
            on_behalf_of: None,
            invocation: crate::InvocationId::new(),
            target: target(),
            argument: InvokeArgument::None,
            context: None,
            confirmation: Some(Confirmation::Witnessed("local-token".into())),
            revision: None,
            wait_ms: Some(1),
            appearance: None,
        };
        let forwarded = checked.forwarded("room-1", &original);
        assert_eq!(forwarded.invocation, original.invocation);
        assert_eq!(
            application_provider(&forwarded.target.provider),
            Some(("room-1", "so.tilo.settings"))
        );
        assert_eq!(forwarded.target.address.action, "storage-summary");
        assert!(matches!(forwarded.argument, InvokeArgument::Json(_)));
        assert_eq!(
            forwarded.confirmation, None,
            "a token only this device can redeem is not sent"
        );
    }
}

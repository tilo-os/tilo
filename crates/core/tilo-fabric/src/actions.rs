//! The fabric's services as typed action providers: each publishes its resources and actions in a
//! table ([`Provider`]), and each action answers at once with its output, so a call from the
//! person's other device, from the session on this one or from an agent reads its answer in the
//! one round trip.
//!
//! Who asks decides what may be asked ([`Who`]): the person on this device, through its session or
//! the device's own tools, or another of the person's devices, which proved a certificate of the
//! same person. An action addressed to another device (`devices/<id>/…` naming another id) is not
//! this provider's.

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, CatalogueRow,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_actions::{
    ActionAddress, ActionContract, ActionDescriptor, ActionOutput, ActionState, ExecutionLocus,
    InputSpec, InvocationId, OperationKind, ResultSpec,
};

use crate::DeviceId;
use crate::calls::DEVICES_PREFIX;

/// The action a resource is read through.
pub const READ: &str = "read";

/// Who asked: the person on this device, or another of the person's devices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Who {
    /// The person here: this device's session, its tools, or an agent acting for them here.
    Here,
    /// Another of the person's devices, by the key it proved.
    Device(DeviceId),
}

/// Who `caller` is to the device `local` of the person `person`.
///
/// # Errors
/// [`ActionCallError::NotPermitted`] for a device of someone else, or a caller acting for nobody.
pub fn who(caller: &Caller, local: &DeviceId, person: &[u8; 32]) -> Result<Who, ActionCallError> {
    if let Some(key) = caller.device_public_key() {
        let device = DeviceId::of_key(key);
        if &device == local {
            return Ok(Who::Here);
        }
        if caller.user_public_key() != Some(person) {
            return Err(ActionCallError::NotPermitted);
        }
        return Ok(Who::Device(device));
    }
    if caller.acting().is_person() {
        Ok(Who::Here)
    } else {
        Err(ActionCallError::NotPermitted)
    }
}

/// Who may ask an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asker {
    /// The person on this device only.
    Here,
    /// Another of the person's devices only.
    Device,
    /// Either.
    Anyone,
}

type Handler = dyn Fn(&Who, Value) -> Result<Value, ActionCallError> + Send + Sync;

struct Entry {
    resource: &'static str,
    action: &'static str,
    label: &'static str,
    kind: OperationKind,
    asker: Asker,
    handler: Box<Handler>,
}

/// One service's resources and actions on this device, each answered at once by its handler.
pub struct Provider {
    name: &'static str,
    local: DeviceId,
    person: [u8; 32],
    /// Whether a device that proved only its key is asked as a device: a scope the daemon admits
    /// uncertified callers to, whose actions check the key themselves.
    any_device: bool,
    entries: Vec<Entry>,
}

impl Provider {
    /// The provider `name` of the device `local`, one of `person`'s.
    #[must_use]
    pub fn new(name: &'static str, local: DeviceId, person: [u8; 32]) -> Self {
        Self {
            name,
            local,
            person,
            any_device: false,
            entries: Vec::new(),
        }
    }

    /// The provider `name` of the device `local`, asked also by a device that holds no certificate
    /// of the person's: each action decides what such a key may have.
    #[must_use]
    pub fn uncertified(name: &'static str, local: DeviceId, person: [u8; 32]) -> Self {
        Self {
            any_device: true,
            ..Self::new(name, local, person)
        }
    }

    fn who(&self, caller: &Caller) -> Result<Who, ActionCallError> {
        match (self.any_device, caller.device_public_key()) {
            (true, Some(key)) if DeviceId::of_key(key) != self.local => {
                Ok(Who::Device(DeviceId::of_key(key)))
            }
            _ => who(caller, &self.local, &self.person),
        }
    }

    /// The resource `resource`, read by the person here or by another of their devices as `read`
    /// says.
    #[must_use]
    pub fn read<O: Serialize + 'static>(
        mut self,
        resource: &'static str,
        label: &'static str,
        read: impl Fn() -> O + Send + Sync + 'static,
    ) -> Self {
        self.entries.push(Entry {
            resource,
            action: READ,
            label,
            kind: OperationKind::View,
            asker: Asker::Anyone,
            handler: Box::new(move |_, _| to_value(&read())),
        });
        self
    }

    /// The action `resource#action`, asked by `asker` with an `I`, answered with an `O`.
    #[must_use]
    pub fn act<I: DeserializeOwned + 'static, O: Serialize + 'static>(
        mut self,
        resource: &'static str,
        action: &'static str,
        label: &'static str,
        asker: Asker,
        act: impl Fn(&Who, I) -> Result<O, ActionCallError> + Send + Sync + 'static,
    ) -> Self {
        self.entries.push(Entry {
            resource,
            action,
            label,
            kind: OperationKind::Effect,
            asker,
            handler: Box::new(move |who, input| {
                let input = serde_json::from_value(input).map_err(|error| {
                    ActionCallError::InvalidRequest {
                        reason: error.to_string(),
                    }
                })?;
                to_value(&act(who, input)?)
            }),
        });
        self
    }

    /// Hand it out as the provider a host mounts.
    #[must_use]
    pub fn shared(self) -> Arc<dyn ActionProvider + Send + Sync> {
        Arc::new(self)
    }

    /// The resource `address` names on this device: its own name, or under `devices/<this id>/`.
    fn resource<'a>(&self, address: &'a ActionAddress) -> Option<&'a str> {
        match address.resource.strip_prefix(DEVICES_PREFIX) {
            Some(rest) => {
                let (device, resource) = rest.split_once('/')?;
                (device == self.local.as_str()).then_some(resource)
            }
            None => Some(address.resource.as_str()),
        }
    }

    fn descriptor(entry: &Entry) -> ActionDescriptor {
        ActionDescriptor {
            did: None,
            state: ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(entry.resource, entry.action),
            resource_kind: String::new(),
            resource_label: entry.resource.to_owned(),
            action_label: entry.label.to_owned(),
            keywords: Vec::new(),
            icon: None,
            description: Some(entry.label.to_owned()),
            locus: ExecutionLocus::default(),
            kind: entry.kind,
            contract: ActionContract {
                result: ResultSpec::Json {
                    schema: serde_json::json!({}),
                },
                ..ActionContract::for_operation(entry.kind)
            },
            input: InputSpec::None,
            params: Vec::new(),
            view: None,
        }
    }
}

fn to_value(value: &impl Serialize) -> Result<Value, ActionCallError> {
    serde_json::to_value(value).map_err(|error| ActionCallError::Unavailable {
        reason: error.to_string(),
    })
}

/// The argument of `request` as one JSON value: an object, empty when it carries none.
fn argument(request: &InvokeRequest) -> Value {
    match &request.argument {
        InvokeArgument::Json(map) => Value::Object(map.clone()),
        InvokeArgument::None => Value::Object(serde_json::Map::new()),
        InvokeArgument::Text(text) => Value::String(text.clone()),
    }
}

/// The argument object `input` is invoked with.
#[must_use]
pub fn invoke_argument(input: Value) -> InvokeArgument {
    match input {
        Value::Object(map) => InvokeArgument::Json(map),
        Value::Null => InvokeArgument::None,
        Value::String(text) => InvokeArgument::Text(text),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("value".to_owned(), other);
            InvokeArgument::Json(map)
        }
    }
}

/// The output value a receipt that completed carries.
///
/// # Errors
/// The refusal, failure or cancellation it carries instead.
pub fn completed_value(receipt: InvocationReceipt) -> Result<Value, ActionCallError> {
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        ReceiptStatus::Rejected { error } => Err(error),
        ReceiptStatus::Failed { error } => Err(ActionCallError::Refused {
            reason: error.message,
        }),
        ReceiptStatus::Cancelled { .. } => Err(ActionCallError::Refused {
            reason: "the call was cancelled".to_owned(),
        }),
        ReceiptStatus::Accepted => Err(ActionCallError::NotReady),
    }
}

impl ActionProvider for Provider {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        if self.who(caller) != Ok(Who::Here)
            || query
                .provider
                .as_deref()
                .is_some_and(|provider| provider != self.name)
        {
            return Pending::ready(Ok(CataloguePage::default()));
        }
        let needle = query.needle.to_lowercase();
        let rows = self
            .entries
            .iter()
            .filter(|entry| entry.asker != Asker::Device)
            .filter(|entry| {
                needle.is_empty()
                    || entry.resource.contains(&needle)
                    || entry.label.to_lowercase().contains(&needle)
            })
            .map(|entry| CatalogueRow::for_action(self.name, self.name, Self::descriptor(entry)))
            .collect();
        Pending::ready(Ok(CataloguePage {
            rows,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(
            self.answer(caller, &request)
                .map(|value| InvocationReceipt {
                    invocation: request.invocation,
                    status: ReceiptStatus::Completed {
                        output: ActionOutput {
                            value: Some(value),
                            resources: Vec::new(),
                        },
                    },
                }),
        )
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        // Every answer is final at once: nothing is left to follow.
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "it answered at once".to_owned(),
        }))
    }
}

impl Provider {
    fn answer(&self, caller: &Caller, request: &InvokeRequest) -> Result<Value, ActionCallError> {
        if request.target.provider != self.name {
            return Err(ActionCallError::UnknownAction);
        }
        let resource = self
            .resource(&request.target.address)
            .ok_or(ActionCallError::UnknownAction)?;
        let action = request.target.address.action.as_str();
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.resource == resource && entry.action == action)
            .ok_or(ActionCallError::UnknownAction)?;
        let who = self.who(caller)?;
        let allowed = match (entry.asker, &who) {
            (Asker::Anyone, _) | (Asker::Here, Who::Here) | (Asker::Device, Who::Device(_)) => true,
            (Asker::Here, Who::Device(_)) | (Asker::Device, Who::Here) => false,
        };
        if !allowed {
            return Err(ActionCallError::NotPermitted);
        }
        (entry.handler)(&who, argument(request))
    }
}

/// The device that asked an action only another device asks.
///
/// # Errors
/// [`ActionCallError::NotPermitted`] when it was the person here.
pub fn asking_device(who: &Who) -> Result<&DeviceId, ActionCallError> {
    match who {
        Who::Device(device) => Ok(device),
        Who::Here => Err(ActionCallError::NotPermitted),
    }
}

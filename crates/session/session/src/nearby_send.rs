//! Nearby Send as the shell publishes it: the devices this one can hand a file
//! to, the offers another device made back, and the actions that answer them.
//!
//! The engine is the session's ([`crate::nearby_transfer`]); this publishes it
//! as a resource the shell's quick settings, an agent and a script read and
//! press. The resource's own shape and its actions live with the engine's
//! host ([`tilo_workspace_service::apps::nearby_send`]), so a call is checked
//! by the same code whether Files, the panel or a script made it.

use std::cell::RefCell;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_service::{Service, Started, reading};
use tilo_workspace_service::apps::nearby_send::{Invoked, NearbySend};

pub(crate) use tilo_workspace_service::apps::nearby_send::RESOURCE;

/// The session's Nearby Send resource: what the engine shows, and the actions
/// that change it.
pub(crate) struct Served {
    send: NearbySend,
    /// What the last pass reported, so a pass that changed nothing is no revision: a device that
    /// comes into reach or a transfer that moves is one.
    published: RefCell<Option<Value>>,
}

impl Served {
    pub(crate) fn new(send: NearbySend) -> Self {
        Self {
            send,
            published: RefCell::new(None),
        }
    }

    /// What the engine shows now, as the resource reads it.
    fn now(&self) -> Value {
        match self.send.read() {
            Ok(shown) => reading(&shown),
            Err(error) => {
                // The engine cannot say what it holds: the panel shows nothing rather than rows
                // it cannot act on.
                tracing::warn!(%error, "Nearby Send could not be read");
                reading(&tilo_workspace_service::apps::nearby_send::Reading::default())
            }
        }
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![tilo_workspace_service::apps::nearby_send::resource(
            crate::i18n::t("session-nearby-send"),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| self.now())
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        match self.send.invoke(resource, action, arguments)? {
            Invoked::Accepted => Ok(Started::Accepted),
            Invoked::Done(value) => Ok(Started::Done(tilo_actions::ActionOutput {
                value,
                ..tilo_actions::ActionOutput::default()
            })),
        }
    }

    fn poll(&mut self) -> bool {
        let now = self.now();
        let mut published = self.published.borrow_mut();
        if published.as_ref() == Some(&now) {
            return false;
        }
        *published = Some(now);
        true
    }
}

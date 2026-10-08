//! An authenticated native delivery's private lifecycle reference. This is not
//! the host's original caller authority or a final-owner mutation capability.
use super::{PublishedInvocationAdmission, PublishedInvocationLease, PublishedInvocationRequest};
use crate::collaboration::RouteSpec;
use std::sync::Arc;

/// Opaque lifecycle evidence attached only by the native invocation transport.
/// A caller cannot deserialize or construct this from action arguments.
#[derive(Clone)]
pub struct NativeInvocationDelivery {
    request: PublishedInvocationRequest,
    route: RouteSpec,
    admission: PublishedInvocationAdmission,
    check: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}

impl NativeInvocationDelivery {
    pub(crate) fn from_stream(
        request: PublishedInvocationRequest,
        route: RouteSpec,
        declaration: &str,
        check: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    ) -> Result<Self, String> {
        let admission = PublishedInvocationAdmission::decode(declaration)?;
        if admission.wire_id != request.wire_id || admission.invocation != request.invocation {
            return Err("native declaration belongs to another original invocation".into());
        }
        check()?;
        Ok(Self {
            request,
            route,
            admission,
            check,
        })
    }

    pub(crate) fn meter_bytes(&self) -> usize {
        let strings = self.request.address.resource.capacity()
            + self.request.address.action.capacity()
            + self.request.actor.capacity()
            + self.request.argument.as_ref().map_or(0, String::capacity)
            + self.route.app_id.capacity()
            + self.route.record.as_ref().map_or(0, String::capacity);
        std::mem::size_of::<Self>()
            + strings
            + serde_json::to_vec(&self.admission)
                .map_or(PublishedInvocationAdmission::MAX_BYTES, |bytes| bytes.len())
    }

    pub(crate) fn lease(
        &self,
        request: &PublishedInvocationRequest,
    ) -> Result<Option<PublishedInvocationLease>, String> {
        if request != &self.request {
            return Err("native delivery belongs to different original inputs".into());
        }
        if self.admission.spec.kind != tilo_actions::OperationKind::Effect {
            return Ok(None);
        }
        (self.check)()?;
        let mut lease =
            PublishedInvocationLease::new(request.clone(), self.route.clone(), self.check.clone())
                .with_dispatch_spec(self.admission.spec.clone())?;
        lease.native_effect = true;
        Ok(Some(lease))
    }
}

impl std::fmt::Debug for NativeInvocationDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeInvocationDelivery(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> (
        PublishedInvocationRequest,
        RouteSpec,
        PublishedInvocationAdmission,
    ) {
        let request = PublishedInvocationRequest {
            wire_id: 19,
            invocation: tilo_actions::InvocationId::new(),
            address: tilo_actions::ActionAddress::new("test.resource", "deliver"),
            argument: Some("original argument".into()),
            actor: "original-caller".into(),
            participant: None,
        };
        let spec = tilo_actions::ActionSpec::operation(
            "deliver",
            "Deliver",
            tilo_actions::OperationKind::Effect,
            "test.deliver",
            &(),
        )
        .unwrap();
        let declaration = PublishedInvocationAdmission {
            version: PublishedInvocationAdmission::VERSION,
            wire_id: request.wire_id,
            invocation: request.invocation,
            spec: super::super::PublishedInvocationSpec {
                kind: spec.kind,
                contract: spec.contract,
                params: vec![],
                input: tilo_actions::InputSpec::None,
                directory_revision: Some("original revision".into()),
            },
        };
        let route = RouteSpec {
            app_id: "so.tilo.test".into(),
            cluster_id: uuid::Uuid::new_v4(),
            record: Some("tab:0000000000000001".into()),
        };
        (request, route, declaration)
    }

    #[test]
    fn native_effect_declaration_rejects_unknown_malformed_oversize_and_foreign_originals() {
        let (request, route, original) = inputs();
        let mut malformed = vec![
            String::new(),
            "not JSON".into(),
            "x".repeat(PublishedInvocationAdmission::MAX_BYTES + 1),
        ];
        for change in [0, 1, 2, 3] {
            let mut declaration = original.clone();
            match change {
                0 => declaration.version += 1,
                1 => declaration.invocation = tilo_actions::InvocationId(uuid::Uuid::nil()),
                2 => declaration.invocation = tilo_actions::InvocationId::new(),
                _ => declaration.wire_id += 1,
            }
            malformed.push(serde_json::to_string(&declaration).unwrap());
        }
        for declaration in malformed {
            assert!(
                NativeInvocationDelivery::from_stream(
                    request.clone(),
                    route.clone(),
                    &declaration,
                    Arc::new(|| panic!("invalid declaration reached lifecycle admission"))
                )
                .is_err()
            );
        }
        let delivery = NativeInvocationDelivery::from_stream(
            request.clone(),
            route,
            &serde_json::to_string(&original).unwrap(),
            Arc::new(|| Ok(())),
        )
        .unwrap();
        let lease = delivery.lease(&request).unwrap().unwrap();
        assert_eq!(lease.dispatch_spec().unwrap(), &original.spec);
        assert!(lease.owner_token.is_none());
        let mut changed = request;
        changed.address.action = "foreign-action".into();
        assert!(delivery.lease(&changed).is_err());
        assert!(!format!("{delivery:?}").contains("original argument"));
    }
}

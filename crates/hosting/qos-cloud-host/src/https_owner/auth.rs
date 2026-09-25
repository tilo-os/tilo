//! The shared proof engine calls this only through the actual borrowed Store turn.
//! Descriptor/origin/boot/capability binding remains the carrier registry's work.
use super::CloudStoreTurn;
use qos_relay::delivery_v3::auth::{AuthError, Authority, Binding};
use qos_user_identity::DeviceCert;

impl Authority for CloudStoreTurn {
    fn authorize(
        &self,
        binding: &Binding,
        device: &DeviceCert,
        host: &DeviceCert,
        now_ns: u64,
    ) -> Result<(), AuthError> {
        let peer = self.peer();
        let now = crate::now_time(now_ns);
        let certificates = peer.certificates();
        if now_ns < self.now_ns()
            || binding.workspace != self.workspace()
            || binding.workspace_root != peer.authority().user_public_key
            || binding.device != device.device_public_key
            || binding.device_root != device.user_public_key
            || binding.host != host.device_public_key
            || binding.host_root != host.user_public_key
            || host != peer.authority()
            || !host.capabilities.can_act_as_host
            || !certificates.contains(device)
            || !certificates.contains(host)
            || peer.permissions(device.device_public_key, now).is_err()
            || peer.permissions(host.device_public_key, now).is_err()
            || peer.incomplete_reason().is_some()
        {
            return Err(AuthError::PolicyDenied);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::https_owner::{CloudStoreSource, Current};
    use qos_relay::delivery_v3::auth::Carrier;
    #[test]
    fn cloud_https_current_authority_requires_exact_live_scope_and_certificates() {
        let (_dir, mut store, device) = crate::agent_owner_tests::enrolled();
        store
            .activate_delivery_storage(qos_relay::clock::now_ns())
            .unwrap();
        let source = CloudStoreSource::bind(&store).unwrap();
        drop(store);
        assert!(matches!(
            source.with_current(|turn| {
                let device = turn
                    .peer()
                    .certificates()
                    .into_iter()
                    .find(|c| c.device_public_key == device)
                    .unwrap();
                let host = turn.peer().authority();
                let mut binding = Binding {
                    workspace: turn.workspace(),
                    workspace_root: host.user_public_key,
                    device: device.device_public_key,
                    device_root: device.user_public_key,
                    host: host.device_public_key,
                    host_root: host.user_public_key,
                    origin: "https://localhost:9443".into(),
                    descriptor_generation: 1,
                    descriptor_digest: [13; 32],
                    server_boot_id: [14; 16],
                    carrier: Carrier::Https,
                    requested_capabilities: 0,
                    mandatory_capabilities: 0,
                };
                turn.authorize(&binding, &device, host, turn.now_ns())
                    .unwrap();
                binding.workspace ^= 1;
                assert!(
                    turn.authorize(&binding, &device, host, turn.now_ns())
                        .is_err()
                );
                binding.workspace ^= 1;
                let mut replacement = device.clone();
                replacement.device_label.push_str(" replaced");
                assert!(
                    turn.authorize(&binding, &replacement, host, turn.now_ns())
                        .is_err()
                );
                assert!(
                    turn.authorize(&binding, &device, host, turn.now_ns() - 1)
                        .is_err(),
                    "caller cannot rewind the trusted owner turn"
                );
            }),
            Current::Current(())
        ));
    }
}

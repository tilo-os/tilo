//! A device its person's root confined to one workspace (a workspace's VM): every other workspace
//! of the person's refuses its key, and no workspace, its own included, gives it the group's keys.

use super::*;

/// The workspace the VM reaches, and another of the same person's.
const VM_WORKSPACE: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

/// The person's laptop (`a`), and their VM's certificate for [`VM_WORKSPACE`] alone, both
/// signed by the person's root.
fn laptop_and_vm() -> (SigningKey, DeviceCert, DeviceCert, SystemTime) {
    let (a, ac, _, _, now) = fixtures();
    let root = SigningKey::from_bytes(&[1; 32]);
    let vm = SigningKey::from_bytes(&[9; 32]);
    let mut vm_cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: vm.verifying_key().to_bytes(),
        device_label: "Workspace computer".into(),
        capabilities: DeviceCapabilities::workspace_device(
            *uuid::Uuid::parse_str(VM_WORKSPACE).unwrap().as_bytes(),
        ),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut vm_cert, &root).unwrap();
    (a, ac, vm_cert, now)
}

/// A workspace `workspace` of the person's, founded on their laptop, that lists the VM among
/// their devices.
fn workspace_listing_the_vm(workspace: &str) -> (StatePeer, DeviceCert, DeviceCert, SystemTime) {
    let (a, ac, vm, now) = laptop_and_vm();
    let roster = vec![ac.clone(), vm.clone()];
    let mut peer = StatePeer::new(
        workspace,
        ac.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(workspace, &ac, &roster).unwrap(),
            documents: Vec::new(),
        },
        &a,
        now,
    )
    .unwrap();
    (peer, ac, vm, now)
}

tilo_test_env::scenario! {
    fn a_device_confined_to_one_workspace_is_nothing_in_another;

    phase another_workspace_of_its_person_refuses_its_key;
    {
        let (peer, laptop, vm, now) = workspace_listing_the_vm(OTHER_WORKSPACE);
        assert!(peer.permissions(laptop.device_public_key, now).is_ok());
        assert!(matches!(
            peer.permissions(vm.device_public_key, now),
            Err(MembershipError::OtherWorkspace)
        ));
        let access = peer.governance().member_device_access(now).unwrap();
        assert!(access.contains_key(&laptop.device_public_key));
        assert!(
            !access.contains_key(&vm.device_public_key),
            "the VM holds no access in another workspace of its person's"
        );
    }

    phase another_workspace_stays_sealed_to_it;
    {
        let (peer, laptop, vm, now) = workspace_listing_the_vm(OTHER_WORKSPACE);
        let holders = peer.governance().key_holders(now).unwrap();
        assert!(holders.contains(&laptop.device_public_key));
        assert!(
            !holders.contains(&vm.device_public_key),
            "another workspace's keys are never sealed to the VM"
        );
    }

    phase the_workspace_it_reaches_admits_it_but_keeps_the_group_keys_from_it;
    {
        let (peer, laptop, vm, now) = workspace_listing_the_vm(VM_WORKSPACE);
        let access = peer.permissions(vm.device_public_key, now).unwrap();
        assert_eq!(access.confined_to.as_deref(), Some(VM_WORKSPACE));
        assert!(access.reaches_its_workspace());
        assert!(access.may_write_for(&tilo_action_policy::Acting::person(hex::encode(
            vm.user_public_key
        ))));
        let holders = peer.governance().key_holders(now).unwrap();
        assert!(holders.contains(&laptop.device_public_key));
        assert!(
            !holders.contains(&vm.device_public_key),
            "even the group keys of the workspace it reaches are not sealed to the VM"
        );
        // Its access, carried into another workspace's policy, writes for no one there.
        let mut elsewhere = access;
        elsewhere.policy = std::sync::Arc::new(tilo_action_policy::WorkspacePolicy::new(
            OTHER_WORKSPACE,
            hex::encode(vm.user_public_key),
        ));
        let person = tilo_action_policy::Acting::person(hex::encode(vm.user_public_key));
        assert!(!elsewhere.may_write_for(&person));
        assert!(elsewhere.invoked_by(&person).is_none());
    }
}

//! End-to-end integration: two devices certified under one user
//! identity each verify the other's certificate chains to the same
//! root, the "matching identities get owner permissions" check.

use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, UserIdentity, sign_device_cert,
    verify_device_cert_at,
};

tilo_test_env::scenario! {
    fn two_certified_devices_are_one_persons_end_to_end;

    phase both_devices_certify_under_one_root;
    {
        // 1. User provisions their root identity on their primary device.
        let (root_sk, user) = UserIdentity::generate("Ada Lovelace", 1_000_000);

        // 2. Primary and phone each generate their own device key; the
        //    person's root certifies both.
        let (_, primary_dev) = DeviceIdentity::generate("Framework 16", 1_100_000);
        let mut primary_cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: primary_dev.public_key,
            device_label: primary_dev.label,
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1_200_000,
            expires_at_ns: None,
            signature: [0u8; 64],
            issuer: None,
        };
        sign_device_cert(&mut primary_cert, &root_sk).unwrap();
        verify_device_cert_at(&primary_cert, 1_300_000).unwrap();

        let (_, phone_dev) = DeviceIdentity::generate("Ada's phone", 1_500_000);
        let mut phone_cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: phone_dev.public_key,
            device_label: phone_dev.label,
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1_600_000,
            expires_at_ns: None,
            signature: [0u8; 64],
            issuer: None,
        };
        sign_device_cert(&mut phone_cert, &root_sk).unwrap();

        // 3. "Matching-user peers get owner permissions on each
        //    other's shares" — expressed via the cert chain: both
        //    certs verify under the same user root, so the phone
        //    is considered part of the user's fleet by the primary
        //    and vice-versa.
        assert_eq!(primary_cert.user_public_key, phone_cert.user_public_key);
        verify_device_cert_at(&primary_cert, 1_750_000).unwrap();
        verify_device_cert_at(&phone_cert, 1_750_000).unwrap();
        assert!(primary_cert.capabilities.can_own_workspaces);
        assert!(phone_cert.capabilities.can_own_workspaces);
    }
}

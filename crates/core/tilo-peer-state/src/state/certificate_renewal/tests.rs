//! A member's renewed certificate keeps the device the same member: its
//! role stays, and only its person's root renews it.
use crate::StatePeer;
use crate::admission_tests::seeded;
use crate::join::Role;
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use web_time::{Duration, SystemTime};

use crate::fixture::member_cert_with;
const SECOND: u64 = 1_000_000_000;

fn cert(root: &SigningKey, device: &SigningKey, issued: u64, expires: Option<u64>) -> DeviceCert {
    member_cert_with(root, device, "Editor", issued, expires)
}

/// Admit `member` into `peer` as an Editor, through the owner's
/// invitation, at `at`.
fn admit(
    peer: &mut StatePeer,
    authority: &SigningKey,
    member: &DeviceCert,
    device: &SigningKey,
    at: SystemTime,
) {
    assert!(crate::fixture::join(
        peer,
        authority,
        (device, member),
        53,
        at
    ));
}

/// A workspace with an Editor whose certificate lasted from 1000s to 1500s, now
/// at 2000s expired; and a renewal of it the person's root issued at 1900s.
struct Fixture {
    peer: StatePeer,
    authority: SigningKey,
    root: SigningKey,
    device: SigningKey,
    held: DeviceCert,
    renewed: DeviceCert,
    now: SystemTime,
}

fn fixture() -> Fixture {
    fixture_of(&DeviceCapabilities::member())
}

/// [`fixture`], its Editor's certificates granting `capabilities`.
fn fixture_of(capabilities: &DeviceCapabilities) -> Fixture {
    let (mut peer, authority, _, _, now) = seeded();
    let root = SigningKey::from_bytes(&[5; 32]);
    let device = SigningKey::from_bytes(&[6; 32]);
    let granting = |mut cert: DeviceCert| {
        cert.capabilities = capabilities.clone();
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    };
    let held = granting(cert(&root, &device, 1000 * SECOND, Some(1500 * SECOND)));
    admit(
        &mut peer,
        &authority,
        &held,
        &device,
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(1200))
            .unwrap(),
    );
    let renewed = granting(cert(&root, &device, 1900 * SECOND, Some(2900 * SECOND)));
    Fixture {
        peer,
        authority,
        root,
        device,
        held,
        renewed,
        now,
    }
}

tilo_test_env::scenario! {
    fn a_certificate_is_renewed_only_by_a_later_one_of_its_root_and_role;

    phase a_renewed_certificate_takes_the_place_of_the_expired_one_and_keeps_the_role;
    {
        let Fixture {
            mut peer,
            authority,
            device,
            held,
            renewed,
            now,
            ..
        } = fixture();
        assert!(
            peer.permissions(held.device_public_key, now).is_err(),
            "an expired certificate acts in nothing"
        );
        assert_eq!(
            peer.governance().renewal_of(&renewed, now).unwrap(),
            Some(held.clone())
        );

        assert!(
            peer.governance_mut()
                .renew_member_certificate(&renewed, &authority, now)
                .is_err(),
            "no other device renews a device's certificate"
        );
        assert!(
            peer.governance_mut()
                .renew_member_certificate(&renewed, &device, now)
                .unwrap()
        );

        assert!(peer.certificates().contains(&renewed));
        assert!(!peer.certificates().contains(&held));
        assert_eq!(
            peer.role(&device.verifying_key().to_bytes(), now).unwrap(),
            Role::Editor,
            "the same member, with the same role"
        );
        assert_eq!(peer.governance().renewal_of(&renewed, now).unwrap(), None);
        assert!(
            !peer
                .governance_mut()
                .renew_member_certificate(&renewed, &device, now)
                .unwrap(),
            "renewing again changes nothing"
        );
    }

    phase a_certificate_from_another_root_is_not_a_renewal;
    {
        let Fixture {
            mut peer,
            device,
            now,
            ..
        } = fixture();
        let stranger = SigningKey::from_bytes(&[9; 32]);
        let forged = cert(&stranger, &device, 1900 * SECOND, Some(2900 * SECOND));
        assert!(peer.governance().renewal_of(&forged, now).is_err());
        assert!(
            peer.governance_mut()
                .renew_member_certificate(&forged, &device, now)
                .is_err()
        );
    }

    phase an_older_certificate_is_refused_after_a_newer_one;
    {
        let Fixture {
            mut peer,
            root,
            device,
            held,
            renewed,
            now,
            ..
        } = fixture();
        peer.governance_mut()
            .renew_member_certificate(&renewed, &device, now)
            .unwrap();
        // The certificate it replaced, and one issued between the two.
        let between = cert(&root, &device, 1800 * SECOND, Some(2800 * SECOND));
        for stale in [&held, &between] {
            let refused = peer.governance().renewal_of(stale, now).unwrap_err();
            assert!(refused.contains("newer"), "{refused}");
        }
        assert!(peer.certificates().contains(&renewed));
    }

    phase a_renewal_grants_no_more_than_the_certificate_it_replaces;
    {
        let Fixture {
            peer,
            root,
            device,
            now,
            ..
        } = fixture();
        let mut wider = cert(&root, &device, 1900 * SECOND, Some(2900 * SECOND));
        wider.capabilities.can_admit_devices = true;
        sign_device_cert(&mut wider, &root).unwrap();
        assert!(peer.governance().renewal_of(&wider, now).is_err());
    }

    phase the_root_lifts_the_cap_of_a_device_that_called_itself_foreign;
    {
        let Fixture {
            mut peer,
            root,
            device,
            now,
            ..
        } = fixture_of(&DeviceCapabilities {
            foreign: true,
            ..DeviceCapabilities::member()
        });
        // Its person's root certifies it again, as the device that holds the root does.
        let uncapped = cert(&root, &device, 1900 * SECOND, Some(2900 * SECOND));
        assert!(
            peer.governance_mut()
                .renew_member_certificate(&uncapped, &device, now)
                .unwrap()
        );
        assert!(peer.certificates().contains(&uncapped));
    }
}

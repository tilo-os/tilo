//! An expiring device certificate is renewed before it runs out, at start
//! and while the device runs, so expiry is never how a device loses its
//! rooms.
use super::*;
use std::cell::{Cell, RefCell};
use std::time::Duration;

const FILES: IdentityStorage = IdentityStorage::Files;
const HOUR: u64 = 60 * 60 * 1_000_000_000;

fn certificate(issued_at_ns: u64, expires_at_ns: Option<u64>, foreign: bool) -> DeviceCert {
    DeviceCert {
        user_public_key: [1; 32],
        device_public_key: [2; 32],
        device_label: "device".into(),
        capabilities: DeviceCapabilities {
            foreign,
            ..DeviceCapabilities::primary()
        },
        issued_at_ns,
        expires_at_ns,
        signature: [0; 64],
    }
}

#[test]
fn a_certificate_is_due_for_renewal_halfway_through_its_lifetime() {
    assert_eq!(
        renewal_due_at(&certificate(100, Some(300), false)),
        Some(200)
    );
    assert_eq!(renewal_due_at(&certificate(100, None, false)), None);
}

#[test]
fn every_class_reissues_an_expiring_certificate_once_it_is_due_and_not_before() {
    let expiring = certificate(100, Some(300), false);
    let foreign_expiring = certificate(100, Some(300), true);
    let lasting = certificate(100, None, false);
    for class in [DeviceClass::Native, DeviceClass::Foreign] {
        let held = if class == DeviceClass::Foreign {
            &foreign_expiring
        } else {
            &expiring
        };
        assert!(!class.must_reissue(held, 199), "{class:?} before it is due");
        assert!(class.must_reissue(held, 200), "{class:?} when it is due");
        assert!(class.must_reissue(held, 400), "{class:?} once it expired");
    }
    // What each class reissued before, it still does.
    assert!(!DeviceClass::Native.must_reissue(&lasting, 400));
    assert!(DeviceClass::Foreign.must_reissue(&lasting, 0));
    assert!(!DeviceClass::Foreign.must_reissue(&certificate(100, None, true), 400));
    #[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
    {
        assert!(DeviceClass::Throwaway.must_reissue(&lasting, 0));
        assert!(!DeviceClass::Throwaway.must_reissue(&expiring, 199));
        assert!(DeviceClass::Throwaway.must_reissue(&expiring, 200));
        assert!(DeviceClass::Throwaway.must_reissue(&expiring, 400));
    }
}

/// A throwaway identity in `tmp`, with its device certificate replaced by
/// one issued `age_ns` ago, as a session that ran that long ago left it.
#[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
fn aged_throwaway(tmp: &Path, age_ns: u64) -> (PathBuf, IdentityPaths, DeviceCert) {
    let personal = tmp.join("config/tilo/identity");
    let paths = IdentityPaths::under(tmp.join("system"));
    ensure_throwaway_test_identity(&personal, &paths, FILES).expect("provision");
    let identity =
        runtime_identity::load_from_path(personal.join(PRIMARY_IDENTITY_FILE)).expect("identity");
    let first = persist::read_device_cert(&paths.device_cert).expect("certificate");
    let aged = issue_device_certificate_at(
        &identity,
        first.device_public_key,
        &first.device_label,
        DeviceClass::Throwaway,
        tilo_clock::unix_now_ns().saturating_sub(age_ns),
    )
    .expect("aged certificate");
    persist::write_device_cert(&paths.device_cert, &aged).unwrap();
    persist::write_device_cert(
        &paths.device_cert_for_user(&identity.group_public_key()),
        &aged,
    )
    .unwrap();
    (personal, paths, aged)
}

#[test]
#[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
fn a_session_starting_after_its_certificate_expired_renews_it_and_keeps_its_device() {
    let tmp = tempfile::tempdir().unwrap();
    // The certificate lasted a day, and two have passed.
    let (personal, paths, expired) = aged_throwaway(tmp.path(), 48 * HOUR);
    assert!(expired.is_expired_at(tilo_clock::unix_now_ns()));

    // A session starts the way every session does.
    ensure_collaboration_identity(&personal, &paths, FILES).expect("start");

    let now = tilo_clock::unix_now_ns();
    for path in [
        paths.device_cert.clone(),
        paths.device_cert_for_user(&expired.user_public_key),
    ] {
        let renewed = persist::read_device_cert(&path).expect("renewed");
        tilo_user_identity::verify_device_cert_at(&renewed, now).expect("valid now");
        assert_eq!(renewed.device_public_key, expired.device_public_key);
        assert_eq!(renewed.user_public_key, expired.user_public_key);
        assert_eq!(renewed.device_label, expired.device_label);
        assert!(renewed.issued_at_ns > expired.issued_at_ns);
        assert!(
            renewed.expires_at_ns.is_some(),
            "a throwaway's renewed certificate still expires"
        );
    }
}

#[test]
#[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
fn a_session_keeps_a_certificate_not_yet_due() {
    let tmp = tempfile::tempdir().unwrap();
    let (personal, paths, young) = aged_throwaway(tmp.path(), HOUR);
    ensure_collaboration_identity(&personal, &paths, FILES).expect("start");
    assert_eq!(
        persist::read_device_cert(&paths.device_cert).unwrap(),
        young
    );
    assert_eq!(
        next_device_certificate_renewal(&personal, &paths),
        renewal_due_at(&young)
    );
}

/// A clock the renewal loop runs by in a test: it starts at `now`, moves
/// on by what it is asked to wait, and stops after `waits` waits.
struct TestClock {
    now: Cell<u64>,
    waited: RefCell<Vec<Duration>>,
    waits: usize,
}
impl RenewalClock for TestClock {
    fn now_ns(&self) -> u64 {
        self.now.get()
    }
    fn wait(&self, duration: Duration) -> bool {
        self.now.set(
            self.now
                .get()
                .saturating_add(u64::try_from(duration.as_nanos()).unwrap()),
        );
        self.waited.borrow_mut().push(duration);
        self.waited.borrow().len() < self.waits
    }
}

#[test]
#[cfg(any(debug_assertions, feature = "throwaway-test-identity"))]
fn a_running_device_renews_its_certificate_when_it_is_due_and_waits_for_the_next() {
    let tmp = tempfile::tempdir().unwrap();
    // Eleven hours into a day: due in one.
    let (personal, paths, held) = aged_throwaway(tmp.path(), 11 * HOUR);
    let identity =
        runtime_identity::load_from_path(personal.join(PRIMARY_IDENTITY_FILE)).expect("identity");
    let start = tilo_clock::unix_now_ns();
    let clock = TestClock {
        now: Cell::new(start),
        waited: RefCell::new(Vec::new()),
        waits: 2,
    };
    let renewals = Cell::new(0);
    keep_device_certificates_renewed(&personal, &paths, &clock, || {
        renewals.set(renewals.get() + 1);
        let renewed = issue_device_certificate_at(
            &identity,
            held.device_public_key,
            &held.device_label,
            DeviceClass::Throwaway,
            clock.now_ns(),
        )?;
        persist::write_device_cert(&paths.device_cert, &renewed)
            .map_err(|error| io::Error::other(error.to_string()))?;
        persist::write_device_cert(
            &paths.device_cert_for_user(&identity.group_public_key()),
            &renewed,
        )
        .map_err(|error| io::Error::other(error.to_string()))
    });
    let due = renewal_due_at(&held).unwrap();
    let waited = clock.waited.borrow();
    assert_eq!(renewals.get(), 1, "renewed once, when it was due");
    assert_eq!(
        waited[0],
        Duration::from_nanos(due.saturating_sub(start)),
        "slept until it was due"
    );
    assert_eq!(
        waited[1],
        Duration::from_nanos(12 * HOUR),
        "then slept until the renewed one is due"
    );
}

#[test]
fn a_device_whose_certificates_never_expire_has_nothing_to_renew() {
    let tmp = tempfile::tempdir().unwrap();
    let personal = tmp.path().join("config/tilo/identity");
    let paths = IdentityPaths::under(tmp.path().join("system"));
    ensure_collaboration_identity(&personal, &paths, FILES).expect("identity");
    assert_eq!(next_device_certificate_renewal(&personal, &paths), None);
    let clock = TestClock {
        now: Cell::new(0),
        waited: RefCell::new(Vec::new()),
        waits: 1,
    };
    keep_device_certificates_renewed(&personal, &paths, &clock, || panic!("nothing is due"));
    assert!(clock.waited.borrow().is_empty());
}

//! Compatibility and allocation checks for the actual signing preimage path.
use crate::canonical::{canonical_bytes, signing_digest};
use crate::{DeviceCapabilities, DeviceCert, sign_device_cert, verify_device_cert};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Serialize, Serializer};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}
struct CountedSystem;
fn record_allocation() {
    let _ = ALLOCATIONS.try_with(|counter| {
        if let Some(count) = counter.get() {
            counter.set(Some(count.saturating_add(1)));
        }
    });
}
// SAFETY: all requests are forwarded unchanged to System. The thread-local
// counter holds only a Copy scalar and does not allocate or touch user memory.
unsafe impl GlobalAlloc for CountedSystem {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        // SAFETY: forwards the caller's valid GlobalAlloc layout unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation();
        // SAFETY: forwards the caller's valid GlobalAlloc layout unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation();
        // SAFETY: forwards the original allocation, layout and requested size.
        unsafe { System.realloc(pointer, layout, size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwards the original allocation and layout unchanged.
        unsafe { System.dealloc(pointer, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: CountedSystem = CountedSystem;

fn measured<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ALLOCATIONS.with(|counter| counter.set(None));
        }
    }
    ALLOCATIONS.with(|counter| {
        assert!(counter.get().is_none());
        counter.set(Some(0));
    });
    let _reset = Reset;
    let value = operation();
    let count = ALLOCATIONS.with(|counter| counter.replace(None).unwrap());
    (value, count)
}

fn allocated_digest<T: Serialize>(tag: &[u8], value: &T) -> [u8; 32] {
    let body = canonical_bytes(value).unwrap();
    let mut hash = blake3::Hasher::new();
    hash.update(tag);
    hash.update(&body);
    *hash.finalize().as_bytes()
}

#[test]
fn signing_preimage_keeps_exact_bytes_without_a_temporary_allocation() {
    let bytes = vec![0xa5; 65_536];
    // Cover both bulk bytes and postcard's varint/element-at-a-time writes.
    for length in [0, 1, 127, 128, 1023, 1024, 65_536] {
        let value = (
            u64::MAX,
            Some("label \u{1f30d}"),
            serde_bytes::Bytes::new(&bytes[..length]),
        );
        let expected = allocated_digest(b"test-signing-v1\0", &value);
        let (actual, allocations) = measured(|| signing_digest(b"test-signing-v1\0", &value));
        assert_eq!(actual.unwrap(), expected);
        assert_eq!(
            allocations, 0,
            "temporary signing allocation at {length} bytes"
        );
    }
}

#[test]
fn existing_certificate_signatures_and_verification_keep_their_preimage() {
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut certificate = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "test device".repeat(128),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 128,
        expires_at_ns: Some(u64::MAX),
        signature: [0; 64],
    };
    // Postcard struct and tuple fields share the frozen, ordered wire shape.
    let view = (
        &certificate.user_public_key,
        &certificate.device_public_key,
        certificate.device_label.as_str(),
        &certificate.capabilities,
        certificate.issued_at_ns,
        certificate.expires_at_ns,
    );
    let expected = root.sign(&allocated_digest(crate::cert::DEVCERT_PREIMAGE_TAG, &view));
    sign_device_cert(&mut certificate, &root).unwrap();
    assert_eq!(certificate.signature, expected.to_bytes());
    // Verification remembers verified signatures in a process-wide set whose
    // first insert allocates its table. Warm it with a different certificate
    // so the measured call still takes the full (uncached) verification path
    // and only its preimage handling is counted.
    let mut warm = certificate.clone();
    warm.device_label = "warm-up device".to_owned();
    sign_device_cert(&mut warm, &root).unwrap();
    verify_device_cert(&warm).unwrap();
    // Tests on other threads insert into the same set; room for them keeps
    // the measured insert from being the one that grows it.
    crate::cert::verified_signatures()
        .lock()
        .unwrap()
        .reserve(1024);
    let (verified, allocations) = measured(|| verify_device_cert(&certificate));
    verified.unwrap();
    assert_eq!(
        allocations, 0,
        "certificate verification buffered its preimage"
    );
}

#[test]
fn signing_preserves_serialization_errors_after_partial_output() {
    struct Invalid;
    impl Serialize for Invalid {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq;
            let mut sequence = serializer.serialize_seq(Some(2))?;
            sequence.serialize_element(&42u64)?;
            Err(serde::ser::Error::custom("invalid remaining element"))
        }
    }
    assert_eq!(
        signing_digest(b"test-signing-v1\0", &Invalid).unwrap_err(),
        canonical_bytes(&Invalid).unwrap_err(),
    );
}

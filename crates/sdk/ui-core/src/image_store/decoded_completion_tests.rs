use super::*;
use crate::wake::WakeScope;

#[test]
fn native_close_during_fetch_cannot_publish_decoded_output() {
    let pumped = crate::background::Pumped::new();
    let scope = Arc::new(Mutex::new(WakeScope::new()));
    let closing = scope.clone();
    let fetch: Fetcher = Arc::new(move |_| {
        closing.lock().unwrap().close();
        Some(crate::png::encode_rgba8(2, 2, &[1, 2, 3, 255].repeat(4)).unwrap())
    });
    let store = {
        let _owner = scope.lock().unwrap().enter();
        let _executor = pumped.background().enter();
        ImageStore::new(&fetch)
    };
    store.get("closed-native");
    assert_eq!(store.pending(), 1);
    assert_eq!(pumped.run_pending(), 1);
    assert_eq!(store.pending(), 0);
    assert!(
        store.get("closed-native").is_none(),
        "closed owner received pixels"
    );
    assert!(
        !store.is_known("closed-native"),
        "closed owner received cache publication"
    );
}

fn pixels(width: u32, height: u32) -> tilo_image_codec::DecodedImage {
    tilo_image_codec::DecodedImage {
        rgba: [12, 23, 34, 255].repeat(width.saturating_mul(height) as usize),
        width,
        height,
    }
}

fn requested(store: &ImageStore, host: &mut HostImageLoader, url: &str) -> ImageCompletionTicket {
    assert!(store.get(url).is_none());
    assert_eq!(host.next_request().as_deref(), Some(url));
    host.reserve(url).expect("admitted request can be reserved")
}

#[test]
fn decoded_completion_publishes_exact_pixels_once_and_excludes_sync_completion() {
    let (store, mut host) = ImageStore::host_driven_with_max_dimension(2048);
    assert!(host.reserve("unknown").is_none());
    let ticket = requested(&store, &mut host, "image");
    assert_eq!(ticket.max_dimension(), 2048);
    assert!(host.reserve("image").is_none());
    assert!(!host.complete("image", None));
    assert_eq!(store.pending(), 1);
    assert!(host.complete_decoded(&ticket, Some(pixels(3, 2))));
    let image = store.get("image").unwrap();
    assert_eq!((image.width, image.height), (3, 2));
    assert_eq!(image.data.data(), &[12, 23, 34, 255].repeat(6));
    assert!(!host.complete_decoded(&ticket, Some(pixels(1, 1))));
    assert!(!host.cancel(&ticket));
    drop(ticket);
    assert_eq!(store.pending(), 0);
    assert_eq!(store.get("image").unwrap().width, 3);
}

#[test]
fn malformed_decoded_pixels_fail_and_allow_retry() {
    let (store, mut host) = ImageStore::host_driven_with_max_dimension(2048);
    for decoded in [
        tilo_image_codec::DecodedImage {
            rgba: vec![0; 3],
            width: 1,
            height: 1,
        },
        tilo_image_codec::DecodedImage {
            rgba: vec![0; 5],
            width: 1,
            height: 1,
        },
        tilo_image_codec::DecodedImage {
            rgba: vec![],
            width: 0,
            height: 1,
        },
        tilo_image_codec::DecodedImage {
            rgba: vec![],
            width: 1,
            height: 0,
        },
        pixels(2049, 1),
        tilo_image_codec::DecodedImage {
            rgba: vec![],
            width: u32::MAX,
            height: u32::MAX,
        },
    ] {
        let ticket = if store.is_known("image") {
            assert!(host.retry("image"));
            host.reserve("image").unwrap()
        } else {
            requested(&store, &mut host, "image")
        };
        assert!(host.complete_decoded(&ticket, Some(decoded)));
        assert_eq!(store.pending(), 0);
        assert!(!host.loaded("image"));
        assert!(store.get("image").is_none());
    }
}

#[test]
fn foreign_completion_and_cancel_leave_both_requests_untouched() {
    let (first, mut a) = ImageStore::host_driven();
    let (second, mut b) = ImageStore::host_driven();
    let ticket_a = requested(&first, &mut a, "same-url");
    let ticket_b = requested(&second, &mut b, "same-url");
    assert!(!b.complete_decoded(&ticket_a, Some(pixels(1, 1))));
    assert!(!b.cancel(&ticket_a));
    assert_eq!(first.pending(), 1);
    assert_eq!(second.pending(), 1);
    assert!(a.complete_decoded(&ticket_a, Some(pixels(2, 1))));
    assert!(b.complete_decoded(&ticket_b, Some(pixels(3, 1))));
    assert_eq!(first.get("same-url").unwrap().width, 2);
    assert_eq!(second.get("same-url").unwrap().width, 3);
}

#[test]
fn canceled_generation_cannot_settle_a_retry_or_cancel_its_pending_custody() {
    let (store, mut host) = ImageStore::host_driven();
    let old = requested(&store, &mut host, "retry");
    assert!(host.cancel(&old));
    assert!(!host.cancel(&old));
    assert_eq!(store.pending(), 0);
    assert!(host.retry("retry"));
    let new = host.reserve("retry").unwrap();
    assert!(!host.complete_decoded(&old, Some(pixels(1, 1))));
    assert!(!host.cancel(&old));
    drop(old);
    assert_eq!(store.pending(), 1);
    assert!(host.complete_decoded(&new, Some(pixels(4, 1))));
    assert_eq!(store.get("retry").unwrap().width, 4);
    assert_eq!(store.pending(), 0);
}

#[test]
fn dropped_ticket_settles_failure_before_retry() {
    let (store, mut host) = ImageStore::host_driven();
    let ticket = requested(&store, &mut host, "dropped");
    std::thread::spawn(move || drop(ticket)).join().unwrap();
    assert_eq!(store.pending(), 0);
    assert!(host.retry("dropped"));
    assert!(host.complete(
        "dropped",
        Some(&crate::png::encode_rgba8(1, 1, &[1, 2, 3, 255]).unwrap())
    ));
    assert!(store.get("dropped").is_some());
}

#[test]
fn loader_and_ticket_drop_in_either_order_settle_queued_and_reserved_once() {
    for ticket_first in [true, false] {
        let (store, mut host) = ImageStore::host_driven();
        let ticket = requested(&store, &mut host, "reserved");
        store.get("queued");
        store.get("handed-out");
        assert_eq!(host.next_request().as_deref(), Some("queued"));
        assert_eq!(store.pending(), 3);
        if ticket_first {
            drop(ticket);
            drop(host);
        } else {
            drop(host);
            assert_eq!(store.pending(), 0);
            drop(ticket);
        }
        assert_eq!(store.pending(), 0);
        for url in ["reserved", "queued", "handed-out"] {
            assert!(store.is_known(url));
            assert!(store.get(url).is_none());
        }
        store.get("disconnected");
        assert_eq!(store.pending(), 0);
    }
}

#[test]
fn owner_close_rejects_completion_and_drop_publication_but_releases_pending() {
    for finish in [0, 1, 2, 3] {
        let mut scope = WakeScope::new();
        let (store, mut host) = {
            let _owner = scope.enter();
            ImageStore::host_driven()
        };
        let ticket = requested(&store, &mut host, "closed");
        scope.close();
        match finish {
            0 => {
                assert!(!host.complete_decoded(&ticket, Some(pixels(1, 1))));
            }
            1 => {
                assert!(!host.cancel(&ticket));
            }
            2 => {
                drop(host);
            }
            _ => {
                drop(ticket);
            }
        }
        assert_eq!(store.pending(), 0);
        assert!(!store.is_known("closed"));
        assert!(store.get("closed").is_none());
        assert_eq!(store.pending(), 0);
    }
}

#[test]
fn dropping_old_ticket_after_cache_eviction_cannot_settle_same_url_again() {
    let (store, mut host) = ImageStore::host_driven();
    let old = requested(&store, &mut host, "evicted");
    assert!(host.complete_decoded(&old, Some(pixels(1, 1))));
    for index in 0..IMAGE_CACHE_ENTRY_LIMIT {
        let url = format!("other-{index}");
        let ticket = requested(&store, &mut host, &url);
        assert!(host.complete_decoded(&ticket, None));
    }
    assert!(!store.is_known("evicted"));
    let new = requested(&store, &mut host, "evicted");
    assert!(!host.complete_decoded(&old, Some(pixels(2, 1))));
    drop(old);
    assert_eq!(store.pending(), 1);
    assert!(host.complete_decoded(&new, Some(pixels(3, 1))));
    assert_eq!(store.get("evicted").unwrap().width, 3);
}

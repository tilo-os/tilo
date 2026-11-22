//! Settings' language packs travel on the same typed connection as the Store's
//! calls, with progress before the answer, and each app may make only its own
//! calls.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use package_service::language_pack::{LanguagePackInstalls, LanguagePacks};
use package_service::wire::{OfferedLanguage, Phase, Progress};

use crate::common;

#[test]
fn settings_reads_and_installs_a_pack_the_store_cannot_touch() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&attempts);
    let packs = LanguagePackInstalls::with_packs(LanguagePacks {
        offers: Box::new(|| {
            Ok(vec![OfferedLanguage {
                locale: "de-DE".into(),
                installed: false,
            }])
        }),
        installed: Box::new(|_| false),
        installer: Arc::new(move |locale, report| {
            assert_eq!(locale, "de-DE");
            counted.fetch_add(1, Ordering::SeqCst);
            report(Phase::Downloading, 4, Some(8));
            report(Phase::Installing, 8, Some(8));
            Ok(())
        }),
    });
    let h = common::Harness::with_language_packs(packs);

    let settings = common::settings_client(h.socket());
    assert_eq!(
        settings.language_packs().unwrap(),
        vec![OfferedLanguage {
            locale: "de-DE".into(),
            installed: false
        }]
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    settings
        .install_language_pack("de-DE", move |event| {
            recorder.lock().unwrap().push(event.progress);
        })
        .unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            Progress::LanguagePack {
                phase: Phase::Downloading,
                received: 4,
                total: Some(8),
            },
            Progress::LanguagePack {
                phase: Phase::Installing,
                received: 8,
                total: Some(8),
            },
        ],
        "progress arrives before the answer"
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 1);

    // The Store manages packages and not the person's languages.
    let store = common::store_client(h.socket());
    assert!(store.language_packs().is_err());
    assert!(store.install_language_pack("de-DE", |_| {}).is_err());
    // And Settings manages no packages.
    assert!(settings.list().is_err());
    assert!(
        settings
            .pin(package_service::PackageId::from_bytes([7; 32]), true)
            .is_err()
    );
}

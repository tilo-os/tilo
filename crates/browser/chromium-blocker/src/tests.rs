use super::*;

/// Third-party scripts are blocked but type and exception are honoured, cosmetic exceptions are
/// preserved and internal pages never injected, query-parameter rewrites are reported, and bundled
/// redirects and scriptlets are available.
#[test]
fn filtering_blocks_rewrites_and_hides_exactly_what_the_lists_say() {
    // Phase: blocks_third_party_script_but_honors_type_and_exception.
    {
        let blocker =
            Blocker::new("||ads.example^$script,third-party\n@@||ads.example/allowed.js$script");
        assert!(
            blocker
                .check(
                    "https://ads.example/ad.js",
                    "https://news.example",
                    "script"
                )
                .unwrap()["blocked"]
                .as_bool()
                .unwrap()
        );
        for (url, source, kind) in [
            (
                "https://ads.example/allowed.js",
                "https://news.example",
                "script",
            ),
            ("https://ads.example/ad.js", "https://ads.example", "script"),
            (
                "https://ads.example/ad.jpg",
                "https://news.example",
                "image",
            ),
        ] {
            assert_eq!(blocker.check(url, source, kind).unwrap()["blocked"], false);
        }
    }

    // Phase: preserves_cosmetic_exceptions_and_never_injects_internal_pages.
    {
        let blocker = Blocker::new("news.example##.sponsor\n##.advert\nnews.example#@#.advert");
        let result = blocker
            .cosmetics("https://news.example", &["advert".into()], &[])
            .unwrap();
        assert!(
            result["hide_selectors"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(".sponsor"))
        );
        assert!(
            !result["hide_selectors"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(".advert"))
        );
        assert!(
            blocker.cosmetics("chrome://settings", &[], &[]).unwrap()["hide_selectors"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    // Phase: reports_query_parameter_rewrite.
    {
        let blocker = Blocker::new("||news.example^$removeparam=tracking");
        let result = blocker
            .check(
                "https://news.example/article?tracking=abc&keep=1",
                "https://news.example",
                "document",
            )
            .unwrap();
        assert_eq!(result["blocked"], false);
        assert_eq!(
            result["rewritten_url"],
            "https://news.example/article?keep=1"
        );
    }

    // Phase: bundled_redirects_and_scriptlets_are_available.
    {
        let blocker = Blocker::new(
            "/replace.js$script,redirect=noopjs\nexample.com##+js(set-constant, tiloFixture, true)",
        );
        let result = blocker
            .check(
                "https://example.com/replace.js",
                "https://example.com",
                "script",
            )
            .unwrap();
        assert!(
            result["redirect"]
                .as_str()
                .unwrap_or("")
                .starts_with("data:")
        );
        let cosmetic = blocker.cosmetics("https://example.com", &[], &[]).unwrap();
        let script = cosmetic["injected_script"].as_str().unwrap();
        assert!(script.contains("tiloFixture"));
        assert!(script.contains("function"));
    }
}

/// The foreign API handles nulls and returns owned JSON, rejects oversized inputs before
/// dereference, rejects null required strings even with zero length, and reports a poisoned matcher
/// as an explicit cosmetic failure.
#[test]
fn the_foreign_api_rejects_bad_input_before_dereferencing_it() {
    // Phase: foreign_api_handles_nulls_and_returns_owned_json.
    {
        unsafe {
            assert!(tilo_blocker_create(std::ptr::null(), 5).is_null());
            assert!(
                tilo_blocker_match(
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    0
                )
                .is_null()
            );
            tilo_blocker_destroy(std::ptr::null_mut());
            tilo_blocker_string_free(std::ptr::null_mut());
            let rules = "||ads.example^";
            let handle = tilo_blocker_create(rules.as_ptr(), rules.len());
            assert!(!handle.is_null());
            let result = tilo_blocker_match(
                handle,
                b"https://ads.example/banner".as_ptr(),
                26,
                b"https://news.example".as_ptr(),
                20,
                b"image".as_ptr(),
                5,
            );
            assert!(!result.is_null());
            let json: serde_json::Value =
                serde_json::from_str(std::ffi::CStr::from_ptr(result).to_str().unwrap()).unwrap();
            assert_eq!(json["blocked"], true);
            tilo_blocker_string_free(result);
            tilo_blocker_destroy(handle);
        }
    }

    // Phase: oversized_foreign_inputs_are_rejected_before_dereference.
    {
        unsafe {
            let handle = tilo_blocker_create(std::ptr::null(), 0);
            assert!(
                tilo_blocker_match(
                    handle,
                    std::ptr::dangling(),
                    65537,
                    b"".as_ptr(),
                    0,
                    b"image".as_ptr(),
                    5
                )
                .is_null()
            );
            assert!(
                tilo_blocker_cosmetics(
                    handle,
                    b"https://example.org".as_ptr(),
                    19,
                    std::ptr::dangling(),
                    1048577,
                    b"[]".as_ptr(),
                    2
                )
                .is_null()
            );
            tilo_blocker_destroy(handle);
        }
    }

    // Phase: required_foreign_strings_reject_null_even_with_zero_length.
    {
        unsafe {
            let handle = tilo_blocker_create(std::ptr::null(), 0);
            assert!(
                tilo_blocker_match(
                    handle,
                    std::ptr::null(),
                    0,
                    b"https://example.org".as_ptr(),
                    19,
                    b"image".as_ptr(),
                    5
                )
                .is_null()
            );
            assert!(
                tilo_blocker_cosmetics(
                    handle,
                    std::ptr::null(),
                    0,
                    b"[]".as_ptr(),
                    2,
                    b"[]".as_ptr(),
                    2
                )
                .is_null()
            );
            tilo_blocker_destroy(handle);
        }
    }

    // Phase: poisoned_matcher_is_an_explicit_cosmetic_failure.
    {
        let blocker = Blocker::new("");
        blocker.panic_worker_for_test();
        assert!(blocker.cosmetics("https://example.org", &[], &[]).is_none());
    }
}

/// The blocker is Send and Sync, and one blocker serves multiple calling threads.
#[test]
fn one_blocker_is_shareable_across_threads() {
    // Phase: blocker_is_send_and_sync.
    {
        fn assert_thread_safe<T: Send + Sync>() {}
        assert_thread_safe::<Blocker>();
    }

    // Phase: one_blocker_serves_multiple_calling_threads.
    {
        let blocker = Blocker::new("||ads.example^");
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    assert_eq!(
                        blocker
                            .check(
                                "https://ads.example/banner",
                                "https://news.example",
                                "image",
                            )
                            .unwrap()["blocked"],
                        true
                    );
                });
            }
        });
    }
}

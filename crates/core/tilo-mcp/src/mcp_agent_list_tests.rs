//! A listing pages its typed rows as it pages its text: what a cursor's
//! offset leaves behind is not on this page.

use super::json;
use super::tests::{Notes, agent, answer};

tilo_test_env::scenario! {
    fn a_listing_pages_its_typed_rows_as_it_pages_its_lines;

    phase the_apps_listing_answers_one_page_of_rows;
    // `tilo sdk gen -w` reads every page of the apps listing; a page that
    // answers every app makes it write each app twice.
    {
        let notes = Notes::new(false);
        let server = agent(&notes);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (first, failed) = answer(&server, "tilo_ls", json!({"limit": 1})).await;
            assert!(!failed, "{first}");
            assert_eq!(first["next_cursor"], "1", "{first}");
            let (second, failed) = answer(
                &server,
                "tilo_ls",
                json!({"limit": 1, "cursor": "1"}),
            )
            .await;
            assert!(!failed, "{second}");
            let rows = |page: &serde_json::Value| {
                page["rows"].as_array().cloned().unwrap_or_default()
            };
            assert_eq!(rows(&first).len(), 1, "{first}");
            assert_eq!(rows(&second).len(), 1, "{second}");
            assert_ne!(
                rows(&first)[0]["path"],
                rows(&second)[0]["path"],
                "each page holds other apps"
            );
        });
    }
}

//! The action listing the control socket answers `actions_enumerate` with,
//! read through the same provider registry as the palette. A paged
//! listing is read once, on its first page, and its continuations are cut
//! from that reading ([`crate::compositor::listing_snapshots`]).

use automerge::{AutoCommit, ObjId};

use super::{FrameBuilderControlState, params_json, test_control, with_action_gate};

impl FrameBuilderControlState<'_> {
    /// Read the action listing for `query` now: its rows (arranged as a
    /// paged listing is, files only when asked), its launcher and whether
    /// nothing it would list is still on its way.
    fn read_listing(
        &self,
        query: &str,
        include_programmatic: bool,
        page: Option<&test_control::ListingRequest>,
        doc: &AutoCommit,
        root: &ObjId,
    ) -> Result<
        (
            crate::shell::search::action_api::ActionCatalog,
            Option<serde_json::Value>,
            bool,
        ),
        test_control::ActionRefusal,
    > {
        let chrome_handle = self.fb.chrome_handle();
        // The borrow lives only for the closure; `ActionCatalog` owns its results
        // outright, so the catalogue outlives the guards it was built through.
        let (mut catalog, launcher, complete) = with_action_gate(&chrome_handle, |chrome, gate| {
            let args = crate::shell::search::QueryArgs {
                needle: query,
                scope: crate::shell::chrome_state::SearchScope::All,
                active_provider_id: None,
                wanted_kinds: &[],
                chrome,
                doc,
                doc_root: root,
            };
            let first_use_gate = crate::compositor::admin_control::FirstUseVisibility {
                base: gate,
                first_use: self.fb.host().onboarding_active(),
            };
            let catalog =
                self.fb
                    .host()
                    .action_catalog_query(&args, &first_use_gate, include_programmatic);
            let launcher = self.fb.host().launcher_answer(&args, page, &first_use_gate);
            // Read after the query, which may itself have started the work
            // that leaves the listing incomplete.
            let complete = self.fb.host().action_listing_complete(&args);
            (catalog, launcher, complete)
        })
        .map_err(test_control::ActionRefusal::listing_failed)?;
        // A paged listing is an agent's: apps' own resources and actions
        // first, files only when asked.
        if let Some(page) = page {
            catalog.arrange(&page.kinds, page.files);
        }
        Ok((catalog, launcher, complete))
    }

    /// The action listing for `query` through the *same* provider registry
    /// that feeds the Ctrl+K palette — but rendering nothing. With `page`,
    /// that page of it ([`test_control::ListingRequest`]); without, all of it.
    /// Only the answered rows become JSON: describing every row of a large
    /// catalogue cost more than building it.
    pub(super) fn listing(
        &self,
        query: &str,
        include_programmatic: bool,
        page: Option<&test_control::ListingRequest>,
    ) -> Result<serde_json::Value, test_control::ActionRefusal> {
        let (doc, root) = self.doc.ok_or_else(|| {
            test_control::ActionRefusal::listing_failed("control state has no live document")
        })?;
        // A continuation of a listing held since its first page is cut from
        // that reading; anything else reads the listing now.
        let key = page.map(|page| {
            crate::compositor::listing_snapshots::ListingKey::new(query, include_programmatic, page)
        });
        let held = key
            .as_ref()
            .zip(page)
            .and_then(|(key, page)| self.listings?.find(key, page.cursor_listing()?));
        let (reading, launcher) = if let Some(reading) = held {
            (reading, None)
        } else {
            let (catalog, launcher, complete) =
                self.read_listing(query, include_programmatic, page, doc, root)?;
            let fingerprint = test_control::listing_fingerprint(catalog.addresses());
            (
                crate::compositor::listing_snapshots::ListingReading {
                    catalog: std::rc::Rc::new(catalog),
                    complete,
                    fingerprint,
                },
                launcher,
            )
        };
        let catalog = &*reading.catalog;
        let complete = reading.complete;
        let fingerprint = reading.fingerprint;
        let window = page
            .map(|page| page.window(catalog.len(), fingerprint))
            .transpose()?;
        if let (Some(listings), Some(key), Some(window)) = (self.listings, key, window.as_ref()) {
            if window.next_cursor.is_some() {
                listings.hold(key, reading.clone());
            } else {
                listings.release(&key, fingerprint);
            }
        }
        let compact = page.is_some_and(|page| !page.detail.is_full());
        let rows = window
            .as_ref()
            .map_or(0..catalog.len(), |window| window.rows.clone());
        let actions: Vec<serde_json::Value> = catalog
            .enumerate_rows(rows)
            .into_iter()
            .map(|a| {
                let hierarchy = a.hierarchy.as_ref();
                let mut row = serde_json::json!({
                    "provider": a.resource.provider,
                    "id": a.resource.id,
                    "kind": a.kind.id(),
                    "operation_kind": a.operation_kind,
                    "title": a.title,
                    "subtitle": a.subtitle,
                    "score": a.score,
                    // What to pass; empty for the nullary majority. A parameter list describes
                    // an invocation, it is not one, so enumeration stays side-effect-free.
                    "params": params_json(&a.params),
                    "contract": a.contract,
                    // The provider resolves identity from its authenticated source;
                    // opaque invocation IDs and presentation labels stay independent.
                    "application": hierarchy.map(|value| &value.application),
                    "room": hierarchy.map(|value| &value.room),
                    "record": hierarchy.map(|value| &value.record),
                    "resource": hierarchy.map(|value| &value.resource),
                    "semantic_identity": hierarchy.and_then(|value| value.semantic_identity.as_ref()),
                    "operation": hierarchy.map(|value| &value.operation),
                });
                if compact && let Some(row) = row.as_object_mut() {
                    row.remove("contract");
                }
                row
            })
            .collect();
        let mut answer = serde_json::Map::new();
        answer.insert("query".into(), query.into());
        answer.insert("count".into(), actions.len().into());
        answer.insert("actions".into(), actions.into());
        if let Some(launcher) = launcher {
            answer.insert("launcher".into(), launcher);
        }
        if let Some(window) = window {
            window.describe(&mut answer);
        }
        // Which listing this is — the identity its cursors are cut from — and
        // whether it is final. `truncated` says this page is not all of the
        // listing; `complete: false` says the listing itself is not all there
        // yet: an index build or an app's directory page that would add to it
        // is still on its way. `wait_ms` waits for that.
        answer.insert("revision".into(), format!("{fingerprint:016x}").into());
        answer.insert(
            "complete".into(),
            (complete && catalog.completion().complete).into(),
        );
        answer.insert("completion".into(), serde_json::json!(catalog.completion()));
        Ok(serde_json::Value::Object(answer))
    }
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the frame builder lives for the whole scenario by design"
)]
mod tests {
    use automerge::AutoCommit;

    use super::super::{FrameBuilderControlState, test_control};
    use crate::compositor::listing_snapshots::ListingSnapshots;
    use crate::shell::frame::FrameBuilder;
    use test_control::ControlState as _;

    /// One page of the listing for `query` at `cursor`, three rows long,
    /// from a control state over `fb` keeping its readings in `listings`.
    fn page(
        fb: &FrameBuilder,
        doc: &AutoCommit,
        listings: Option<&ListingSnapshots>,
        cursor: Option<String>,
    ) -> Result<serde_json::Value, test_control::ActionRefusal> {
        FrameBuilderControlState {
            fb,
            doc: Some((doc, &automerge::ROOT)),
            shares: None,
            lock: None,
            seat: None,
            resolved: None,
            listings,
        }
        .enumerate_actions_with_programmatic(
            "",
            false,
            &test_control::ListingRequest {
                limit: Some(3),
                cursor,
                detail: test_control::ListingDetail::Compact,
                ..test_control::ListingRequest::default()
            },
        )
    }

    fn ids(answer: &serde_json::Value) -> Vec<String> {
        answer["actions"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|row| format!("{}:{}", row["provider"], row["id"]))
            .collect()
    }

    /// A paged listing reads the desktop once, on its first page: the
    /// desktop changing between two pages shows up on the next listing, and
    /// the cursor in hand walks the listing it was cut from to its end
    /// instead of going stale. Without the readings held, the same cursor is
    /// stale, which is what a caller looped on.
    #[test]
    fn a_paged_listing_reads_one_snapshot_from_its_first_page_to_its_last() {
        let mut fb = FrameBuilder::new((1600, 900));
        let doc = AutoCommit::new();
        let snapshots = ListingSnapshots::default();
        let first = page(&fb, &doc, Some(&snapshots), None).expect("the first page");
        let revision = first["revision"].as_str().expect("revision").to_owned();
        let total = first["total"].as_u64().expect("total");
        let mut seen = ids(&first);
        let mut cursor = first["next_cursor"].as_str().map(str::to_owned);
        assert!(cursor.is_some(), "the demo desktop lists more than a page");

        // The desktop changes between two pages: setup starts, and the
        // listing narrows to what setup offers.
        let identity =
            std::env::temp_dir().join(format!("tilo-listing-snapshot-{}", std::process::id()));
        fb.host_mut().enable_onboarding(identity.clone());
        let now = page(&fb, &doc, Some(&snapshots), None).expect("a new listing");
        assert_ne!(
            now["revision"].as_str(),
            Some(revision.as_str()),
            "the change is one the listing shows"
        );
        let unheld = page(&fb, &doc, None, cursor.clone());
        assert!(
            unheld.is_err_and(|refusal| refusal.code == "stale_cursor"),
            "read afresh, the cursor's listing is gone"
        );

        while let Some(next) = cursor.take() {
            let page = page(&fb, &doc, Some(&snapshots), Some(next))
                .expect("a continuation of the held listing");
            assert_eq!(page["revision"].as_str(), Some(revision.as_str()));
            assert_eq!(page["total"], total);
            seen.extend(ids(&page));
            cursor = page["next_cursor"].as_str().map(str::to_owned);
        }
        let _ = std::fs::remove_dir_all(&identity);
        assert_eq!(
            u64::try_from(seen.len()).ok(),
            Some(total),
            "every row of the listing"
        );
        let mut unique = seen.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), seen.len(), "each row once");
    }
}

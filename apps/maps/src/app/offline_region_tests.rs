use super::*;

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn offline_regions_record_delete_and_clear_without_losing_shared_tiles() {
    // Phase `offline_clear_cache_dispatch_removes_dir`.
    // Clearing the cache deletes the on-disk tile tree and zeroes the readout —
    // exercised against a disposable temp dir, never the user's real cache.
    {
        let mut a = app_at(1280.0, 720.0);
        let uniq = ui::time::unix_now_ns();
        let dir =
            std::env::temp_dir().join(format!("tilo-maps-cctest-{}-{}", std::process::id(), uniq));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("seed.png"), b"not-a-real-tile").unwrap();
        a.tiles = TileLayer::offline_with_cache_dir(dir.clone());
        assert!(a.tiles.cache_bytes() > 0, "seeded cache is non-empty");

        tap_offline(&mut a, OfflineAction::ClearCache);
        assert!(!dir.exists(), "clear-cache removes the on-disk tree");
        assert_eq!(
            a.offline_panel.cache_bytes(),
            0,
            "the cache-size readout resets to 0"
        );
        assert!(
            a.model.status.starts_with("Cleared"),
            "a status states what was freed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // Phase `offline_download_records_and_delete_removes_region`.
    // Starting a download records the chosen viewport as a **manageable
    // region** (so it shows in the saved-areas list), and deleting it by id
    // removes the record. This is the see/manage-downloaded-regions flow.
    {
        let mut a = app_at(1280.0, 800.0);
        // Isolate the on-disk cache so the delete step never touches the user's
        // real tile cache.
        let dir = region_test_cache("regtest");
        a.tiles = TileLayer::offline_with_cache_dir(dir.clone());
        a.tiles.allow_prefetch_for_test();
        a.offline_panel.open(a.tiles.cache_bytes());
        let before = a.model.regions.len();
        tap_offline(&mut a, OfflineAction::Download);
        assert_eq!(
            a.model.regions.len(),
            before + 1,
            "a downloaded area is recorded as a saved region"
        );
        let region = a.model.regions.last().unwrap().clone();
        assert!(region.tiles > 0, "the region covers real tiles");
        assert!(
            region.z1 >= region.z0,
            "the saved zoom range is well-formed"
        );
        assert!(!region.name.is_empty(), "the region has a human label");

        tap_offline(&mut a, OfflineAction::DeleteRegion(region.id.clone()));
        assert!(
            !a.model.regions.iter().any(|r| r.id == region.id),
            "delete removes the region from the managed list"
        );
        assert_eq!(
            a.model.regions.len(),
            before,
            "back to the starting region count"
        );
        assert!(
            a.model.status.contains("Removed"),
            "a status confirms the removal"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // Phase `delete_region_keeps_tiles_shared_with_another_area`.
    // Removing one saved area must NOT delete tiles another saved area still
    // covers. A nested area's tiles are a subset of the containing area's, so
    // deleting the nested one frees nothing; deleting the last owner frees all.
    {
        use crate::model::DownloadedRegion;
        let dir = region_test_cache("region");
        let mut a = MapsApp::offline();
        a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        a.tiles = TileLayer::offline_with_cache_dir(dir.clone());

        let big = GeoBounds::new(LatLon::new(48.10, 11.55), LatLon::new(48.18, 11.62));
        let small = GeoBounds::new(LatLon::new(48.13, 11.57), LatLon::new(48.14, 11.58));
        let (z0, z1) = (14u8, 15u8);
        let big_ids = big.tiles_in_range(z0, z1);
        let small_ids = small.tiles_in_range(z0, z1);
        assert!(!small_ids.is_empty());
        assert!(
            small_ids.iter().all(|t| big_ids.contains(t)),
            "the nested area's tiles are a subset of the containing area's"
        );

        // Seed the on-disk cache with the containing area's tiles.
        for id in &big_ids {
            let p = a.tiles.cache_path(*id);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"seed").unwrap();
        }
        a.model.regions = vec![
            DownloadedRegion {
                id: "big".to_string(),
                name: "Big".to_string(),
                min: big.min,
                max: big.max,
                z0,
                z1,
                tiles: big_ids.len() as u32,
            },
            DownloadedRegion {
                id: "small".to_string(),
                name: "Small".to_string(),
                min: small.min,
                max: small.max,
                z0,
                z1,
                tiles: small_ids.len() as u32,
            },
        ];

        // Deleting the nested area removes its record but keeps every tile,
        // since the containing area still needs them.
        tap_offline(&mut a, OfflineAction::DeleteRegion("small".to_string()));
        assert!(!a.model.regions.iter().any(|r| r.id == "small"));
        for id in &big_ids {
            assert!(
                a.tiles.cache_path(*id).exists(),
                "a shared tile must be kept: {id:?}"
            );
        }

        tap_offline(&mut a, OfflineAction::DeleteRegion("big".to_string()));
        for id in &big_ids {
            assert!(
                !a.tiles.cache_path(*id).exists(),
                "the last owner's tiles are freed: {id:?}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}

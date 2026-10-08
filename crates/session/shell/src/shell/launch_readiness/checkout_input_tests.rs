// Actual signed directory, authenticated Wayland adoption and measured input layout.
#[tokio::test(flavor = "current_thread")]
async fn certified_checkout_survives_actual_native_input_geometry_projection() {
    tokio::task::LocalSet::new().run_until(async {
        let checkout = tilo_action_broker::collab_broker_capnp::tests::NativeCheckoutFixture::new().await;
        let room = crate::shell::state::RoomId::parse(&checkout.route.cluster_id).unwrap();
        let mut wire = Wire::with_identity(room, &checkout.route.app_id, true);
        let _registry = wire.connection.display().get_registry(&wire.queue.handle(), ());
        wire.pump();
        let compositor = wire.bind::<wl_compositor::WlCompositor>("wl_compositor", 4);
        let wm = wire.bind::<xdg_wm_base::XdgWmBase>("xdg_wm_base", 1);
        let surface = compositor.create_surface(&wire.queue.handle(), ());
        let xdg = wm.get_xdg_surface(&surface, &wire.queue.handle(), ());
        let top = xdg.get_toplevel(&wire.queue.handle(), ());
        top.set_app_id(checkout.route.app_id.clone());
        wire.pump();
        let tree = wire.state.workspace_tree.clone();
        let tab = {
            let mut tree = tree.write();
            let tab = tree.workspaces[0].strips.iter().flat_map(|s| &s.window_groups)
                .flat_map(|g| &g.tabs).find(|t| matches!(t.tile, crate::shell::state::TileContent::Foreign { .. }))
                .expect("actual adopted native toplevel").id;
            tree.remember_local_app_record(tab, 73);
            tab
        };
        let mut frame = crate::compositor::frame::FrameBuilder::with_handle(tree.clone(), false, (800, 600));
        let _ = frame.paint_settled();
        let proof = wire.state.native_window_input_provenance_with_broker(&frame, Some(&checkout.broker));
        let rows = proof["windows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row["tab_id"], serde_json::to_value(tab).unwrap());
        assert_eq!(row["peer_pid"], std::process::id());
        assert_eq!(row["certified_checkout"], serde_json::json!({
            "application": checkout.route.app_id, "room": checkout.route.cluster_id,
            "record": checkout.route.record_id,
        }));
        let surface_id = row["surface_id"].as_u64().unwrap();
        let hit = crate::shell::input_policy::foreign_tile_for_surface(&tree.read(), frame.host(), surface_id)
            .expect("actual painted native input target");
        assert_eq!(row["input_rect"], serde_json::json!([hit.rect.x0,hit.rect.y0,hit.rect.x1,hit.rect.y1]));
        assert!(hit.rect.width() > 1.0 && hit.rect.height() > 1.0);
        assert!(!proof["input_outputs"].as_array().unwrap().is_empty());
        assert!(wire.state.native_window_input_provenance_json(&frame)["windows"][0]["certified_checkout"].is_null());
        tree.write().local_app_records.get_mut(&tab).unwrap().record_instance = 74;
        assert!(wire.state.native_window_input_provenance_with_broker(&frame, Some(&checkout.broker))["windows"][0]["certified_checkout"].is_null());
        tree.write().local_app_records.get_mut(&tab).unwrap().record_instance = 73;
        assert_eq!(wire.state.native_window_input_provenance_with_broker(&frame, Some(&checkout.broker)), proof);
        checkout.revoke();
        assert!(wire.state.native_window_input_provenance_with_broker(&frame, Some(&checkout.broker))["windows"][0]["certified_checkout"].is_null());
        checkout.disconnect();
        top.destroy(); xdg.destroy(); surface.destroy(); wire.pump();
        assert_eq!(wire.state.native_window_input_provenance_with_broker(&frame, Some(&checkout.broker))["windows"], serde_json::json!([]));
        drop(frame);
        drop(checkout);
    }).await;
}

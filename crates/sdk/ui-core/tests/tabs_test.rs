#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::widget::{TabBar, TabDescriptor};

#[test]
fn tab_bar_activates_reorders_and_stubs_render() {
    // Case `tabbar_activates_by_index`.
    {
        let mut bar = TabBar::new(
            "g1",
            vec![
                TabDescriptor::new(1, "A"),
                TabDescriptor::new(2, "B"),
                TabDescriptor::new(3, "C"),
            ],
            2,
        );
        assert_eq!(bar.active_id(), 2);
        bar.set_active(3);
        assert_eq!(bar.active_id(), 3);
    }

    // Case `tabbar_reorder_swaps_positions`.
    {
        let mut bar = TabBar::new(
            "g1",
            vec![
                TabDescriptor::new(1, "A"),
                TabDescriptor::new(2, "B"),
                TabDescriptor::new(3, "C"),
            ],
            1,
        );
        bar.reorder(0, 2);
        let ids: Vec<u64> = bar.tabs().iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![2, 3, 1]);
    }

    // Case `tile_content_stub_renders_label`.
    {
        let tc = TileContent::Stub {
            kind: AppKind::Browser,
            label: "Browser".into(),
        };
        assert_eq!(TileHost::new(tc).kind_string(), "Browser");
    }
}

use tilo_ui::widget::{AppKind, TileContent, TileHost};

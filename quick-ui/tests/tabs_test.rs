use quick_ui::widget::{TabBar, TabDescriptor};

#[test]
fn tabbar_activates_by_index() {
    let mut bar = TabBar::new("g1", vec![
        TabDescriptor { id: 1, title: "A".into() },
        TabDescriptor { id: 2, title: "B".into() },
        TabDescriptor { id: 3, title: "C".into() },
    ], 2);
    assert_eq!(bar.active_id(), 2);
    bar.set_active(3);
    assert_eq!(bar.active_id(), 3);
}

#[test]
fn tabbar_reorder_swaps_positions() {
    let mut bar = TabBar::new("g1", vec![
        TabDescriptor { id: 1, title: "A".into() },
        TabDescriptor { id: 2, title: "B".into() },
        TabDescriptor { id: 3, title: "C".into() },
    ], 1);
    bar.reorder(0, 2);
    let ids: Vec<u64> = bar.tabs().iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![2, 3, 1]);
}

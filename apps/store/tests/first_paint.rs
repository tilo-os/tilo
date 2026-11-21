//! A new Store window paints the catalogue the last window kept in its
//! first frame: the catalogue is read from the disk cache and laid out
//! before anything is asked of the network or the app host.

#![allow(
    clippy::panic,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use store::StoreApp;
use ui::headless::Framed;
use ui::widget::Widget;

/// A catalogue the size of Flathub's: every fixture app many times over,
/// each with its own id and name.
fn flathub_sized() -> Vec<store::StoreItem> {
    let base = store::fixture::fixture_catalog();
    base.iter()
        .cycle()
        .take(3000)
        .enumerate()
        .map(|(index, item)| {
            let mut item = item.clone();
            item.id = format!("{}.n{index}", item.id);
            item.name = format!("{} {index}", item.name);
            item
        })
        .collect()
}

fn names(root: &dyn Widget, out: &mut Vec<String>) {
    if let Some(name) = root.accessibility().and_then(|node| node.name) {
        out.push(name);
    }
    root.for_each_child(&mut |child| names(child, out));
}

/// Read the kept catalogue back and lay out the first frame, as a new window
/// does; how long that took.
fn first_frame(shown: &mut Vec<String>) -> Duration {
    let dir = tempfile::tempdir().expect("a cache directory");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_STORE_CACHE_DIR", dir.path());
    store::catalogue::cache::save(&flathub_sized(), "en-US").expect("the catalogue is kept");

    let started = Instant::now();
    let cached = store::catalogue::cache::load().expect("the kept catalogue reads back");
    let app = StoreApp::new();
    app.state().set_items(cached.items);
    let framed = Framed::new(
        ui::State::new(),
        app,
        ui::headless::viewport((1280.0, 800.0)),
        ui::Theme::dark(),
    );
    let took = started.elapsed();
    env.remove("TILO_STORE_CACHE_DIR");

    names(&*framed, shown);
    took
}

fn assert_shows_real_apps(shown: &[String]) {
    assert!(
        shown
            .iter()
            .any(|name| name == "Featured" || name == "Recently updated"),
        "the first frame shows real apps: {shown:?}"
    );
}

#[test]
fn a_new_window_paints_a_kept_flathub_sized_catalogue_in_its_first_frame() {
    let mut shown = Vec::new();
    first_frame(&mut shown);
    assert_shows_real_apps(&shown);
}

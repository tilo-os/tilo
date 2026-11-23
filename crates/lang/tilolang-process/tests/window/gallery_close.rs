//! Closing the gallery (`apps/tl/gallery`) as `tilolang dev` starts it, its instance in the window's
//! own process: the window lets go of its patch connection, the instance finds it closed on its
//! next turn and ends at once, without a wait for it, on the window's thread, which never waits
//! on storage. A window closing is no failure, so nothing warns of one.

use std::sync::{Arc, Mutex};

use tilolang_process::window::app::TlApp;
use ui::framework::testing::HeadlessApp;
use ui::tilo_resource::profiling::observe;

/// What the process logged while the window closed.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn closing_the_gallery_ends_its_instance_at_once_and_quietly() {
    crate::support::installed("gallery");
    // On the disk, as a person's profile is: a wait for it to reach the disk is a real one.
    let profile = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("gallery-close-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    let mut app = HeadlessApp::<TlApp>::open_data_dir(&profile).expect("the gallery opens");
    app.settle();
    let log = Log::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
        .with_writer(move || writer.clone())
        .finish();
    // The window closes on its frame loop, which never waits on storage.
    let ((), scopes) = tracing::subscriber::with_default(subscriber, || {
        observe(|| {
            let _frame_loop = durable_store::waiting::forbid_on_this_thread();
            drop(app);
        })
    });
    let calls = |name: &str| {
        scopes
            .iter()
            .find(|sample| sample.phase == name)
            .map_or(0, |sample| sample.calls)
    };
    assert_eq!(calls("tl.instance.close"), 1, "the instance closed");
    assert_eq!(
        calls("instance.close_wait"),
        0,
        "the instance ends on the first turn after the window let go, without a wait"
    );
    let logged = String::from_utf8_lossy(
        &log.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
    .into_owned();
    assert!(
        logged.is_empty(),
        "a window closing warns of nothing: {logged}"
    );
    let _ = std::fs::remove_dir_all(&profile);
}

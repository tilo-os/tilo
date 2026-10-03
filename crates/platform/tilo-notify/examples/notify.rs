//! Show, then withdraw, one notification through this platform's backend.
//!
//! `cargo run -p tilo-notify --example notify -- [APP_ID]`
//!
//! On macOS notifications need an app bundle: run the binary from inside a
//! `.app` whose `Info.plist` names a bundle identifier, or it reports
//! `Unavailable`.
fn main() {
    let app_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "so.tilo.notify-example".into());
    let notifier = tilo_notify::system(Some(&app_id));
    println!(
        "availability: {:?}",
        tilo_notify::availability(tilo_notify::Feature::Show)
    );
    let shown = notifier.show(&tilo_notify::Notification::new(
        "example",
        "Tilo",
        "A notification from the tilo-notify example",
    ));
    println!("show: {shown:?}");
    let at = tilo_clock::wall()
        .checked_add(tilo_clock::Duration::from_secs(60))
        .unwrap_or_else(tilo_clock::wall);
    let scheduled = notifier.schedule(
        &tilo_notify::Notification::new("later", "Tilo", "One minute later"),
        at,
    );
    println!("schedule: {scheduled:?}");
    // Asynchronous backends (macOS asks for permission first) answer on
    // their own queues; give them a moment before listing and exiting.
    std::thread::sleep(std::time::Duration::from_secs(3));
    println!("scheduled: {:?}", notifier.scheduled());
    println!("cancel: {:?}", notifier.cancel_scheduled("later"));
    println!("withdraw: {:?}", notifier.withdraw("example"));
}

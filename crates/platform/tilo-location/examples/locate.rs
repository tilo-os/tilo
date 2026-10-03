//! Ask for one location fix through this platform's backend.
//!
//! `cargo run -p tilo-location --example locate`
//!
//! On macOS the binary must run inside a `.app` whose `Info.plist` has a
//! bundle identifier and `NSLocationWhenInUseUsageDescription`, or it
//! reports `Unavailable` without asking.

fn main() {
    println!(
        "availability: {:?}",
        tilo_location::availability(tilo_location::Feature::Locate)
    );
    let answer = tilo_location::system().locate().recv();
    println!("outcome: {answer:?}");
}

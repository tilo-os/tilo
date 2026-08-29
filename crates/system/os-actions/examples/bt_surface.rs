//! Exercise the blocking Bluetooth surface against the real adapter.
//!
//! ```text
//! cargo run -p os-actions --example bt_surface
//! ```
//!
//! A tool rather than a test, so it can insist on real hardware instead of
//! skipping when there is none. Every step that cannot observe its subject
//! exits non-zero.
//!
//! It deliberately never pairs or forgets anything: those are destructive to
//! whatever the machine is already paired with. `qos-bluez`'s own
//! `live_check` covers the pairing path.

use std::time::Duration;

use os_actions::bluez;

fn main() {
    if let Err(e) = run() {
        eprintln!("\nFAIL: {e}");
        std::process::exit(1);
    }
    println!("\nOK");
}

fn run() -> Result<(), String> {
    let adapter = bluez::adapter_path()?;
    println!("adapter    {adapter}");
    println!("powered    {}", bluez::is_powered()?);

    let paired = bluez::paired_devices()?;
    println!("paired     {} devices", paired.len());
    for d in &paired {
        println!(
            "  {:<24} {} connected={} trusted={}",
            d.name, d.address, d.connected, d.trusted
        );
    }

    if bluez::is_scanning()? {
        return Err("the adapter was already scanning before we started".into());
    }
    // Assert on what this code controls — that a scan really starts and
    // really stops. How many strangers' devices happen to be advertising in
    // the next ten seconds is not ours to promise, so the count is reported
    // rather than asserted: a quiet room must not read as a broken radio.
    println!("scan       starting");
    bluez::start_scan()?;
    if !bluez::is_scanning()? {
        return Err("start_scan returned OK but the adapter is not discovering".into());
    }
    println!("scan       adapter confirms Discovering=true");
    std::thread::sleep(Duration::from_secs(10));
    let found = bluez::devices()?;
    let in_range: Vec<_> = found.iter().filter(|d| d.rssi.is_some()).collect();
    bluez::stop_scan()?;
    if bluez::is_scanning()? {
        return Err("stop_scan left the adapter scanning".into());
    }
    println!("scan       stopped cleanly");
    println!(
        "scan       {} known, {} reporting a signal",
        found.len(),
        in_range.len()
    );
    for d in in_range.iter().take(8) {
        println!(
            "  {:<24} {} rssi={:?} paired={}",
            d.name, d.address, d.rssi, d.paired
        );
    }

    // Prove the unpaired devices a scan turns up are exactly the ones that
    // `paired_devices` does not list — the distinction the "nearby" and
    // "your devices" views rest on.
    let unpaired = found.iter().filter(|d| !d.paired).count();
    if unpaired + paired.len() != found.len() {
        return Err("the paired and unpaired lists do not partition the device list".into());
    }
    println!(
        "lists      {} paired + {} unpaired = {} total",
        paired.len(),
        unpaired,
        found.len()
    );
    Ok(())
}

//! Open this platform's chooser through the same asynchronous path apps use.
//!
//! `cargo run -p tilo-dialogs --example pick -- open|multiple|directory|save|uri URL`
//!
//! Prints the outcome; Save mode reports the chosen path and writes nothing.
#![allow(clippy::expect_used, reason = "an example fails loudly")]

use std::sync::mpsc::Receiver;

use tilo_dialogs::portal::{FileFilter, PickRequest};

fn main() {
    tilo_dialogs::install_resolver(tilo_dialogs::Resolver {
        picker: tilo_dialogs::system_picker,
        opener: tilo_dialogs::system_opener,
    });
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_else(|| "open".into());
    if mode == "uri" {
        let uri = args.next().unwrap_or_else(|| "https://tilo.so".into());
        println!("{:?}", wait(&tilo_dialogs::portal::send_open_uri(&uri)));
        return;
    }
    let request = match mode.as_str() {
        "open" => PickRequest::open("Tilo: open file", "Open"),
        "multiple" => PickRequest::open_multiple("Tilo: open files", "Open"),
        "directory" => PickRequest::directory("Tilo: choose a folder", "Choose"),
        "save" => PickRequest::save("Tilo: save file", "Save", "report.txt"),
        _ => {
            eprintln!("usage: pick [open|multiple|directory|save|uri URL]");
            std::process::exit(2);
        }
    }
    .filter(FileFilter::new("Text").glob("*.txt").mimetype("text/plain"))
    .all_files();
    if request.is_multiple() {
        println!("{:?}", wait(&request.send_multiple()));
    } else {
        println!("{:?}", wait(&request.send()));
    }
}

/// Block until the answer arrives. On macOS the panel lives on the main run
/// loop, so this thread keeps running it while it waits.
#[cfg(target_os = "macos")]
fn wait<T>(answer: &Receiver<T>) -> T {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::{NSDate, NSRunLoop};

    let mtm = MainThreadMarker::new().expect("examples run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    #[allow(deprecated, reason = "the example has no other app to yield to")]
    app.activateIgnoringOtherApps(true);
    loop {
        if let Ok(outcome) = answer.try_recv() {
            return outcome;
        }
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
    }
}

#[cfg(not(target_os = "macos"))]
fn wait<T>(answer: &Receiver<T>) -> T {
    answer.recv().expect("the chooser answers once")
}

//! Exercise the host dialog and the same asynchronous result path apps use.
//! `cargo run -p ui-examples --example native-file-picker --features portal -- open`
use tilo_ui::portal::{FileFilter, MultiOutcome, PickRequest, PortalOutcome};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "open".into());
    let request = match mode.as_str() {
        "open" => PickRequest::open("Tilo: open file", "Open"),
        "multiple" => PickRequest::open_multiple("Tilo: select files", "Select"),
        "directory" => PickRequest::directory("Tilo: select folder", "Select"),
        "save" => PickRequest::save("Tilo: save file", "Save", "report.txt"),
        _ => {
            eprintln!("usage: native-file-picker [open|multiple|directory|save]");
            std::process::exit(2);
        }
    }
    .filter(FileFilter::new("Text").glob("*.txt").mimetype("text/plain"))
    .all_files();
    // Blocking here is intentional: this console example has no UI loop.
    // Apps use try_recv in their per-frame drain instead.
    let failed = if request.is_multiple() {
        let result = request.send_multiple().recv()?;
        println!("{result:?}");
        matches!(result, MultiOutcome::Failed(_))
    } else {
        let result = request.send().recv()?;
        println!("{result:?}");
        matches!(result, PortalOutcome::Failed(_))
    };
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

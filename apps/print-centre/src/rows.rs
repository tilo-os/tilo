//! The devices Print Centre publishes: the printers this machine prints to,
//! the documents waiting on them, the scanners it can reach and the printers
//! a search found. Each is a row type of its own, so search names it by what
//! it is, and a press takes one of [`PrintCentreEffect`]'s effects, bound to
//! the device.
//!
//! A row's id is the device's, encoded: the queue name, the job id, the
//! scanner's device name or the printer's address, each under its kind.

use crate::resource::PrintCentreEffect;

use ui::tilo_resource;

/// A printer this machine prints to (its CUPS queue).
#[derive(tilo_resource::Row)]
#[row(
    kind = "print-centre.printer",
    label = "Printer",
    effects = PrintCentreEffect
)]
pub struct PrinterRow;

/// One document in a printer's queue.
#[derive(tilo_resource::Row)]
#[row(
    kind = "print-centre.job",
    label = "Print job",
    effects = PrintCentreEffect
)]
pub struct JobRow;

/// A scanner this machine can reach.
#[derive(tilo_resource::Row)]
#[row(
    kind = "print-centre.scanner",
    label = "Scanner",
    effects = PrintCentreEffect
)]
pub struct ScannerRow;

/// A printer a search found nearby, which no queue prints to yet.
#[derive(tilo_resource::Row)]
#[row(
    kind = "print-centre.found-printer",
    label = "Found printer",
    effects = PrintCentreEffect
)]
pub struct FoundPrinterRow;

/// The id a printer row is published under.
pub(crate) fn printer_row_id(name: &str) -> String {
    format!("print-centre.printer.{}", hex::encode(name))
}

/// The id a print job's row is published under.
pub(crate) fn job_row_id(id: &str) -> String {
    format!("print-centre.job.{}", hex::encode(id))
}

/// The id a scanner's row is published under.
pub(crate) fn scanner_row_id(device: &str) -> String {
    format!("print-centre.scanner.{}", hex::encode(device))
}

/// The id a found printer's row is published under.
pub(crate) fn found_printer_row_id(uri: &str) -> String {
    format!("print-centre.found.{}", hex::encode(uri))
}

/// The device `id` names: a row's id, or the device's own name (a queue
/// name, a job id, a scanner's device or a printer's address) as the app's
/// own controls and an agent that knows it hand it over.
pub(crate) fn row_entity(kind: &str, id: String) -> String {
    let prefix = format!("{kind}.");
    id.strip_prefix(&prefix)
        .and_then(|key| hex::decode(key).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or(id)
}

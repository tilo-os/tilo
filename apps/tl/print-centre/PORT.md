# Print Centre, ported from the Rust app

| Feature | Where it lives now | Test |
| --- | --- | --- |
| The printers, their jobs (waiting, printing, held, stopped, cancelled, failed, printed), supplies, paper sizes and nearby printers | the `printers` reading, as the system service already publishes it | `the_overview_shows_the_printers_their_supplies_and_what_prints` |
| The overview: printers, what prints now, what needs attention | `dashboard`, `printer_card` | `the_overview_shows_the_printers_their_supplies_and_what_prints` |
| The queue, with progress and cancel/retry | `queue_pane`, `job_row` → `device.cancel_job`/`device.retry_job` | `the_queue_shows_the_active_jobs_and_the_history_the_finished_ones` |
| The history of what printed, failed or was cancelled | `history_pane` | `the_queue_shows_the_active_jobs_and_the_history_the_finished_ones` |
| Diagnostics: state, accepting, reachability, reason, paper, supplies, the scanners | `diagnostics_pane` | `diagnostics_names_how_each_printer_stands_and_its_scanners` |
| Finding printers and scanners, and adding one | `discover_pane` → `device.find`/`device.add`; `device.find_scanners` | `found_printers_are_offered_to_add` |
| Pausing, resuming, defaulting and removing a queue | `printer_card` → `device.set_paused`/`set_default`/`remove` | `the_overview_shows_the_printers_their_supplies_and_what_prints` |
| The print form: document, preview, printer, copies, page range, both sides, the printer's own paper/colour/duplex/orientation | `print_pane`, `print_form`, `choose_printer`, `ask_options`; the engine's `options` call | `the_print_form_offers_the_printer_and_what_it_reports`, `choosing_a_document_on_a_printer_asks_the_engine_for_its_options` |
| Printing the document | `send_to_printer` → the engine's `print` job; `crates/lang/tilolang-process/src/print_centre_engine.rs` over CUPS | `printing_asks_the_engine_with_the_form` |
| Scanning a page, another page, and the scan defaults (resolution, colour, source) | `scan_page`, `scan_pane`; the engine's `scan` job over SANE | `a_scan_keeps_the_page_the_scanner_answered` |
| Turning and removing a page, and putting pages in order | `turn_page`, `remove_page`, `move_page` | `a_page_is_turned_read_copied_and_removed` |
| Reading a page's text, copying it, and the language it is read in | `recognize` → the engine's `ocr` call; `copy_text` | `a_page_is_turned_read_copied_and_removed` |
| Saving a page, and the pages as one searchable PDF | `page_to_save`, `document_to_save` → the engine's `pages_pdf` call | `the_scanned_document_is_written_as_one_pdf` |
| The defaults a workspace keeps: both sides, scan resolution, colour, source, text language | `Defaults` on the app's root, set by the form's chips | `the_print_form_offers_the_printer_and_what_it_reports` |

## Known missing rows

- **Cropping a scanned page** is not ported: the Rust app drew a crop rectangle on the page, and
  this port turns, orders and removes pages but has no crop control yet.
- A print or scan job cannot be stopped once the engine started it: the engine's `cancel` marks
  the job cancelled, while `lp` and `scanimage` keep their process.

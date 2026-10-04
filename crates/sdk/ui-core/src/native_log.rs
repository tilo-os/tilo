//! Complete synchronous native diagnostic rows; callers retain their error policy.
use std::{
    fmt::Display,
    io::{self, Write},
};

pub(crate) fn write_json(prefix: &str, row: serde_json::Value) -> io::Result<()> {
    write_json_with(prefix, row, || io::stderr().lock())
}

pub(crate) fn emit_json(prefix: &str, row: serde_json::Value) {
    panic_on_error(write_json(prefix, row));
}

#[allow(
    clippy::panic,
    reason = "preserve synchronous stderr failure policy; a failed benchmark trace must not silently qualify"
)]
fn panic_on_error(result: io::Result<()>) {
    if let Err(error) = result {
        panic!("failed printing to stderr: {error}");
    }
}

fn write_json_with<W: Write>(
    prefix: &str,
    row: impl Display,
    writer: impl FnOnce() -> W,
) -> io::Result<()> {
    let line = format!("{prefix} {row}\n");
    writer().write_all(line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    const CAPTURED: &str = r###"tilo-native-frame {"cpu_frame_id":4,"frame_commit_us":8415,"input_ids":[0],"state":{"app":"dev.tilo.file-explorer","content_rect":[212.0,56.0,788.0,40892.0],"directory":"/tmp/file-explorer-native-t8p2snbi/fixture","entry_count":1200,"has_error":false,"hash_algorithm":"sha256","is_loading":false,"last_cursor":[350.0,200.0],"offset_units":"pixels","offset_x":0.0,"offset_y":0.0,"order_hash":"b8b390ea86922b9297a211bf10ab47e3a9cff0c70b81a976448714323f35f863","primary_index":null,"ready":true,"root_rect":[0.0,0.0,800.0,600.0],"selected_indices":[],"selected_names":[],"sort_by":"Name","sort_order":"Ascending","thumbnail_pending":0,"view_id":"file_view","view_mode":"List","viewport_rect":[212.0,56.0,788.0,596.0]}}
tilo-native-main-stages {"after_accessibility_host_hi_s":67936.09460075002,"after_accessibility_host_lo_s":67936.09460075002,"after_damage_mark_drawn_host_hi_s":67936.09460033335,"after_damage_mark_drawn_host_lo_s":67936.09460029168,"after_debug_outlines_host_hi_s":67936.09459866668,"after_debug_outlines_host_lo_s":67936.09459862502,"after_hud_request_host_hi_s":67936.09460087502,"after_hud_request_host_lo_s":67936.09460087502,"after_native_frame_log_host_hi_s":67936.09459862502,"after_native_frame_log_host_lo_s":67936.09459858335,"after_native_redraw_log_host_hi_s":67936.09463987501,"after_native_redraw_log_host_lo_s":67936.09463983335,"after_profiler_commit_host_hi_s":67936.09407504168,"after_profiler_commit_host_lo_s":67936.09407495835,"before_profiler_commit_host_hi_s":67936.09405400002,"before_profiler_commit_host_lo_s":67936.09405379168,"cpu_frame_id":4,"host_clock":"mach_absolute_seconds"}
tilo-display-frame {"backend":"metal","cpu_frame_id":1,"drawable_id":0,"presented_time_s":0.0,"worker_frame_ix":1,"worker_id":1}
"###;
    #[derive(Default)]
    struct Counter {
        bytes: Vec<u8>,
        calls: usize,
        chunk: Option<usize>,
        interrupt_first: bool,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.interrupt_first && self.calls == 1 {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let count = bytes.len().min(self.chunk.unwrap_or(usize::MAX));
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn native_log_rows_keep_their_bytes_in_one_complete_write() {
        // Case `captured_rows_preserve_bytes_and_use_one_accepting_write`.
        {
            for line in CAPTURED.lines() {
                let (prefix, body) = line.split_once(' ').unwrap();
                let row: serde_json::Value = serde_json::from_str(body).unwrap();
                let mut writer = Counter::default();
                write_json_with(prefix, row, || &mut writer).unwrap();
                assert_eq!(writer.bytes, format!("{line}\n").as_bytes());
                assert_eq!(writer.calls, 1, "{prefix}");
            }
        }

        // Case `escaped_unicode_and_null_keep_existing_row_content`.
        {
            let row = serde_json::json!({"quote":"\"slash\\tab\tnewline\n", "unicode":"é🦀", "absent":null});
            let expected = format!("tilo-test {row}\n");
            let mut writer = Counter::default();
            write_json_with("tilo-test", row, || &mut writer).unwrap();
            assert_eq!(writer.bytes, expected.as_bytes());
        }

        // Case `serialization_finishes_before_acquiring_writer`.
        {
            struct Observed<'a>(&'a RefCell<Vec<&'static str>>);
            impl Display for Observed<'_> {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    self.0.borrow_mut().push("serialize");
                    f.write_str("{}")
                }
            }
            let events = RefCell::new(Vec::new());
            let mut writer = Counter::default();
            write_json_with("tilo-test", Observed(&events), || {
                events.borrow_mut().push("lock");
                &mut writer
            })
            .unwrap();
            assert_eq!(*events.borrow(), ["serialize", "lock"]);
        }
    }
    #[test]
    fn native_log_writes_retry_partial_progress_and_report_failures() {
        // Case `short_writes_and_interrupted_are_retried_without_data_loss`.
        {
            let row = serde_json::json!({"value":"a longer payload"});
            let expected = format!("tilo-test {row}\n");
            let mut writer = Counter {
                chunk: Some(3),
                interrupt_first: true,
                ..Default::default()
            };
            write_json_with("tilo-test", row, || &mut writer).unwrap();
            assert_eq!(writer.bytes, expected.as_bytes());
            assert!(writer.calls > 2);
        }

        // Case `write_error_propagates_and_panic_policy_remains_explicit`.
        {
            let result = write_json_with("tilo-test", serde_json::Value::Null, || Failing);
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
            // Ignoring this Result remains available to the existing feedback logger.
            let _ = write_json_with("tilo-test", serde_json::Value::Null, || Failing);
            assert!(
                std::panic::catch_unwind(|| panic_on_error(Err(io::ErrorKind::BrokenPipe.into())))
                    .is_err()
            );
        }

        // Case `zero_progress_is_reported_as_write_zero`.
        {
            let mut writer = Counter {
                chunk: Some(0),
                ..Default::default()
            };
            let error =
                write_json_with("tilo-test", serde_json::Value::Null, || &mut writer).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::WriteZero);
        }
    }
    struct Failing;
    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}

use super::{
    AppManifest, AppSpec, Frame, MAX_REQUEST_BYTES, parse_args, read_frame, validate_manifest,
    write_reply,
};
use serde_json::json;
use std::{ffi::OsString, io::BufReader, path::PathBuf};

#[test]
#[expect(
    clippy::naive_bytecount,
    reason = "counts the newlines of one short test reply"
)]
fn stdio_framing_is_bounded_line_based_and_flushed() {
    // Phase `bounded_framing_recovers_after_an_oversized_line`.
    {
        let mut bytes = vec![b'x'; MAX_REQUEST_BYTES + 1];
        bytes.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
        let mut input = BufReader::new(bytes.as_slice());

        assert_eq!(read_frame(&mut input).unwrap(), Frame::TooLong);
        let Frame::Line(next) = read_frame(&mut input).unwrap() else {
            panic!("expected the complete request after the oversized line");
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&next).unwrap()["method"],
            "ping"
        );
        assert_eq!(read_frame(&mut input).unwrap(), Frame::Eof);
    }
    // Phase `line_at_the_one_mib_limit_is_accepted`.
    {
        let mut bytes = vec![b' '; MAX_REQUEST_BYTES];
        bytes.push(b'\n');
        let mut input = BufReader::new(bytes.as_slice());
        let Frame::Line(line) = read_frame(&mut input).unwrap() else {
            panic!("line at limit must be accepted");
        };
        assert_eq!(line.len(), MAX_REQUEST_BYTES);
    }
    // Phase `framing_joins_lines_split_across_buffer_refills`.
    {
        // A three-byte buffer forces every line to span several `fill_buf` calls,
        // including a refill that starts exactly at a newline.
        let bytes = b"abcdef\r\n\nxy\nlast";
        let mut input = BufReader::with_capacity(3, bytes.as_slice());
        assert_eq!(
            read_frame(&mut input).unwrap(),
            Frame::Line(b"abcdef".to_vec())
        );
        assert_eq!(read_frame(&mut input).unwrap(), Frame::Line(Vec::new()));
        assert_eq!(read_frame(&mut input).unwrap(), Frame::Line(b"xy".to_vec()));
        assert_eq!(
            read_frame(&mut input).unwrap(),
            Frame::Line(b"last".to_vec())
        );
        assert_eq!(read_frame(&mut input).unwrap(), Frame::Eof);
    }
    // Phase `protocol_writer_emits_one_json_line_and_flushes`.
    {
        #[derive(Default)]
        struct Output {
            bytes: Vec<u8>,
            flushes: usize,
        }
        impl std::io::Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes += 1;
                Ok(())
            }
        }
        let mut output = Output::default();
        write_reply(&mut output, &json!({"jsonrpc":"2.0","id":1,"result":{}})).unwrap();
        assert_eq!(output.flushes, 1);
        assert_eq!(output.bytes.last(), Some(&b'\n'));
        assert_eq!(
            output.bytes.iter().filter(|byte| **byte == b'\n').count(),
            1
        );
        serde_json::from_slice::<serde_json::Value>(&output.bytes).unwrap();
    }
}

/// The stdio host answers one session's requests as they finish, not in
/// the order asked: a `tilo_status` and a `tilo_cancel` answer while two
/// invokes wait, and every reply is one whole line.
#[test]
fn parallel_requests_complete_out_of_order() {
    use crate::mcp_http::tests::settle_tests::{GatedHost, overlapping_lines};
    let host = GatedHost::default();
    let server = std::sync::Arc::new(crate::mcp::McpServer::new(
        host.clone(),
        "room".into(),
        "owner".into(),
    ));
    let (client, end) = std::os::unix::net::UnixStream::pair().unwrap();
    let input = super::spawn_reader(end.try_clone().unwrap()).unwrap();
    let serving = std::thread::spawn(move || {
        let mut writer = end;
        super::serve_lines(
            tilo_rt::try_app().unwrap(),
            input,
            &mut writer,
            |request| super::mcp_answer(&server, request),
            Some(&mut || Ok(())),
        )
    });
    overlapping_lines(&host, &client);
    serving.join().unwrap().unwrap();
}

#[test]
fn cli_and_manifest_are_validated_before_launch() {
    // Phase `manifest_validation_precedes_launch_and_accepts_duplicate_ids`.
    {
        let executable = std::env::current_exe().unwrap();
        let duplicates = AppManifest {
            apps: vec![
                AppSpec {
                    id: "dev.tilo.same".into(),
                    executable: executable.clone(),
                    args: vec![],
                },
                AppSpec {
                    id: "dev.tilo.same".into(),
                    executable,
                    args: vec!["--help".into()],
                },
            ],
        };
        assert_eq!(validate_manifest(duplicates).unwrap().apps.len(), 2);

        let invalid = AppManifest {
            apps: vec![
                AppSpec {
                    id: "dev.tilo.valid".into(),
                    executable: std::env::current_exe().unwrap(),
                    args: vec![],
                },
                AppSpec {
                    id: "dev.tilo.invalid".into(),
                    executable: PathBuf::from("relative"),
                    args: vec![],
                },
            ],
        };
        let error = validate_manifest(invalid).unwrap_err();
        assert!(error.to_string().contains("absolute"));
    }
    // Phase `cli_requires_profile_and_manifest_paths`.
    {
        let parsed = parse_args([
            OsString::from("--profile"),
            OsString::from("/tmp/profile"),
            OsString::from("--apps"),
            OsString::from("/tmp/apps.json"),
        ])
        .unwrap();
        assert_eq!(parsed.profile, PathBuf::from("/tmp/profile"));
        assert_eq!(parsed.apps, PathBuf::from("/tmp/apps.json"));
        assert!(parse_args([OsString::from("--profile"), OsString::from("/tmp/profile")]).is_err());
        assert!(parse_args([OsString::from("--unknown")]).is_err());
    }
}

use super::{
    AppManifest, AppSpec, Frame, MAX_REQUEST_BYTES, parse_args, read_frame, validate_manifest,
    write_reply,
};
use serde_json::json;
use std::{ffi::OsString, io::BufReader, path::PathBuf};

#[cfg(target_os = "linux")]
#[test]
fn admission_profile_uses_readonly_existing_directories_without_repair_or_provisioning() {
    use std::{
        os::unix::fs::{PermissionsExt as _, symlink},
        path::Path,
        process::Command,
    };
    const CHILD_PROFILE: &str = "TILO_ADMISSION_PROFILE_TEST_CHILD";
    if let Some(profile) = std::env::var_os(CHILD_PROFILE) {
        let profile = PathBuf::from(profile);
        let environment = super::prepare_admission_profile_environment(&profile).unwrap();
        assert_eq!(environment.len(), 5);
        assert_eq!(environment[4].1, profile.join("identity"));
        assert!(
            std::fs::write(profile.join("identity/probe"), b"write").is_err(),
            "the test must actually observe a readonly mount"
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile");
    super::prepare_profile_environment(&profile).unwrap();
    let output = Command::new("bwrap")
        .args(["--die-with-parent", "--unshare-user", "--unshare-pid", "--unshare-net",
            "--ro-bind", "/", "/", "--proc", "/proc", "--dev", "/dev", "--setenv", CHILD_PROFILE])
        .arg(&profile)
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "mcp_process::tests::admission_profile_uses_readonly_existing_directories_without_repair_or_provisioning", "--nocapture"])
        .output().expect("the native admission integration test requires working bubblewrap");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!profile.join("identity/probe").exists());
    let absent = root.path().join("absent");
    assert!(super::prepare_admission_profile_environment(&absent).is_err());
    assert!(
        !absent.exists(),
        "admission never creates a missing profile"
    );
    std::fs::set_permissions(
        profile.join("identity"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(super::prepare_admission_profile_environment(&profile).is_err());
    assert_eq!(
        std::fs::metadata(profile.join("identity"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "admission must refuse, not repair exposed identity directories"
    );
    std::fs::remove_dir(profile.join("identity")).unwrap();
    symlink(profile.join("data"), profile.join("identity")).unwrap();
    assert!(super::prepare_admission_profile_environment(&profile).is_err());
    assert!(super::prepare_admission_profile_environment(Path::new("relative")).is_err());
}

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

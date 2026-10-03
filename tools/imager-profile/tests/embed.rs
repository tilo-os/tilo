use std::fs;
use std::process::Command;

fn command(target: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tilo-imager-profile"));
    command.arg("--target").arg(target).args([
        "--filesystem",
        "btrfs",
        "--hostname",
        "studio",
        "--full-name",
        "Test User",
        "--username",
        "testuser",
        "--locale",
        "en_US.UTF-8",
        "--keyboard",
        "de",
        "--timezone",
        "Europe/Berlin",
    ]);
    command
}

#[test]
fn embedding_changes_only_the_reserved_tail_of_regular_files() {
    // Phase `only_reserved_tail_changes_and_live_extractor_accepts_it`.
    {
        let temporary = tempfile::tempdir().unwrap();
        let media = temporary.path().join("media.img");
        fs::write(&media, vec![0xa5; 131072]).unwrap();
        assert!(command(&media).status().unwrap().success());
        let bytes = fs::read(&media).unwrap();
        assert_eq!(bytes.len(), 131072);
        assert!(bytes[..65536].iter().all(|value| *value == 0xa5));
        assert!(bytes[65536..].starts_with(b"TILO_INSTALL_PROFILE_V1\n"));
        #[cfg(target_os = "linux")]
        {
            let extractor = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../os/usr/libexec/tilo-installer-profile-extract");
            let output = temporary.path().join("profile");
            assert!(
                Command::new("bash")
                    .arg(extractor)
                    .env("TILO_INSTALLER_PROFILE_DEVICE", &media)
                    .env("TILO_INSTALLER_PROFILE_ALLOW_FILE", "1")
                    .env("TILO_INSTALLER_PROFILE_OUT_DIR", &output)
                    .status()
                    .unwrap()
                    .success()
            );
            let profile: serde_json::Value =
                serde_json::from_slice(&fs::read(output.join("profile.json")).unwrap()).unwrap();
            assert_eq!(profile["hostname"], "studio");
            assert_eq!(profile["username"], "testuser");
            assert_eq!(profile["filesystem"], "btrfs");
            assert!(profile.get("password").is_none());
        }
    }
    // Phase `undersized_regular_file_is_unchanged`.
    {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), b"too small").unwrap();
        assert!(!command(file.path()).status().unwrap().success());
        assert_eq!(fs::read(file.path()).unwrap(), b"too small");
    }
}

#[cfg(unix)]
#[test]
fn symlink_and_fifo_are_rejected_without_writing_or_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("real");
    fs::write(&file, vec![0xa5; 131072]).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(!command(&link).status().unwrap().success());
    assert!(fs::read(&file).unwrap().iter().all(|value| *value == 0xa5));
    let fifo = dir.path().join("fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    assert!(!command(&fifo).status().unwrap().success());
}

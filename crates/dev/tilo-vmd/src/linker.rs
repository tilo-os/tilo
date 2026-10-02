//! Native development links use the LLVM linker shipped with the Rust compiler.
//! Keep the C compiler frontend and Cargo's target flags, including hardening.

use std::fs;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::error::DaemonError;

const DRIVER: &str = "#!/bin/sh\nset -eu\nsysroot=$(rustc --print sysroot)\ntarget=$(rustc -vV | sed -n 's/^host: //p')\nexec cc -fuse-ld=lld \"-B$sysroot/lib/rustlib/$target/bin/gcc-ld\" \"$@\"\n";

/// Select bundled LLVM for native GNU development builds. An explicit caller
/// linker wins; release builds never call this function.
pub fn configure(command: &mut Command, root: &Path, target: &str) -> Result<(), DaemonError> {
    if !target.ends_with("-unknown-linux-gnu") {
        return Ok(());
    }
    let key = format!(
        "CARGO_TARGET_{}_LINKER",
        target.replace('-', "_").to_uppercase()
    );
    if std::env::var_os(&key).is_some() {
        return Ok(());
    }
    let output = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .map_err(DaemonError::io("locate Rust's native linker"))?;
    let sysroot = String::from_utf8_lossy(&output.stdout);
    let bundled = Path::new(sysroot.trim())
        .join("lib/rustlib")
        .join(target)
        .join("bin/gcc-ld/ld.lld");
    if !output.status.success() || !bundled.is_file() {
        return Err(DaemonError::ToolMissing {
            tool: "Rust's bundled ld.lld".into(),
            purpose: "link native development components without GNU ld memory pressure".into(),
        });
    }
    let directory = root.join("linkers");
    fs::create_dir_all(&directory).map_err(DaemonError::io("create linker driver directory"))?;
    let name = format!("cc-lld-{}", hex::encode(Sha256::digest(DRIVER.as_bytes())));
    let driver = directory.join(name);
    if !driver.exists() {
        crate::fsutil::write_atomic(&driver, DRIVER.as_bytes())?;
    }
    if fs::read(&driver).map_err(DaemonError::io("read native linker driver"))? != DRIVER.as_bytes()
    {
        return Err(DaemonError::InvalidParams(
            "native linker driver changed".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&driver, fs::Permissions::from_mode(0o755))
            .map_err(DaemonError::io("make native linker driver executable"))?;
    }
    if Path::new("/sys/fs/selinux/enforce").exists() {
        let status = Command::new("chcon")
            .arg("--reference=/usr/bin/cc")
            .arg(&driver)
            .status()
            .map_err(DaemonError::io("label native linker driver"))?;
        if !status.success() {
            return Err(DaemonError::Job {
                step: "label native linker driver".into(),
                detail: status.to_string(),
            });
        }
    }
    command.env(key, &driver);
    eprintln!("tilo-vmd: native development linker: {}", bundled.display());
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn driver_preserves_arguments_and_a_sysroot_with_spaces() {
        let directory = tempfile::tempdir().unwrap();
        let driver = directory.path().join("driver");
        fs::write(&driver, DRIVER).unwrap();
        let rustc = directory.path().join("rustc");
        fs::write(&rustc, "#!/bin/sh\ncase $1 in --print) echo '/compiler path';; -vV) echo 'host: aarch64-unknown-linux-gnu';; esac\n").unwrap();
        let cc = directory.path().join("cc");
        fs::write(&cc, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        for path in [&driver, &rustc, &cc] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = Command::new(&driver)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", directory.path().display()),
            )
            .args([
                "-Wl,-z,relro,-z,now,-z,noexecstack",
                "object with spaces.o",
                "'; touch /unexpected; #",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "-fuse-ld=lld\n-B/compiler path/lib/rustlib/aarch64-unknown-linux-gnu/bin/gcc-ld\n-Wl,-z,relro,-z,now,-z,noexecstack\nobject with spaces.o\n'; touch /unexpected; #\n"
        );
    }
}

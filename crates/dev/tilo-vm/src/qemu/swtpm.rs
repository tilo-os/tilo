//! A software TPM 2.0 (swtpm) for a QEMU guest, so encrypted and TPM-bound
//! installs can be unlock-tested and a measured boot can be observed at all.
//! Its state persists in its directory, so an installer boot (enrol) and the
//! installed system's boot (unlock) share one emulated TPM across a reboot.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{Arch, QemuError};

pub struct Swtpm {
    pub dir: PathBuf,
}

impl Swtpm {
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.dir.join("swtpm-ctrl.sock")
    }

    /// The QEMU arguments attaching it to the guest: a TIS interface on the
    /// PC's LPC bus, or on the ARM virt machine its sysbus device.
    #[must_use]
    pub fn qemu_args(&self, arch: Arch) -> Vec<String> {
        vec![
            "-chardev".into(),
            format!(
                "socket,id=chrtpm,path={}",
                super::escape(&self.socket().display().to_string())
            ),
            "-tpmdev".into(),
            "emulator,id=tpm0,chardev=chrtpm".into(),
            "-device".into(),
            match arch {
                Arch::X86_64 => "tpm-tis,tpmdev=tpm0".into(),
                Arch::Aarch64 => "tpm-tis-device,tpmdev=tpm0".into(),
            },
        ]
    }

    /// Start a started TPM on the socket, or keep the one already serving
    /// it, so the boots of one install cycle share its state.
    ///
    /// # Errors
    /// swtpm is missing, its state cannot persist, or it does not answer.
    pub fn start(&self) -> Result<(), QemuError> {
        if !super::on_path("swtpm") {
            return Err(QemuError::new(
                "swtpm is not installed",
                "install swtpm and swtpm-tools (Fedora: sudo dnf install swtpm swtpm-tools) where QEMU runs",
            ));
        }
        std::fs::create_dir_all(&self.dir).map_err(QemuError::io(&self.dir))?;
        // A listening emulator is not enough: swtpm enters permanent failure
        // mode when its NVRAM backend cannot persist state. Catch full or
        // quota-limited storage before the firmware boots into a misleading
        // PCR-service reboot loop.
        let probe = self.dir.join(".write-test");
        std::fs::write(&probe, b"Tilo TPM state preflight\n")
            .and_then(|()| std::fs::File::open(&probe)?.sync_all())
            .and_then(|()| std::fs::remove_file(&probe))
            .map_err(|error| {
                QemuError::new(
                    format!(
                        "cannot persist software-TPM state in {}: {error}",
                        self.dir.display()
                    ),
                    "free space on that filesystem (or its quota), or point --tpm-dir elsewhere",
                )
            })?;
        let socket = self.socket();
        // A leftover swtpm without startup-clear would be reused silently and
        // put the unstarted TPM back: check what is listening rather than
        // trusting the socket's existence.
        if socket.exists() {
            let serving = processes(&socket);
            if serving.is_empty()
                || serving
                    .iter()
                    .any(|(_, line)| !line.contains("startup-clear"))
            {
                eprintln!(
                    "[tilo-vm] a swtpm without --flags startup-clear (or none) holds {}; replacing it",
                    socket.display()
                );
                self.stop();
                std::thread::sleep(Duration::from_secs(1));
                let _ = std::fs::remove_file(&socket);
            }
        }
        if !socket.exists() {
            // --flags not-need-init,startup-clear is what makes this a
            // *started* TPM. Without it swtpm answers every command with
            // TPM_RC_INITIALIZE (256): the firmware measures nothing, every
            // PCR reads zero, and Linux issues TPM2_Startup itself, far too
            // late for the boot to have been measured.
            let log = std::fs::File::create(self.dir.join("swtpm.log"))
                .map_err(QemuError::io(self.dir.join("swtpm.log")))?;
            let mut command = Command::new("swtpm");
            command
                .arg("socket")
                .arg("--tpmstate")
                .arg(format!("dir={}", self.dir.display()))
                .arg("--ctrl")
                .arg(format!("type=unixio,path={}", socket.display()))
                .args(["--tpm2", "--flags", "not-need-init,startup-clear"])
                .stdin(Stdio::null())
                .stdout(log.try_clone().map_err(QemuError::io(&self.dir))?)
                .stderr(log);
            super::detach(&mut command);
            command.spawn().map_err(QemuError::io("swtpm"))?;
            let deadline = Instant::now()
                .checked_add(Duration::from_secs(5))
                .unwrap_or_else(Instant::now);
            while !socket.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if !socket.exists() {
            return Err(QemuError::new(
                format!(
                    "swtpm failed to start (see {})",
                    self.dir.join("swtpm.log").display()
                ),
                "if it rejected '--flags not-need-init,startup-clear', this swtpm is too old to start the TPM itself and the firmware cannot measure; update swtpm and swtpm-tools",
            ));
        }
        // Prove the emulator answers, rather than reading a socket as a TPM.
        if super::on_path("swtpm_ioctl") {
            let answered = Command::new("swtpm_ioctl")
                .args(["-c", "--unix"])
                .arg(&socket)
                .stdout(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if !answered {
                return Err(QemuError::new(
                    format!(
                        "swtpm is listening on {} but not answering control commands",
                        socket.display()
                    ),
                    format!(
                        "read {}; a guest booted against it would measure nothing and report every PCR as zero",
                        self.dir.join("swtpm.log").display()
                    ),
                ));
            }
        }
        eprintln!(
            "[tilo-vm] swtpm up (TPM 2.0, started, state={})",
            self.dir.display()
        );
        Ok(())
    }

    /// Stop every swtpm serving this socket.
    pub fn stop(&self) {
        for (pid, _) in processes(&self.socket()) {
            super::terminate(pid);
        }
    }
}

/// The swtpm processes serving `socket`: their pids and command lines.
/// Matched on the command line's first word, so a shell whose own command
/// line mentions swtpm and the socket is never taken for one.
fn processes(socket: &Path) -> Vec<(u32, String)> {
    let socket = socket.display().to_string();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            let raw = std::fs::read(entry.path().join("cmdline")).ok()?;
            let words: Vec<String> = raw
                .split(|byte| *byte == 0)
                .filter(|word| !word.is_empty())
                .map(|word| String::from_utf8_lossy(word).into_owned())
                .collect();
            let program = Path::new(words.first()?).file_name()?.to_str()?.to_owned();
            (program == "swtpm" && words.get(1).is_some_and(|w| w == "socket"))
                .then(|| words.join(" "))
                .filter(|line| line.contains(&socket))
                .map(|line| (pid, line))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tpm_sits_on_each_machines_own_interface() {
        let tpm = Swtpm {
            dir: "/run/t/tpm".into(),
        };
        assert_eq!(
            tpm.qemu_args(Arch::X86_64).last().unwrap(),
            "tpm-tis,tpmdev=tpm0"
        );
        assert_eq!(
            tpm.qemu_args(Arch::Aarch64).last().unwrap(),
            "tpm-tis-device,tpmdev=tpm0"
        );
    }
}

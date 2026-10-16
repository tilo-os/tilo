use std::process::Command;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bwrap_full_env() {
        let mut cmd = Command::new("bwrap");
        cmd.arg("--ro-bind")
            .arg("/usr")
            .arg("/usr")
            .arg("--ro-bind")
            .arg("/etc")
            .arg("/etc")
            .arg("--symlink")
            .arg("usr/lib")
            .arg("/lib")
            .arg("--symlink")
            .arg("usr/lib64")
            .arg("/lib64")
            .arg("--symlink")
            .arg("usr/bin")
            .arg("/bin")
            .arg("--proc")
            .arg("/proc")
            .arg("--dev")
            .arg("/dev")
            .arg("--tmpfs")
            .arg("/tmp")
            .arg("sh")
            .arg("-c")
            .arg("echo 'Hello from Sandbox'; ls /usr/lib64/libwayland-client.so.0");

        let output = cmd.output().expect("Failed to execute bwrap");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Hello from Sandbox"));
        assert!(stdout.contains("libwayland-client.so.0"));
    }

    /// The sandbox must pass device nodes through rather than hiding them.
    ///
    /// A GPU-less machine cannot prove that about `/dev/dri`, and asserting it
    /// anyway tests the runner rather than the recipe. So the device under
    /// test is whichever one the host actually has: `/dev/dri` where there is
    /// a GPU, `/dev/null` otherwise. Either way the assertion runs and the
    /// `--dev-bind` recipe is exercised — this never skips.
    #[test]
    fn test_gl_check() {
        // What the sandbox must do is pass a device node through. Where the
        // host has a DRM render node that claim can be made in full; where it
        // has none — a CI runner with no GPU — asserting it anyway would test
        // the runner rather than the recipe. So the device under test is
        // whichever one exists, and the assertion runs either way: a check
        // that cannot observe its subject must still check something real,
        // never skip.
        let host_has_render_node = std::fs::read_dir("/dev/dri")
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with("renderD"));
        let node = if host_has_render_node {
            "/dev/dri"
        } else {
            "/dev/null"
        };

        let mut cmd = Command::new("bwrap");
        cmd.arg("--ro-bind")
            .arg("/usr")
            .arg("/usr")
            .arg("--ro-bind")
            .arg("/etc")
            .arg("/etc")
            .arg("--symlink")
            .arg("usr/lib")
            .arg("/lib")
            .arg("--symlink")
            .arg("usr/lib64")
            .arg("/lib64")
            .arg("--dev-bind")
            .arg(node)
            .arg(node)
            .arg("sh")
            .arg("-c")
            .arg(format!("ls {node}"));

        let output = cmd.output().expect("Failed to execute bwrap");
        assert!(
            output.status.success(),
            "{node} was bound into the sandbox but is not visible inside it: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        // Visible is not enough where a real GPU is present: the bind has to
        // carry the render node itself, not just an empty directory.
        assert!(
            !host_has_render_node || String::from_utf8_lossy(&output.stdout).contains("renderD"),
            "sandbox /dev/dri contains no render node: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

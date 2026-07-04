use std::process::Command;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bwrap_full_env() {
        // Create symlinks and binds exactly like our production Container
        let mut cmd = Command::new("bwrap");
        cmd.arg("--ro-bind").arg("/usr").arg("/usr")
           .arg("--ro-bind").arg("/etc").arg("/etc")
           .arg("--symlink").arg("usr/lib").arg("/lib")
           .arg("--symlink").arg("usr/lib64").arg("/lib64")
           .arg("--symlink").arg("usr/bin").arg("/bin")
           .arg("--proc").arg("/proc")
           .arg("--dev").arg("/dev")
           .arg("--tmpfs").arg("/tmp")
           .arg("sh").arg("-c").arg("echo 'Hello from Sandbox'; ls /usr/lib64/libwayland-client.so.0");
        
        let output = cmd.output().expect("Failed to execute bwrap");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Hello from Sandbox"));
        assert!(stdout.contains("libwayland-client.so.0"));
    }

    #[test]
    fn test_gl_check() {
        // Try to see if glxinfo or similar is available and runs
        let mut cmd = Command::new("bwrap");
        cmd.arg("--ro-bind").arg("/usr").arg("/usr")
           .arg("--ro-bind").arg("/etc").arg("/etc")
           .arg("--symlink").arg("usr/lib").arg("/lib")
           .arg("--symlink").arg("usr/lib64").arg("/lib64")
           .arg("--dev-bind").arg("/dev/dri").arg("/dev/dri")
           .arg("sh").arg("-c").arg("ls /dev/dri");
        
        let output = cmd.output().expect("Failed to execute bwrap");
        assert!(output.status.success(), "GPU nodes not visible in sandbox");
    }
}

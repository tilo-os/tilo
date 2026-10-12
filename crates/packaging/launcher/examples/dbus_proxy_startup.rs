//! Measure the production sandbox proxy against a private, real D-Bus daemon.
use std::error::Error;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

struct PrivateBus(Child);
impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = tempfile::tempdir()?;
    let binary = launcher::dbus_proxy::find_proxy_binary().ok_or("xdg-dbus-proxy unavailable")?;
    let mut bus = PrivateBus(
        Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()?,
    );
    let stdout = bus
        .0
        .stdout
        .take()
        .ok_or("missing private bus address stream")?;
    let mut address = String::new();
    BufReader::new(stdout).read_line(&mut address)?;
    if address.is_empty() {
        return Err("private bus did not publish an address".into());
    }
    for sample in 0..40 {
        let socket = root.path().join(format!("proxy-{sample}.sock"));
        let started = Instant::now();
        let guard = launcher::dbus_proxy::spawn_dbus_proxy(&binary, address.trim(), &socket)?;
        let ready_us = started.elapsed().as_micros();
        // Observe the real listener rather than accepting the helper's existence.
        let connection = UnixStream::connect(guard.socket_path())?;
        println!("{{\"sample\":{sample},\"ready_us\":{ready_us}}}");
        drop(connection);
        drop(guard);
    }
    Ok(())
}

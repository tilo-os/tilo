//! Measure the production sandbox proxy against a private, real D-Bus daemon.
use std::error::Error;
use std::os::unix::net::UnixStream;
use std::time::Instant;

use tilo_test_env::PrivateBus;

fn main() -> Result<(), Box<dyn Error>> {
    let root = tempfile::tempdir()?;
    let binary = launcher::dbus_proxy::find_proxy_binary().ok_or("xdg-dbus-proxy unavailable")?;
    let bus = PrivateBus::start();
    for sample in 0..40 {
        let socket = root.path().join(format!("proxy-{sample}.sock"));
        let started = Instant::now();
        let guard = launcher::dbus_proxy::spawn_dbus_proxy(&binary, bus.address(), &socket)?;
        let ready_us = started.elapsed().as_micros();
        // Observe the real listener rather than accepting the helper's existence.
        let connection = UnixStream::connect(guard.socket_path())?;
        println!("{{\"sample\":{sample},\"ready_us\":{ready_us}}}");
        drop(connection);
        drop(guard);
    }
    Ok(())
}

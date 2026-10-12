//! Explicit backend audit: no capture fixtures and no network/image pulls.
//! Run on a host with a real rootless Podman engine. Failure is a failed audit.
use containers::podman;

struct OwnedVolume {
    name: String,
    active: bool,
}

impl Drop for OwnedVolume {
    fn drop(&mut self) {
        if self.active
            && let Err(error) = podman::remove_volume(&self.name, false)
        {
            eprintln!("could not remove audit volume {}: {error}", self.name);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let initial = podman::probe().engine;
    if !initial.live {
        return Err(format!(
            "live Podman engine is unavailable; backend audit did not run: {}",
            initial.connection
        )
        .into());
    }
    let name = format!(
        "tilo-live-audit-{}-{}",
        std::process::id(),
        ui::time::try_unix_now_ns().ok_or("system clock is before the Unix epoch")?
    );
    podman::create_volume(&name)?;
    let mut volume = OwnedVolume { name, active: true };
    let created = podman::probe().engine;
    if !created.live || !created.volumes.iter().any(|item| item.name == volume.name) {
        return Err("created volume was not returned by the actual live projection".into());
    }
    podman::remove_volume(&volume.name, false)?;
    let removed = podman::probe().engine;
    if !removed.live || removed.volumes.iter().any(|item| item.name == volume.name) {
        return Err("removed volume still appears in the actual live projection".into());
    }
    // The successful removal is proved above; do not ask the cleanup guard
    // to remove it a second time.
    volume.active = false;
    println!("live Podman list/create-volume/remove-volume projection: passed");
    Ok(())
}

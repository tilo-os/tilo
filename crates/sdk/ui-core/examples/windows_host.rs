//! Installer and device-verification entry point for the Windows host APIs.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use tilo_ui::host_notifications::{HostNotifications, register_shortcut};
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [command, id, title, exe, link] if command == "register" => {
            register_shortcut(id, title, exe.as_ref(), link.as_ref())?;
        }
        [command, id, tag, title, body] if command == "show" => {
            let host = HostNotifications::new(id)?;
            host.show(tag, title, body)?;
            println!("enabled={:?}", host.enabled());
        }
        [command, id, tag, uri, title, body] if command == "show-protocol" => {
            HostNotifications::new(id)?.with_protocol_activation(uri)?.show(tag, title, body)?;
        }
        [command, id] if command == "history" => {
            println!("{}", serde_json::to_string(&HostNotifications::new(id)?.history()?)?);
        }
        [command, id, tag] if command == "remove" => HostNotifications::new(id)?.remove(tag)?,
        [command, id, tag, seconds, title, body] if command == "schedule" => {
            let time = tilo_ui::time::wall() + std::time::Duration::from_secs(seconds.parse()?);
            HostNotifications::new(id)?.schedule(tag, title, body, time)?;
        }
        [command, id] if command == "scheduled" => {
            let pending: Vec<_> = HostNotifications::new(id)?.scheduled()?.into_iter()
                .map(|notice| (notice.id, notice.delivery_time.duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_millis()).unwrap_or_default()))
                .collect();
            println!("{}", serde_json::to_string(&pending)?);
        }
        [command, id, tag] if command == "cancel" => HostNotifications::new(id)?.cancel_schedule(tag)?,
        _ => return Err("expected register ID TITLE EXE LINK | show ID TAG TITLE BODY | show-protocol ID TAG URI TITLE BODY | history ID | remove ID TAG | schedule ID TAG SECONDS TITLE BODY | scheduled ID | cancel ID TAG".into()),
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The Windows host installer requires Windows.");
    std::process::exit(1);
}

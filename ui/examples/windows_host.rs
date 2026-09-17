//! Installer and device-verification entry point for the Windows host APIs.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use ui::host_notifications::{HostNotifications, register_shortcut};
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
        [command, id] if command == "history" => {
            println!("{}", serde_json::to_string(&HostNotifications::new(id)?.history()?)?);
        }
        [command, id, tag] if command == "remove" => HostNotifications::new(id)?.remove(tag)?,
        _ => return Err("expected register ID TITLE EXE LINK | show ID TAG TITLE BODY | history ID | remove ID TAG".into()),
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("The Windows host installer requires Windows.");
    std::process::exit(1);
}

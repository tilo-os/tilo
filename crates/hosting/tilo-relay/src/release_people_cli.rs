//! `tilo-relay release-people`: the people a released relay kept, read out of
//! its state directory, and taken into this relay's own workspace.
//!
//! `export` runs beside the earlier release's files, on a copy of its volume
//! and read-only, and writes what it finds as JSON. `import` hands that JSON to
//! the running relay over its operator socket, which keeps the addresses on its
//! wait list as they stood and declares or invites each person.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Subcommand;
use tilo_relay_protocol::release_people::{Imported, People};

#[derive(Subcommand)]
pub enum Cmd {
    /// Read the earlier release's people out of its state directory, read-only.
    Export {
        /// The released relay's workspace state directory, e.g. the volume's
        /// `lounge/`: its `relay/rooms/` holds the workspace it served.
        #[arg(long)]
        state: PathBuf,
        /// Where to write the JSON `import` takes; `-` writes it to standard
        /// output.
        #[arg(long)]
        out: PathBuf,
    },
    /// Take the exported people into the running relay, over its operator socket.
    Import {
        /// The relay's operator socket (`--operator-socket`).
        #[arg(long)]
        socket: PathBuf,
        /// The JSON `export` wrote; `-` reads it from standard input.
        #[arg(long)]
        file: PathBuf,
    },
}

pub fn run(cmd: Cmd) -> anyhow::Result<()> {
    match cmd {
        Cmd::Export { state, out } => export(&state, &out),
        Cmd::Import { socket, file } => import(&socket, &file),
    }
}

/// The file `path` names, or the standard stream `-` does.
fn export(state: &std::path::Path, out: &std::path::Path) -> anyhow::Result<()> {
    let people =
        tilo_relay::release_people::export(state).map_err(|reason| anyhow::anyhow!("{reason}"))?;
    let json = serde_json::to_vec_pretty(&people)?;
    // The JSON is what `import` takes, so the summary never mixes into it:
    // it goes to standard error, and an `--out -` is the JSON alone.
    eprintln!(
        "{} kept {} addresses at its door and {} people",
        people.release,
        people.wait_list.len(),
        people.members.len()
    );
    match out.to_str() {
        Some("-") => {
            use std::io::Write as _;
            std::io::stdout().write_all(&json)?;
            std::io::stdout().write_all(b"\n")?;
        }
        _ => std::fs::write(out, &json).with_context(|| format!("write {}", out.display()))?,
    }
    Ok(())
}

fn import(socket: &std::path::Path, file: &std::path::Path) -> anyhow::Result<()> {
    let bytes = match file.to_str() {
        Some("-") => {
            use std::io::Read as _;
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes)?;
            bytes
        }
        _ => std::fs::read(file).with_context(|| format!("read {}", file.display()))?,
    };
    let people: People = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not what export wrote", file.display()))?;
    let command = tilo_relay_protocol::operator::OperatorCommand::ImportPeople { people };
    let answer = ask(socket, &serde_json::to_string(&command)?)?;
    let event: tilo_relay_protocol::operator::OperatorEvent =
        serde_json::from_str(&answer).with_context(|| format!("the relay answered {answer:?}"))?;
    let tilo_relay_protocol::operator::OperatorEvent::PeopleImported { imported } = event else {
        anyhow::bail!("the relay answered {event:?}");
    };
    say(&imported);
    if imported.refused.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{} people were not taken in", imported.refused.len())
    }
}

fn say(imported: &Imported) {
    println!("{} addresses on the wait list", imported.waiting.len());
    for owner in &imported.owners {
        println!("{} holds the workspace as its owner", owner.name);
    }
    for invitation in &imported.invitations {
        println!(
            "{} is invited as {}{}: {}",
            invitation.name,
            invitation.role,
            if invitation.mailed { ", mailed" } else { "" },
            invitation.link
        );
    }
    for refused in &imported.refused {
        eprintln!("{refused}");
    }
}

/// One operator command over the workspace's socket, and its one answer line.
fn ask(socket: &std::path::Path, command: &str) -> anyhow::Result<String> {
    use std::io::{Read as _, Write as _};
    let mut connection = std::os::unix::net::UnixStream::connect(socket)
        .with_context(|| format!("the workspace is not listening on {}", socket.display()))?;
    connection.write_all(command.as_bytes())?;
    connection.write_all(b"\n")?;
    connection.flush()?;
    let mut answer = String::new();
    connection.read_to_string(&mut answer)?;
    let line = answer.trim();
    let event = line
        .strip_prefix("tilo-workspace-operator ")
        .ok_or_else(|| anyhow::anyhow!("the workspace answered nothing: {line:?}"))?;
    Ok(event.to_owned())
}

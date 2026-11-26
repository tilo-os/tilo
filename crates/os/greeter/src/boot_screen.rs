//! `tilo-greeter --boot`: the greeter as the sign-in screen of the
//! initramfs (docs/security/sign-in.md §3.4).
//!
//! `tilo-boot-signin` starts it with no capabilities and hands it three
//! kinds of descriptor: its end of the socket (`--link-fd`), the display's
//! DRM card as its master (`--card-fd`) and each input device
//! (`--input FD:PATH`). It draws on the CPU through the toolkit's KMS host
//! ([`ui_kms`]); nothing here opens a device, talks to the TPM or reads the
//! volume. The device's sign-in comforts come first over the socket and are
//! clamped as they are applied: the volume header they come from is not
//! authenticated.

use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};
use std::path::PathBuf;

use settings_store::sign_in_comforts::SignInComforts;

use crate::GreeterApp;
use crate::auth::boot::{BootLink, Link, ToGreeter};

/// What the supervisor handed over, by descriptor number.
#[derive(Debug, Default, PartialEq, Eq)]
struct Handed {
    link: Option<RawFd>,
    card: Option<RawFd>,
    inputs: Vec<(RawFd, PathBuf)>,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Handed, String> {
    let mut handed = Handed::default();
    let mut args = args;
    let number = |value: Option<String>, flag: &str| {
        value
            .as_deref()
            .and_then(|value| value.parse::<RawFd>().ok())
            .filter(|fd| *fd > 2)
            .ok_or_else(|| format!("{flag} takes a descriptor number"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--boot" => {}
            "--link-fd" => handed.link = Some(number(args.next(), "--link-fd")?),
            "--card-fd" => handed.card = Some(number(args.next(), "--card-fd")?),
            "--input" => {
                let value = args.next().unwrap_or_default();
                let (fd, path) = value.split_once(':').ok_or("--input takes FD:PATH")?;
                handed
                    .inputs
                    .push((number(Some(fd.to_owned()), "--input")?, PathBuf::from(path)));
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(handed)
}

/// The espeak-ng voice for a language tag, among the voices the initramfs
/// carries (the dictionaries the `94tilo-greeter` dracut module installs).
fn language_voice(tag: &str) -> Option<&'static str> {
    let primary = tag.split('-').next()?.to_ascii_lowercase();
    ["en", "de", "fr", "es"]
        .into_iter()
        .find(|voice| *voice == primary)
}

/// Whether the command line asks for the boot sign-in.
pub fn requested(mut args: impl Iterator<Item = String>) -> bool {
    args.any(|arg| arg == "--boot")
}

/// Run the boot sign-in until the supervisor ends it.
///
/// # Errors
///
/// When the command line is not what the supervisor passes, the supervisor
/// went away before saying who signs in, or there is no display to draw on.
/// The supervisor then asks another way.
pub fn run(args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let handed = parse(args)?;
    let (Some(link), Some(card)) = (handed.link, handed.card) else {
        return Err("--link-fd and --card-fd are required".into());
    };
    // SAFETY: the supervisor opened these descriptors for this process alone
    // and passed their numbers; nothing else here owns them.
    let (link, card, inputs) = unsafe {
        (
            OwnedFd::from_raw_fd(link),
            OwnedFd::from_raw_fd(card),
            handed
                .inputs
                .into_iter()
                .map(|(fd, path)| (path, OwnedFd::from_raw_fd(fd)))
                .collect::<Vec<_>>(),
        )
    };
    let link = BootLink::new(Link::from_fd(link));
    let comforts = match link.recv()? {
        ToGreeter::Hello { comforts } => SignInComforts::from_value(&comforts),
        _ => return Err("the supervisor did not start with its hello".into()),
    };
    if let Some(language) = &comforts.language {
        crate::i18n::CATALOG.set_locale(language);
    }
    let layout = ui_kms::Layout {
        layout: comforts.xkb_layout.clone().unwrap_or_default(),
        variant: comforts.xkb_variant.clone().unwrap_or_default(),
    };
    let host = ui_kms::Host::new(ui_kms::Devices { card, inputs }, &layout)?;

    let mut widget = GreeterApp::for_boot(link.clone(), comforts.screen_reader);
    // The first frame shows who signs in, so it waits for the list, which
    // the supervisor sends as soon as it has read it.
    let mut counted_from = Ran::default();
    loop {
        let waits = !link.has_message();
        let message = link.recv()?;
        let people = matches!(message, ToGreeter::People { .. });
        widget.boot_message(message);
        if people {
            // Ready before the list came: the first frame's work starts now.
            // The list came first: everything since the start was on its way.
            if waits {
                counted_from = Ran::so_far();
            }
            break;
        }
    }

    let mut wake_scope = ui::wake::WakeScope::new();
    let waker = host.proxy();
    wake_scope.bind(move |_| waker.wake())?;
    let mut app = ui::App::from_scoped_parts(wake_scope, ui::State::new(), Box::new(widget))
        .without_native_document_chrome();
    app.autofocus_first = true;
    app.autofocus_on_structure_change = true;
    app.set_text_scale(comforts.text_scale);
    app.set_high_contrast(comforts.high_contrast);
    if comforts.screen_reader {
        let mut reader = tilo_ui_base::screen_reader::ScreenReader::new();
        let settings = comforts.screen_reader_settings();
        reader.configure(&settings);
        // The system voice is the screen's language, where espeak-ng has it.
        if settings.voice.engine_voice().is_none()
            && let Some(voice) = comforts.language.as_deref().and_then(language_voice)
        {
            reader.set_voice(voice);
        }
        app.set_screen_reader(Some(reader));
    }
    // The chord turns the reader over here as everywhere, and the card's
    // toggle follows it. Nothing in the initramfs keeps the change.
    let chord = host.proxy();
    app.on_screen_reader_chord(move |on| {
        chord.send_external(crate::comforts::chord_payload(on));
    });

    let messages = host.proxy();
    let ended = host.proxy();
    link.listen(
        move |payload| messages.send_external(payload),
        move || ended.quit(),
    );
    host.run(&mut app, || {
        // The boot performance check reads this line's time, and holds the
        // work it names to the first frame's budget.
        let ran = Ran::so_far().since(counted_from);
        eprintln!(
            "tilo-greeter: first frame of the boot sign-in ran_ms={} waited_ms={}",
            ran.ran_ns / 1_000_000,
            ran.waited_ns / 1_000_000
        );
    })?;
    Ok(())
}

/// What this process's threads have run on a core and waited for one, from
/// the kernel's scheduler statistics for each (`/proc/self/task/*/schedstat`):
/// the work of the first frame, apart from the time a busy machine kept it
/// waiting. A thread that has ended counts no more.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ran {
    ran_ns: u64,
    waited_ns: u64,
}

impl Ran {
    fn so_far() -> Self {
        let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
            return Self::default();
        };
        tasks
            .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("schedstat")).ok())
            .map(|stat| Self::parse(&stat))
            .fold(Self::default(), |total, thread| Self {
                ran_ns: total.ran_ns.saturating_add(thread.ran_ns),
                waited_ns: total.waited_ns.saturating_add(thread.waited_ns),
            })
    }

    /// One thread's `schedstat`: the nanoseconds it ran, then the nanoseconds
    /// it waited to run, then how many times it ran.
    fn parse(stat: &str) -> Self {
        let mut fields = stat
            .split_whitespace()
            .map(|field| field.parse::<u64>().unwrap_or(0));
        Self {
            ran_ns: fields.next().unwrap_or(0),
            waited_ns: fields.next().unwrap_or(0),
        }
    }

    fn since(self, earlier: Self) -> Self {
        Self {
            ran_ns: self.ran_ns.saturating_sub(earlier.ran_ns),
            waited_ns: self.waited_ns.saturating_sub(earlier.waited_ns),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> impl Iterator<Item = String> {
        line.split(' ')
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn the_supervisor_command_line_names_each_descriptor() {
        assert_eq!(
            parse(args(
                "--boot --link-fd 3 --card-fd 4 --input 5:/dev/input/event0 --input 6:/dev/input/event3"
            )),
            Ok(Handed {
                link: Some(3),
                card: Some(4),
                inputs: vec![
                    (5, PathBuf::from("/dev/input/event0")),
                    (6, PathBuf::from("/dev/input/event3")),
                ],
            })
        );
        assert!(requested(args("--boot --link-fd 3")));
        assert!(!requested(args("--shots out.png")));
        assert!(parse(args("--boot --link-fd 1")).is_err());
        assert!(parse(args("--boot --input 5")).is_err());
        assert!(parse(args("--boot --session x")).is_err());
    }

    #[test]
    fn the_system_voice_follows_a_language_the_initramfs_can_speak() {
        assert_eq!(language_voice("de-DE"), Some("de"));
        assert_eq!(language_voice("FR"), Some("fr"));
        assert_eq!(language_voice("ja-JP"), None);
    }
}

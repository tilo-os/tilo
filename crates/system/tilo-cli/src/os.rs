//! `tilo os upgrade`: move this machine onto the newest OS image made for it.
//!
//! What a person wants from it is one command that picks the right image,
//! says what it is doing while it downloads, says in a sentence what went
//! wrong when something does, and ends with a single restart. The pieces:
//!
//! * [`Facts`]: what the machine is (production or development image, which
//!   board) and what bootc follows now.
//! * [`plan`]: the image to move onto. A production image keeps its signed,
//!   measured update route (`tilo-deployment update`); a development image
//!   follows the moving tag CI forwards for every main build of its board
//!   (`tilo-vm:<board>-ci`) or, without a board, of the generic dev image
//!   (`tilo-vm:main-ci`), unless `--to` names a build.
//! * [`stage`]: bootc with `--progress-fd`, its JSON lines drawn as a
//!   [`ProgressView`], its refusals turned into [`Failure`]s.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;

/// Where development images live.
pub const DEV_REPOSITORY: &str = "registry.tilo.so/tilo/tilo-vm";

/// The bootc this runs. Overridable so the flow can be exercised against a
/// stand-in without a machine to change.
pub fn bootc_program() -> String {
    std::env::var("TILO_BOOTC").unwrap_or_else(|_| "bootc".to_string())
}

/// What the machine is, read from the booted image and bootc.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// `/usr/lib/tilo/image-flavour` says `dev`.
    pub development: bool,
    /// The board the image was built for (`hardware-profile.json`'s `id`).
    pub board: Option<String>,
    /// The image bootc follows: (transport, reference).
    pub follows: Option<(String, String)>,
    /// The image the machine runs now.
    pub booted: Option<String>,
}

impl Facts {
    /// The image's own files under `root` (`/` on a machine).
    pub fn read_image(root: &Path) -> Self {
        let development = std::fs::read_to_string(root.join("usr/lib/tilo/image-flavour"))
            .is_ok_and(|flavour| flavour.trim() == "dev");
        let board = std::fs::read(root.join("usr/lib/tilo/hardware-profile.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|profile| profile.get("id")?.as_str().map(str::to_owned))
            .filter(|id| !id.is_empty());
        Self {
            development,
            board,
            follows: None,
            booted: None,
        }
    }

    /// Add what `bootc status --json` says.
    pub fn with_bootc_status(mut self, json: &str) -> Self {
        let Ok(status) = serde_json::from_str::<Value>(json.trim()) else {
            return self;
        };
        self.follows = status.pointer("/spec/image").and_then(|image| {
            Some((
                image
                    .get("transport")
                    .and_then(Value::as_str)
                    .unwrap_or("registry")
                    .to_owned(),
                image.get("image")?.as_str()?.to_owned(),
            ))
        });
        self.booted = status
            .pointer("/status/booted/image/image/image")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self
    }
}

/// What `tilo os upgrade` will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// A production image: the signed update route, which knows its own
    /// image.
    Signed,
    /// Move onto another image: `bootc switch <image>`.
    Switch(String),
    /// Fetch the newest build of the image already followed: `bootc upgrade`.
    Upgrade(String),
}

impl Plan {
    /// bootc's words for it (after `bootc`).
    pub fn bootc_words(&self) -> Vec<String> {
        match self {
            Self::Signed => Vec::new(),
            Self::Switch(image) => vec!["switch".into(), image.clone()],
            Self::Upgrade(_) => vec!["upgrade".into()],
        }
    }

    pub fn image(&self) -> Option<&str> {
        match self {
            Self::Signed => None,
            Self::Switch(image) | Self::Upgrade(image) => Some(image),
        }
    }
}

/// The image `--to` names: a full reference as it is; a CI build's tag
/// (`main-1a2b3c4`, `pr-4433-c5d51ba`) as that build's image for this board,
/// or its generic dev image without one.
pub fn image_for(to: &str, board: Option<&str>) -> String {
    if to.contains('/') {
        return to.to_owned();
    }
    let suffix = board.map_or_else(|| "-ci".to_owned(), |board| format!("-{board}"));
    if to.ends_with(&suffix) {
        format!("{DEV_REPOSITORY}:{to}")
    } else {
        format!("{DEV_REPOSITORY}:{to}{suffix}")
    }
}

/// The newest main build for this machine: its board's moving tag, or the
/// generic dev image's.
pub fn newest_for(board: Option<&str>) -> String {
    match board {
        Some(board) => format!("{DEV_REPOSITORY}:{board}-ci"),
        None => format!("{DEV_REPOSITORY}:main-ci"),
    }
}

/// Decide what to move onto.
///
/// # Errors
/// `--to` on a production image, whose signed route picks its own image.
pub fn plan(facts: &Facts, to: Option<&str>) -> Result<Plan> {
    if !facts.development {
        if let Some(to) = to {
            anyhow::bail!(
                "this machine runs a signed Tilo OS image, which updates only to the newest signed release; \
                 it cannot be moved onto {to}. Run `tilo os upgrade` without --to"
            );
        }
        return Ok(Plan::Signed);
    }
    let target = to.map_or_else(
        || newest_for(facts.board.as_deref()),
        |to| image_for(to, facts.board.as_deref()),
    );
    let follows = facts
        .follows
        .as_ref()
        .is_some_and(|(transport, image)| transport == "registry" && *image == target);
    Ok(if follows {
        Plan::Upgrade(target)
    } else {
        Plan::Switch(target)
    })
}

/// The shell line that runs bootc with the ESP writable. bootc (1.16, on the
/// composefs backend) fails its own switch and upgrade when /boot is a vfat
/// mounted read-only, and it applies a staged deployment at shutdown, which
/// mounts the ESP again: so /boot is made writable, stays so until the next
/// boot after a successful stage, and goes back to read-only after a failed
/// one. bootc's JSON progress goes to standard output, its own messages to
/// standard error. `$0` is the bootc program, `"$@"` its words.
pub const STAGE_SCRIPT: &str = "ro=; if findmnt -n -o FSTYPE,OPTIONS /boot 2>/dev/null | grep -q '^vfat *ro,'; then ro=1; mount -o remount,rw /boot || exit 1; fi; \"$0\" \"$@\" --quiet --progress-fd 3 3>&1 1>&2; rc=$?; if [ \"$rc\" != 0 ] && [ -n \"$ro\" ]; then mount -o remount,ro /boot; fi; exit $rc";

/// `bootc status --format json` with the ESP writable for the one command
/// (see [`STAGE_SCRIPT`]), and read-only again after it.
pub const STATUS_SCRIPT: &str = "ro=; if findmnt -n -o FSTYPE,OPTIONS /boot 2>/dev/null | grep -q '^vfat *ro,'; then ro=1; mount -o remount,rw /boot || exit 1; fi; \"$0\" status --format json; rc=$?; [ -z \"$ro\" ] || mount -o remount,ro /boot; exit $rc";

/// What bootc says about the machine, through [`STATUS_SCRIPT`]; empty when
/// it says nothing usable.
pub fn bootc_status(bootc: &str) -> String {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(STATUS_SCRIPT)
        .arg(bootc)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default()
}

/// Why bootc did not stage anything, in words for a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The machine already runs (or has staged) the newest image.
    UpToDate(String),
    /// bootc's space check refused: (needed, free).
    NoSpace { needed: String, free: String },
    /// Anything else, with bootc's own last words.
    Other(String),
}

impl Failure {
    pub fn from_bootc(output: &str) -> Self {
        if let Some((free, needed)) = insufficient_space(output) {
            return Self::NoSpace { needed, free };
        }
        if output.contains("Some(Staged) deployment") || output.contains("Update already staged") {
            return Self::UpToDate(
                "the newest image is already staged; it boots at the next restart".into(),
            );
        }
        if output.contains("No changes in")
            || output.contains("Some(Booted) deployment")
            || output.contains("Image specification is unchanged")
        {
            return Self::UpToDate("already up to date: this machine runs the newest image".into());
        }
        let last = output
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("bootc stopped without saying why");
        Self::Other(last.to_owned())
    }

    /// The sentence `tilo` ends with.
    pub fn sentence(&self) -> String {
        match self {
            Self::UpToDate(text) => text.clone(),
            Self::NoSpace { needed, free } => format!(
                "not enough space: the new OS image needs {needed}, {free} is free.\n\
                 What was already downloaded is kept, so the next try fetches only the rest.\n\
                 Room can come from: developer layers no slot uses (`tilo-vm gc` on a development machine),\n\
                 app runtimes no app uses (`flatpak uninstall --unused`), and old logs (`journalctl --vacuum-size=200M`)."
            ),
            Self::Other(text) => format!("the upgrade did not stage: {text}"),
        }
    }
}

/// bootc's refusal for want of space, as (free, needed):
/// `Insufficient free space for <image> (available: 2.3 GB required: 5.2 GB)`.
pub fn insufficient_space(output: &str) -> Option<(String, String)> {
    let (_, rest) = output.split_once("Insufficient free space for ")?;
    let (_, rest) = rest.split_once("(available: ")?;
    let (free, rest) = rest.split_once(" required: ")?;
    let (needed, _) = rest.split_once(')')?;
    Some((free.trim().to_owned(), needed.trim().to_owned()))
}

/// bootc's progress, drawn as stages.
#[derive(Debug)]
pub struct ProgressView {
    started: Instant,
    terminal: bool,
    last_line: Option<Instant>,
    /// The last download state: (bytes, total, layers done, layers total, layers cached).
    download: Option<(u64, u64, u64, u64, u64)>,
    /// The stage last named.
    stage: Option<String>,
}

/// A size as people read it.
pub fn human_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        let tenths = bytes / 100_000_000;
        format!("{}.{} GB", tenths / 10, tenths % 10)
    } else if bytes >= 1_000_000 {
        format!("{} MB", bytes / 1_000_000)
    } else {
        format!("{} kB", bytes / 1000)
    }
}

impl ProgressView {
    pub fn new(terminal: bool) -> Self {
        Self::starting_at(terminal, Instant::now())
    }

    pub fn starting_at(terminal: bool, started: Instant) -> Self {
        Self {
            started,
            terminal,
            last_line: None,
            download: None,
            stage: None,
        }
    }

    /// One JSON line from bootc; what to print for it, if anything.
    pub fn line(&mut self, json: &str, now: Instant) -> Option<String> {
        let event: Value = serde_json::from_str(json.trim()).ok()?;
        let number = |key: &str| event.get(key).and_then(Value::as_u64).unwrap_or(0);
        let text = |key: &str| {
            event
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        match event.get("type").and_then(Value::as_str)? {
            "ProgressBytes" => {
                let state = (
                    number("bytes"),
                    number("bytesTotal"),
                    number("steps"),
                    number("stepsTotal"),
                    number("stepsCached"),
                );
                self.download = Some(state);
                let first = self.stage.as_deref() != Some("download");
                self.stage = Some("download".into());
                let finished = state.0 >= state.1 && state.1 > 0;
                // Off a terminal, a line every few seconds is enough to show life.
                let due = self.terminal
                    || first
                    || finished
                    || self
                        .last_line
                        .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(10));
                if !due {
                    return None;
                }
                self.last_line = Some(now);
                Some(self.download_line(now))
            }
            "ProgressSteps" => {
                let description = text("description");
                let label = if description.is_empty() {
                    text("task")
                } else {
                    description
                };
                let changed = self.stage.as_deref() != Some(label.as_str());
                self.stage = Some(label.clone());
                let (steps, total) = (number("steps"), number("stepsTotal"));
                if !changed && !self.terminal {
                    return None;
                }
                Some(if total > 1 {
                    format!(
                        "{label} (step {} of {total})",
                        steps.saturating_add(1).min(total)
                    )
                } else {
                    label
                })
            }
            _ => None,
        }
    }

    fn download_line(&self, now: Instant) -> String {
        use std::fmt::Write as _;
        let Some((bytes, total, steps, steps_total, cached)) = self.download else {
            return String::new();
        };
        let elapsed = now.duration_since(self.started).as_secs().max(1);
        let speed = bytes.checked_div(elapsed).unwrap_or(0);
        let to_go = steps_total.saturating_sub(steps);
        let mut line = format!("Downloading {} of {}", human_size(bytes), human_size(total));
        if steps_total > 0 {
            let _ = write!(line, ", {to_go} of {steps_total} layers to go");
        }
        if cached > 0 {
            let _ = write!(line, " ({cached} already here)");
        }
        if bytes > 0 {
            let _ = write!(line, ", {}/s", human_size(speed));
        }
        line
    }
}

/// Run bootc with `plan`'s words through [`STAGE_SCRIPT`], drawing its
/// progress on `out`.
///
/// # Errors
/// bootc could not be started; a refusal is `Ok(Err(Failure))`.
pub fn stage(
    bootc: &str,
    plan: &Plan,
    out: &mut impl Write,
    terminal: bool,
) -> Result<std::result::Result<(), Failure>> {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(STAGE_SCRIPT)
        .arg(bootc)
        .args(plan.bootc_words())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {bootc}"))?;
    let mut stderr = child.stderr.take().context("bootc's standard error")?;
    let messages = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let mut view = ProgressView::new(terminal);
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(text) = view.line(&line, Instant::now()) {
                if terminal {
                    write!(out, "\r\x1b[2K  {text}")?;
                } else {
                    writeln!(out, "  {text}")?;
                }
                out.flush()?;
            }
        }
    }
    let status = child.wait().context("waiting for bootc")?;
    if terminal {
        writeln!(out)?;
    }
    let messages = messages.join().unwrap_or_default();
    Ok(if status.success() {
        // bootc says "no changes" with success, too.
        match Failure::from_bootc(&messages) {
            Failure::UpToDate(text) => Err(Failure::UpToDate(text)),
            _ => Ok(()),
        }
    } else {
        Err(Failure::from_bootc(&messages))
    })
}

/// What happens at the restart, said before it.
pub fn restart_note(plan: &Plan) -> String {
    let image = plan.image().unwrap_or("the new image");
    format!(
        "Staged {image}. At the next restart:\n  \
         - the new image boots; the one running now stays as the fallback (`tilo os rollback`)\n  \
         - developer layers swapped onto the old image are dropped: they were built for it\n  \
         - an encrypted disk asks for its passphrase at the keyboard"
    )
}

/// Whether to restart after staging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    Now,
    Ask,
    Later,
}

pub fn restart_decision(reboot: bool, yes: bool, terminal: bool) -> Restart {
    if reboot {
        Restart::Now
    } else if yes || !terminal {
        Restart::Later
    } else {
        Restart::Ask
    }
}

/// The arguments `tilo` runs itself again with under sudo.
pub fn sudo_args(
    exe: &Path,
    to: Option<&str>,
    yes: bool,
    reboot: bool,
    check: bool,
) -> Vec<String> {
    let mut args = vec![
        "--".to_owned(),
        exe.display().to_string(),
        "os".to_owned(),
        "upgrade".to_owned(),
    ];
    if let Some(to) = to {
        args.extend(["--to".to_owned(), to.to_owned()]);
    }
    for (set, flag) in [(yes, "--yes"), (reboot, "--reboot"), (check, "--check")] {
        if set {
            args.push(flag.to_owned());
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(board: Option<&str>, follows: Option<&str>) -> Facts {
        Facts {
            development: true,
            board: board.map(str::to_owned),
            follows: follows.map(|image| ("registry".to_owned(), image.to_owned())),
            booted: follows.map(str::to_owned),
        }
    }

    #[test]
    fn the_image_is_the_newest_build_for_this_device() {
        // A board follows its own moving main tag.
        assert_eq!(
            plan(
                &dev(
                    Some("lenovo-83ed"),
                    Some("registry.tilo.so/tilo/tilo-vm:pr-4227-4ab5dcc-lenovo-83ed")
                ),
                None
            )
            .unwrap(),
            Plan::Switch("registry.tilo.so/tilo/tilo-vm:lenovo-83ed-ci".into())
        );
        // Already following it: fetch its newest build.
        let following = dev(
            Some("lenovo-83ed"),
            Some("registry.tilo.so/tilo/tilo-vm:lenovo-83ed-ci"),
        );
        let upgrade = plan(&following, None).unwrap();
        assert_eq!(
            upgrade,
            Plan::Upgrade("registry.tilo.so/tilo/tilo-vm:lenovo-83ed-ci".into())
        );
        assert_eq!(upgrade.bootc_words(), vec!["upgrade".to_owned()]);
        // A generic dev image follows main's dev image.
        assert_eq!(
            plan(&dev(None, None), None).unwrap(),
            Plan::Switch("registry.tilo.so/tilo/tilo-vm:main-ci".into())
        );
        // --to names a CI build, for this board or generic.
        assert_eq!(
            plan(&following, Some("pr-4433-c5d51ba"))
                .unwrap()
                .bootc_words(),
            vec![
                "switch".to_owned(),
                "registry.tilo.so/tilo/tilo-vm:pr-4433-c5d51ba-lenovo-83ed".to_owned()
            ]
        );
        assert_eq!(
            image_for("main-1a2b3c4-lenovo-83ed", Some("lenovo-83ed")),
            "registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-lenovo-83ed"
        );
        assert_eq!(
            image_for("main-1a2b3c4", None),
            "registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci"
        );
        assert_eq!(
            image_for("localhost/tilo-vm:mine", Some("lenovo-83ed")),
            "localhost/tilo-vm:mine"
        );
        // A signed image keeps its signed route and refuses another image.
        let signed = Facts::default();
        assert_eq!(plan(&signed, None).unwrap(), Plan::Signed);
        let error = plan(&signed, Some("main-1a2b3c4")).unwrap_err().to_string();
        assert!(error.contains("signed Tilo OS image"), "{error}");
    }

    #[test]
    fn facts_come_from_the_image_and_bootc() {
        let root = std::env::temp_dir().join(format!("tilo-os-facts-{}", std::process::id()));
        std::fs::create_dir_all(root.join("usr/lib/tilo")).unwrap();
        std::fs::write(root.join("usr/lib/tilo/image-flavour"), "dev\n").unwrap();
        std::fs::write(
            root.join("usr/lib/tilo/hardware-profile.json"),
            r#"{"schema_version":1,"id":"lenovo-83ed"}"#,
        )
        .unwrap();
        let facts = Facts::read_image(&root).with_bootc_status(
            r#"{"spec":{"image":{"image":"registry.tilo.so/tilo/tilo-vm:lenovo-83ed-ci","transport":"registry"}},
                "status":{"booted":{"image":{"image":{"image":"registry.tilo.so/tilo/tilo-vm:pr-1-abc-lenovo-83ed"}}}}}"#,
        );
        std::fs::remove_dir_all(&root).unwrap();
        assert!(facts.development);
        assert_eq!(facts.board.as_deref(), Some("lenovo-83ed"));
        assert_eq!(
            facts.follows,
            Some((
                "registry".to_owned(),
                "registry.tilo.so/tilo/tilo-vm:lenovo-83ed-ci".to_owned()
            ))
        );
        assert!(!Facts::read_image(Path::new("/nonexistent")).development);
    }

    #[test]
    fn bootc_refusals_read_as_sentences() {
        let space = Failure::from_bootc(
            "error: Switching: Composefs Switching: Performing Upgrade Operation: Insufficient free space for registry.tilo.so/tilo/tilo-vm:lenovo-83ed-ci (available: 2.3 GB required: 5.2 GB)",
        );
        assert_eq!(
            space,
            Failure::NoSpace {
                needed: "5.2 GB".into(),
                free: "2.3 GB".into()
            }
        );
        let sentence = space.sentence();
        assert!(
            sentence
                .starts_with("not enough space: the new OS image needs 5.2 GB, 2.3 GB is free."),
            "{sentence}"
        );
        assert!(
            sentence.contains("already downloaded is kept"),
            "{sentence}"
        );
        for up_to_date in [
            "No changes in: registry.tilo.so/tilo/tilo-vm:lenovo-83ed-ci",
            "error: Switching: Composefs Switching: Target image has the same fs-verity digest as the existing Some(Booted) deployment.",
        ] {
            assert_eq!(
                Failure::from_bootc(up_to_date).sentence(),
                "already up to date: this machine runs the newest image"
            );
        }
        assert!(matches!(
            Failure::from_bootc("Target image has the same fs-verity digest as the existing Some(Staged) deployment."),
            Failure::UpToDate(text) if text.contains("already staged")
        ));
        assert_eq!(
            Failure::from_bootc("warning: x\nerror: pulling: connection refused\n").sentence(),
            "the upgrade did not stage: error: pulling: connection refused"
        );
    }

    #[test]
    fn progress_shows_stages_bytes_layers_and_speed() {
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        let bytes = |done: u64, steps: u64| {
            format!(
                r#"{{"type":"ProgressBytes","task":"pulling","description":"Pulling image","id":"x","bytesCached":0,"bytes":{done},"bytesTotal":3400000000,"stepsCached":180,"steps":{steps},"stepsTotal":223,"subtasks":[]}}"#
            )
        };
        let mut view = ProgressView::starting_at(true, start);
        assert_eq!(
            view.line(r#"{"type":"Start","version":"0.1.0"}"#, at(0)),
            None
        );
        assert_eq!(
            view.line(&bytes(1_200_000_000, 200), at(40)).unwrap(),
            "Downloading 1.2 GB of 3.4 GB, 23 of 223 layers to go (180 already here), 30 MB/s"
        );
        assert_eq!(
            view.line(
                r#"{"type":"ProgressSteps","task":"importing","description":"Importing Image","id":"i","stepsCached":0,"steps":0,"stepsTotal":1,"subtasks":[]}"#,
                at(50)
            )
            .unwrap(),
            "Importing Image"
        );
        assert_eq!(view.line("not json", at(51)), None);

        // Off a terminal: the first line, then one every ten seconds, and the end.
        let mut plain = ProgressView::starting_at(false, start);
        assert!(plain.line(&bytes(100_000_000, 190), at(1)).is_some());
        assert!(plain.line(&bytes(200_000_000, 191), at(5)).is_none());
        assert!(plain.line(&bytes(900_000_000, 199), at(12)).is_some());
        assert!(plain.line(&bytes(3_400_000_000, 223), at(13)).is_some());
    }

    /// A stand-in bootc: what it was asked, its progress drawn, its refusal
    /// explained, and "no changes" with success read as up to date.
    #[test]
    fn staging_runs_bootc_with_progress_and_explains_refusals() {
        let dir = std::env::temp_dir().join(format!("tilo-os-stage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = |name: &str, body: &str| {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path.display().to_string()
        };
        let args = dir.join("args");
        let ok = fake(
            "ok",
            &format!(
                "echo \"$@\" > {}\necho '{{\"type\":\"ProgressBytes\",\"task\":\"pulling\",\"description\":\"Pulling\",\"id\":\"x\",\"bytesCached\":0,\"bytes\":5000000,\"bytesTotal\":5000000,\"stepsCached\":0,\"steps\":2,\"stepsTotal\":2,\"subtasks\":[]}}' >&3\necho 'Queued for next boot' >&2",
                args.display()
            ),
        );
        let plan = Plan::Switch("registry.tilo.so/tilo/tilo-vm:lenovo-83ed-ci".into());
        let mut out = Vec::new();
        assert_eq!(stage(&ok, &plan, &mut out, false).unwrap(), Ok(()));
        assert_eq!(
            std::fs::read_to_string(&args).unwrap().trim(),
            "switch registry.tilo.so/tilo/tilo-vm:lenovo-83ed-ci --quiet --progress-fd 3"
        );
        let shown = String::from_utf8(out).unwrap();
        assert!(
            shown.contains("Downloading 5 MB of 5 MB, 0 of 2 layers to go"),
            "{shown}"
        );

        let full = fake(
            "full",
            "echo 'error: Insufficient free space for x (available: 2.3 GB required: 5.2 GB)' >&2; exit 1",
        );
        assert_eq!(
            stage(&full, &plan, &mut Vec::new(), false).unwrap(),
            Err(Failure::NoSpace {
                needed: "5.2 GB".into(),
                free: "2.3 GB".into()
            })
        );
        let same = fake("same", "echo 'No changes in: x'");
        assert!(matches!(
            stage(&same, &Plan::Upgrade("x".into()), &mut Vec::new(), false).unwrap(),
            Err(Failure::UpToDate(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn restart_and_root_decisions() {
        assert_eq!(restart_decision(true, false, false), Restart::Now);
        assert_eq!(restart_decision(false, false, true), Restart::Ask);
        assert_eq!(restart_decision(false, true, true), Restart::Later);
        assert_eq!(restart_decision(false, false, false), Restart::Later);
        assert_eq!(
            sudo_args(
                Path::new("/usr/bin/tilo"),
                Some("main-1a2b3c4"),
                true,
                false,
                false
            ),
            vec![
                "--",
                "/usr/bin/tilo",
                "os",
                "upgrade",
                "--to",
                "main-1a2b3c4",
                "--yes"
            ]
        );
        let note = restart_note(&Plan::Switch("img".into()));
        assert!(
            note.contains("developer layers swapped onto the old image are dropped"),
            "{note}"
        );
        assert!(note.contains("passphrase"), "{note}");
    }
}

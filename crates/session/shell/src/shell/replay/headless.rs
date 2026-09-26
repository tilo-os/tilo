//! A replay run where nobody watches: the shell itself, headless.
//!
//! [`HeadlessReplay`] starts `shell --backend headless` with the real
//! launcher and sandbox, so the recorded apps run as the real binaries they
//! were recorded as, and drives the session's replay through the same
//! shell actions a person or agent uses (`replay.*` over the control
//! socket). Frames come from the compositor's own capture of the replay
//! room's surfaces, the path a recording captures through, which is what
//! lets the divergence check compare them.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use session_trace::Offset;
use session_trace::frames::{FrameError, FrameProvider, SurfaceFrame};

/// Where the headless shell runs and what it may use from this machine.
#[derive(Clone, Debug)]
pub struct HeadlessOptions {
    /// The `shell` binary.
    pub shell: PathBuf,
    /// A private directory for the session: its homes, socket and log.
    pub root: PathBuf,
    /// The installed-app registry the session launches from.
    pub apps_dir: Option<PathBuf>,
    /// The runtime bundle store packaged apps pin.
    pub runtime_bundle_dir: Option<PathBuf>,
    /// Output size, `(width, height)`.
    pub size: (u32, u32),
    /// How long a replayed state may take to settle after a seek.
    pub settle: Duration,
}

/// Why a headless replay failed.
#[derive(Debug, thiserror::Error)]
pub enum HeadlessError {
    #[error("the headless shell could not start: {0}")]
    Start(String),
    #[error("the headless shell refused {op}: {message}")]
    Refused { op: String, message: String },
    #[error("talking to the headless shell failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("the headless shell answered something unreadable: {0}")]
    Protocol(String),
}

/// A headless shell session driving one replay.
pub struct HeadlessReplay {
    child: Child,
    socket: PathBuf,
    root: PathBuf,
    settle: Duration,
    captures: u64,
    /// The scale frames are rendered at; `None` takes the recorded one.
    scale: Option<f64>,
}

impl std::fmt::Debug for HeadlessReplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeadlessReplay")
            .field("socket", &self.socket)
            .finish_non_exhaustive()
    }
}

impl HeadlessReplay {
    /// Start a headless shell and wait for its control socket.
    ///
    /// # Errors
    ///
    /// When the shell does not start or its socket does not appear.
    pub fn start(options: &HeadlessOptions) -> Result<Self, HeadlessError> {
        let root = &options.root;
        for dir in ["data", "cache", "state", "config", "run", "keys"] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        let socket = root.join("run").join("control.sock");
        let log = std::fs::File::create(root.join("shell.log"))?;
        let mut command = Command::new(&options.shell);
        command
            .arg("--backend")
            .arg("headless")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env("TILO_TEST_CONTROL", &socket)
            .env("TILO_STATE_PATH", root.join("config").join("state.json"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("TILO_PEER_KEY_PATH", root.join("keys").join("peer-key"))
            .env(
                "TILO_HEADLESS_SIZE",
                format!("{}x{}", options.size.0, options.size.1),
            )
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        if let Some(apps) = &options.apps_dir {
            command.env("TILO_APPS_DATA_DIR", apps);
        }
        if let Some(bundles) = &options.runtime_bundle_dir {
            command.env("TILO_RUNTIME_BUNDLE_DIR", bundles);
        }
        let child = command
            .spawn()
            .map_err(|error| HeadlessError::Start(error.to_string()))?;
        let mut replay = Self {
            child,
            socket,
            root: root.clone(),
            settle: options.settle,
            captures: 0,
            scale: None,
        };
        let deadline = later(Duration::from_secs(90));
        while Instant::now() < deadline {
            if replay.socket.exists()
                && replay
                    .call(&serde_json::json!({"op": "state_dump"}))
                    .is_ok()
            {
                return Ok(replay);
            }
            if let Ok(Some(status)) = replay.child.try_wait() {
                return Err(HeadlessError::Start(format!(
                    "the shell exited ({status}); see {}",
                    replay.root.join("shell.log").display()
                )));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(HeadlessError::Start(
            "the control socket did not appear".into(),
        ))
    }

    /// One control-socket request and its answer.
    ///
    /// # Errors
    ///
    /// When the socket fails or the shell answers with an error.
    pub fn call(&self, request: &serde_json::Value) -> Result<serde_json::Value, HeadlessError> {
        let mut stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        stream.write_all(format!("{request}\n").as_bytes())?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        let answer: serde_json::Value = serde_json::from_str(line.trim())
            .map_err(|error| HeadlessError::Protocol(error.to_string()))?;
        if let Some(error) = answer.get("err") {
            return Err(HeadlessError::Refused {
                op: request["op"].as_str().unwrap_or_default().to_string(),
                message: error.to_string(),
            });
        }
        Ok(answer)
    }

    /// Invoke one shell action, as `tilo-control actions invoke` does.
    ///
    /// # Errors
    ///
    /// When the action is refused.
    pub fn shell_action(
        &self,
        id: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, HeadlessError> {
        self.call(&serde_json::json!({
            "op": "actions_invoke",
            "provider": "shell.actions",
            "id": id,
            "args": args,
        }))
    }

    /// The shell's `chrome.replay` status: transport and step list.
    ///
    /// # Errors
    ///
    /// When the state cannot be read.
    pub fn status(&self) -> Result<serde_json::Value, HeadlessError> {
        let dump = self.call(&serde_json::json!({"op": "state_dump"}))?;
        Ok(dump
            .pointer("/state/chrome/replay")
            .cloned()
            .unwrap_or_default())
    }

    /// Open the session record `record` for replay, shown in this session.
    ///
    /// # Errors
    ///
    /// When the record cannot be replayed.
    pub fn open(&self, record: &str) -> Result<(), HeadlessError> {
        self.shell_action("replay.open", &serde_json::json!({ "record": record }))
            .map(|_| ())
    }

    /// Render frames at `scale` device pixels per surface unit, such as a
    /// recorded frame's `StoredFrame::scale`, instead of the recorded
    /// surface's.
    pub fn set_scale(&mut self, scale: Option<f64>) {
        self.scale = scale;
    }

    /// Stand at `at` in recorded time and let the apps settle there.
    ///
    /// # Errors
    ///
    /// When the replay refuses the seek.
    pub fn seek(&self, at: Offset) -> Result<(), HeadlessError> {
        self.shell_action(
            "replay.seek",
            &serde_json::json!({ "at_ms": at.as_millis() }),
        )?;
        std::thread::sleep(self.settle);
        Ok(())
    }

    /// Capture every surface of the replay room as it looks now.
    ///
    /// # Errors
    ///
    /// When the capture is refused or its output cannot be read.
    pub fn capture(&mut self, scale: Option<f64>) -> Result<Vec<SurfaceFrame>, HeadlessError> {
        self.captures = self.captures.saturating_add(1);
        let dir = self.root.join("frames").join(self.captures.to_string());
        self.call(&serde_json::json!({"op": "replay_frames", "dir": dir, "scale": scale}))?;
        let manifest = dir.join("frames.json");
        let deadline = later(Duration::from_secs(30));
        while !manifest.exists() {
            if Instant::now() > deadline {
                return Err(HeadlessError::Protocol(
                    "the replay room's frames were not written".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        read_frames(&dir, &manifest)
    }
}

fn read_frames(dir: &Path, manifest: &Path) -> Result<Vec<SurfaceFrame>, HeadlessError> {
    let protocol = |error: &dyn std::fmt::Display| HeadlessError::Protocol(error.to_string());
    let entries: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(manifest)?).map_err(|error| protocol(&error))?;
    let mut frames = Vec::with_capacity(entries.len());
    for entry in entries {
        let field = |key: &str| entry.get(key).cloned().unwrap_or_default();
        let text = |key: &str| field(key).as_str().unwrap_or_default().to_string();
        let number = |key: &str| {
            field(key)
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| HeadlessError::Protocol(format!("frame {key} is missing")))
        };
        let (width, height) = (number("width")?, number("height")?);
        let png = std::fs::read(dir.join(text("file")))?;
        let rgba = session_trace::frames::decode_png(&png, width, height)
            .map_err(|error| protocol(&error))?;
        frames.push(
            SurfaceFrame::new(
                Offset(field("at_us").as_u64().unwrap_or_default()),
                session_trace::Route::new(text("app_id"), text("room"), text("record_id")),
                width,
                height,
                field("scale").as_f64().unwrap_or(1.0),
                rgba,
            )
            .map_err(|error| protocol(&error))?,
        );
    }
    Ok(frames)
}

impl FrameProvider for HeadlessReplay {
    fn frames_at(&mut self, at: Offset) -> Result<Vec<SurfaceFrame>, FrameError> {
        self.seek(at).map_err(|_| FrameError::Unreachable(at))?;
        self.capture(self.scale)
            .map(|frames| {
                frames
                    .into_iter()
                    .map(|mut frame| {
                        frame.at = at;
                        frame
                    })
                    .collect()
            })
            .map_err(|error| FrameError::Render(error.to_string()))
    }
}

impl Drop for HeadlessReplay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn later(by: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(by).unwrap_or(now)
}

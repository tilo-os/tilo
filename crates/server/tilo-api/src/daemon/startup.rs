//! Startup progress published by the native daemon and the limits its callers use.

use std::time::Duration;

/// How long a caller waits for a daemon that is starting.
#[derive(Clone, Copy, Debug)]
pub struct StartupLimits {
    /// Time without observed progress before giving up.
    pub stall: Duration,
    /// Total time a start may take, including time spent making progress.
    pub cap: Duration,
    /// Time between observations.
    pub poll: Duration,
}

/// The native daemon's startup limits.
pub const STARTUP: StartupLimits = StartupLimits {
    stall: Duration::from_secs(15),
    cap: Duration::from_secs(120),
    poll: Duration::from_millis(25),
};

/// The profile file where a daemon records each startup step on a new line before taking it.
pub const STAGES_FILE: &str = "startup.stages";

/// One bounded snapshot of the native daemon's newline-separated startup journal.
pub struct StageJournal {
    device: u64,
    inode: u64,
    bytes: Vec<u8>,
}

impl StageJournal {
    /// Read the opened journal. The caller selects and validates its profile and file.
    #[must_use]
    pub fn read(file: std::fs::File) -> Option<Self> {
        use std::io::Read as _;
        use std::os::unix::fs::MetadataExt as _;
        const LIMIT: u64 = 8192;
        let meta = file.metadata().ok()?;
        if !meta.is_file() || meta.len() > LIMIT {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes).ok()?;
        (bytes.len() as u64 <= LIMIT).then(|| Self {
            device: meta.dev(),
            inode: meta.ino(),
            bytes,
        })
    }

    /// What the daemon wrote, including an incomplete last line while a write is in flight.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Whether the writer finished every recorded stage line.
    #[must_use]
    pub fn complete(&self) -> bool {
        std::str::from_utf8(&self.bytes).is_ok_and(|text| {
            text.ends_with('\n')
                && text.lines().all(|line| {
                    !line.is_empty()
                        && line.len() <= 200
                        && line.bytes().all(|b| b == b' ' || b.is_ascii_graphic())
                })
        })
    }

    /// Whether the same journal grew by appending steps. A replacement or truncation is
    /// a different start, whose progress the original caller cannot use.
    #[must_use]
    pub fn advanced_from(&self, previous: &Self) -> Option<bool> {
        (self.device == previous.device
            && self.inode == previous.inode
            && self.bytes.starts_with(&previous.bytes))
        .then(|| self.bytes.len() > previous.bytes.len())
    }
}

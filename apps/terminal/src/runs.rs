//! The command runs of a session, as its shell marks them (docs/app-model/
//! ush.md, "Command runs"): [`Marks`] takes the session's own OSC 133 marks
//! out of the pty stream, and [`Runs`] keeps the run that is going — its
//! command line, directory, start and raw output — until its end mark makes
//! it a [`FinishedRun`].

use base64::Engine as _;

/// The most raw output a run keeps; it is `truncated` past this.
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// The longest mark taken; anything longer is passed on as output.
const MAX_MARK_BYTES: usize = 16 * 1024;

/// The command lines `run` asked this device to run, handed from the view
/// that carries out the session's outbox to the terminal that types them,
/// with the reply each answers once its run ends. Shared, so the view's
/// handler and the terminal hold one queue.
#[derive(Clone, Default)]
pub struct RunQueue {
    inner: std::rc::Rc<std::cell::RefCell<RunQueueState>>,
}

#[derive(Default)]
struct RunQueueState {
    waiting: Vec<Asked>,
    pane: String,
}

/// One command line asked to run, and who waits for its run.
pub struct Asked {
    pub request: ui::tilo_resource::Id,
    pub command: String,
    /// Who asked: whom the run is attributed to.
    pub requested_by: String,
    /// An agent asked: a shell that renders for agents does so for this
    /// run.
    pub agent: bool,
    pub reply: ui::framework::EffectReply,
}

impl RunQueue {
    /// Ask for a command line to run, answering its reply with the run.
    pub fn ask(&self, asked: Asked) {
        self.inner.borrow_mut().waiting.push(asked);
    }

    /// What was asked since the last call.
    pub fn take(&self) -> Vec<Asked> {
        std::mem::take(&mut self.inner.borrow_mut().waiting)
    }

    /// The pane of the process this device runs the session in: whose
    /// outbox it carries out.
    pub fn set_pane(&self, pane: &str) {
        pane.clone_into(&mut self.inner.borrow_mut().pane);
    }

    /// Whether this device runs the session whose pane is `pane`.
    #[must_use]
    pub fn runs(&self, pane: &str) -> bool {
        let own = &self.inner.borrow().pane;
        !own.is_empty() && own == pane
    }
}

/// A fresh key to mark a session's runs with: 32 letters and digits.
#[must_use]
pub fn new_key() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// One mark of the session's shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    /// The prompt is shown.
    Prompt,
    /// A command line starts running in `cwd`. A shell that numbers its
    /// runs also says where this one is in its history: the number the run
    /// carries.
    Started {
        command: String,
        cwd: String,
        number: Option<u64>,
    },
    /// The command ended with `status`, the one numbered `number` when the
    /// shell numbers its runs.
    Ended { status: i32, number: Option<u64> },
    /// A background job started as run `number`: job `job` of the shell,
    /// `command` in `cwd`. It runs beside whatever the shell runs next.
    JobStarted {
        number: u64,
        job: u32,
        command: String,
        cwd: String,
    },
    /// What follows is output of job run `number` (`open`), or no longer is.
    JobOutput { number: u64, open: bool },
    /// Job run `number` ended with `status`.
    JobEnded { number: u64, status: i32 },
    /// The shell asks for the person's history: the newest `limit` command
    /// lines past the `skip` newest that contain `matching`.
    History(HistoryAsk),
}

/// A question of the shell's line editor to the person's history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryAsk {
    pub skip: usize,
    pub limit: usize,
    pub matching: String,
}

/// A piece of the pty stream: output, or one of the session's marks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Output(Vec<u8>),
    Mark(Mark),
}

/// Splits the pty stream into output and the marks that carry the session's
/// key, across reads. A mark without the key (a program printing one) stays
/// output, which the emulator ignores like any OSC it does not know.
#[derive(Debug)]
pub struct Marks {
    key: String,
    /// An `ESC ]` sequence still arriving.
    pending: Vec<u8>,
    /// The shell said at its last prompt that it reads marks on its input
    /// (`in=1`): a run asked for, an answer from the history.
    reads_input: bool,
}

const START: &[u8] = b"\x1b]133;";

impl Marks {
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            pending: Vec::new(),
            reads_input: false,
        }
    }

    /// The key the session's marks carry.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Whether the shell reads Terminal's marks on its input.
    #[must_use]
    pub const fn reads_input(&self) -> bool {
        self.reads_input
    }

    /// The pieces of `bytes`, which follow what was pushed before.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Piece> {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(bytes);
        let mut pieces = Vec::new();
        let mut output = Vec::new();
        let mut at = 0;
        while at < input.len() {
            let rest = input.get(at..).unwrap_or_default();
            let Some(offset) = rest.iter().position(|b| *b == 0x1b) else {
                output.extend_from_slice(rest);
                break;
            };
            output.extend_from_slice(rest.get(..offset).unwrap_or_default());
            let candidate = rest.get(offset..).unwrap_or_default();
            if candidate.len() < START.len() {
                if START.starts_with(candidate) {
                    self.pending = candidate.to_vec();
                    break;
                }
                output.push(0x1b);
                at += offset + 1;
                continue;
            }
            if !candidate.starts_with(START) {
                output.push(0x1b);
                at += offset + 1;
                continue;
            }
            let body = candidate.get(START.len()..).unwrap_or_default();
            let end = body
                .iter()
                .position(|b| *b == 0x07)
                .map(|end| (end, 1))
                .or_else(|| {
                    body.windows(2)
                        .position(|w| w == b"\x1b\\")
                        .map(|end| (end, 2))
                });
            let Some((end, terminator)) = end else {
                if candidate.len() > MAX_MARK_BYTES {
                    output.push(0x1b);
                    at += offset + 1;
                    continue;
                }
                self.pending = candidate.to_vec();
                break;
            };
            let consumed = START.len() + end + terminator;
            match self.mark(body.get(..end).unwrap_or_default()) {
                Some(mark) => {
                    if !output.is_empty() {
                        pieces.push(Piece::Output(std::mem::take(&mut output)));
                    }
                    pieces.push(Piece::Mark(mark));
                }
                None => output.extend_from_slice(candidate.get(..consumed).unwrap_or_default()),
            }
            at += offset + consumed;
        }
        if !output.is_empty() {
            pieces.push(Piece::Output(output));
        }
        pieces
    }

    /// The mark `body` (what follows `133;`) names, when it carries the key.
    fn mark(&mut self, body: &[u8]) -> Option<Mark> {
        let body = std::str::from_utf8(body).ok()?;
        let mut fields = body.split(';');
        let kind = fields.next()?;
        let mut key = None;
        let mut command = None;
        let mut cwd = None;
        let mut number = None;
        let mut status = None;
        let mut job = None;
        let mut reads_input = false;
        let mut skip = None;
        let mut limit = None;
        let mut matching = None;
        for field in fields {
            match field.split_once('=') {
                Some(("k", value)) => key = Some(value),
                Some(("cmd", value)) => command = Some(decode(value)?),
                Some(("cwd", value)) => cwd = Some(decode(value)?),
                Some(("t", value)) => number = value.parse().ok(),
                Some(("j", value)) => job = value.parse().ok(),
                Some(("in", value)) => reads_input = value == "1",
                Some(("s", value)) => skip = value.parse().ok(),
                Some(("n", value)) => limit = value.parse().ok(),
                Some(("q", value)) => matching = Some(decode(value)?),
                Some(_) => {}
                None => status = field.parse().ok(),
            }
        }
        if key != Some(self.key.as_str()) {
            return None;
        }
        match (kind, job) {
            ("A", _) => {
                self.reads_input = reads_input;
                Some(Mark::Prompt)
            }
            ("C", Some(job)) => Some(Mark::JobStarted {
                number: number?,
                job,
                command: command?,
                cwd: cwd.unwrap_or_default(),
            }),
            ("C", None) => Some(Mark::Started {
                command: command?,
                cwd: cwd.unwrap_or_default(),
                number,
            }),
            ("D", Some(_)) => Some(Mark::JobEnded {
                number: number?,
                status: status.unwrap_or(0),
            }),
            ("D", None) => Some(Mark::Ended {
                status: status.unwrap_or(0),
                number,
            }),
            ("J" | "E", _) => Some(Mark::JobOutput {
                number: number?,
                open: kind == "J",
            }),
            ("H", _) => Some(Mark::History(HistoryAsk {
                skip: skip.unwrap_or(0),
                limit: limit.unwrap_or(0),
                matching: matching.unwrap_or_default(),
            })),
            _ => None,
        }
    }
}

fn decode(value: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// A run that ended, as the session marked it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinishedRun {
    pub command: String,
    pub cwd: String,
    /// The command's place in the shell's own history, when its shell
    /// numbered it: the session record keeps runs by this, so a person
    /// typing the same line as a request does not take its id.
    pub number: Option<u64>,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub exit: i32,
    pub output: Vec<u8>,
    pub truncated: bool,
    /// The shell's job number, for a background job's run: it is no line
    /// the person or a request typed.
    pub job: Option<u32>,
}

impl FinishedRun {
    /// Keep `bytes` of its output, up to [`MAX_OUTPUT_BYTES`].
    fn keep(&mut self, bytes: &[u8]) {
        let room = MAX_OUTPUT_BYTES.saturating_sub(self.output.len());
        if bytes.len() > room {
            self.truncated = true;
        }
        self.output
            .extend_from_slice(bytes.get(..room.min(bytes.len())).unwrap_or_default());
    }
}

/// The run going on in a session, and the background jobs running beside
/// it.
#[derive(Debug, Default)]
pub struct Runs {
    open: Option<FinishedRun>,
    /// Job runs, by their number.
    jobs: std::collections::BTreeMap<u64, FinishedRun>,
    /// The job run whose output is arriving now.
    job_output: Option<u64>,
}

impl Runs {
    /// Follow `piece`, at `now_ms`; a run its end mark finishes comes back.
    pub fn follow(&mut self, piece: &Piece, now_ms: i64) -> Option<FinishedRun> {
        match piece {
            Piece::Output(bytes) => {
                let run = match self.job_output {
                    Some(number) => self.jobs.get_mut(&number),
                    None => self.open.as_mut(),
                };
                if let Some(run) = run {
                    run.keep(bytes);
                }
                None
            }
            Piece::Mark(Mark::JobStarted {
                number,
                job,
                command,
                cwd,
            }) => {
                self.jobs.insert(
                    *number,
                    FinishedRun {
                        command: command.clone(),
                        cwd: cwd.clone(),
                        number: Some(*number),
                        started_ms: now_ms,
                        ended_ms: now_ms,
                        exit: 0,
                        output: Vec::new(),
                        truncated: false,
                        job: Some(*job),
                    },
                );
                None
            }
            Piece::Mark(Mark::JobOutput { number, open }) => {
                self.job_output = open.then_some(*number);
                None
            }
            Piece::Mark(Mark::JobEnded { number, status }) => {
                let mut run = self.jobs.remove(number)?;
                run.ended_ms = now_ms;
                run.exit = *status;
                Some(run)
            }
            Piece::Mark(Mark::History(_)) => None,
            Piece::Mark(Mark::Started { command, cwd, .. }) => {
                self.open = Some(FinishedRun {
                    command: command.clone(),
                    cwd: cwd.clone(),
                    number: None,
                    started_ms: now_ms,
                    ended_ms: now_ms,
                    exit: 0,
                    output: Vec::new(),
                    truncated: false,
                    job: None,
                });
                None
            }
            Piece::Mark(Mark::Ended { status, number }) => {
                let mut run = self.open.take()?;
                run.ended_ms = now_ms;
                run.exit = *status;
                // The end says which command it was when both sides number
                // their runs; a start that named none takes the end's.
                run.number = run.number.or(*number);
                Some(run)
            }
            Piece::Mark(Mark::Prompt) => {
                self.open = None;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    /// The marks a real session's rc file writes ([`ush::runs`]), split
    /// across reads at every byte, make one run with its output; a mark
    /// without the session's key is output.
    #[test]
    fn marks_split_anywhere_make_runs_and_forged_ones_stay_output() {
        let key = "0123456789abcdef0123456789abcdef";
        let stream = format!(
            "\x1b]133;A;k={key}\x07$ \x1b]133;C;k={key};cmd={};cwd={};t=4\x07hello\r\n\
             \x1b]133;D;0;k=forged\x07more\r\n\x1b]133;D;3;k={key};t=4\x07\x1b]133;A;k={key}\x07$ ",
            b64("echo hello"),
            b64("/home/you")
        );
        for split in [1, 2, 7, stream.len()] {
            let mut marks = Marks::new(key);
            let mut runs = Runs::default();
            let mut finished = Vec::new();
            let mut shown = Vec::new();
            for chunk in stream.as_bytes().chunks(split) {
                for piece in marks.push(chunk) {
                    if let Piece::Output(bytes) = &piece {
                        shown.extend_from_slice(bytes);
                    }
                    finished.extend(runs.follow(&piece, 7));
                }
            }
            let [run] = finished.as_slice() else {
                panic!("one run at split {split}: {finished:?}");
            };
            assert_eq!(run.command, "echo hello");
            assert_eq!(run.cwd, "/home/you");
            assert_eq!(run.exit, 3);
            assert_eq!(run.number, Some(4), "the shell's own history number");
            let output = String::from_utf8_lossy(&run.output);
            assert!(
                output.starts_with("hello\r\n\x1b]133;D;0;k=forged\x07more"),
                "{output:?}"
            );
            let shown = String::from_utf8_lossy(&shown);
            assert!(!shown.contains(&format!("k={key}")), "{shown:?}");
            assert!(
                shown.contains("k=forged"),
                "the forged mark is output: {shown:?}"
            );
        }
    }

    #[test]
    fn output_past_the_limit_is_cut_and_says_so() {
        let mut runs = Runs::default();
        runs.follow(
            &Piece::Mark(Mark::Started {
                command: "yes".into(),
                cwd: String::new(),
                number: None,
            }),
            1,
        );
        runs.follow(&Piece::Output(vec![b'y'; MAX_OUTPUT_BYTES + 10]), 2);
        let run = runs
            .follow(
                &Piece::Mark(Mark::Ended {
                    status: 130,
                    number: None,
                }),
                3,
            )
            .unwrap();
        assert_eq!(run.output.len(), MAX_OUTPUT_BYTES);
        assert!(run.truncated);
        assert_eq!((run.started_ms, run.ended_ms, run.exit), (1, 3, 130));
    }
}

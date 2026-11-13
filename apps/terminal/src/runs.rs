//! The command runs of a session, as its shell marks them (docs/app-model/ush.md, "Command runs").

use base64::Engine as _;

pub const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

const MAX_MARK_BYTES: usize = 16 * 1024;

#[derive(Clone, Default)]
pub struct RunQueue {
    inner: std::rc::Rc<std::cell::RefCell<RunQueueState>>,
}

#[derive(Default)]
struct RunQueueState {
    waiting: Vec<Asked>,
    pane: String,
}

pub struct Asked {
    pub request: ui::tilo_resource::Id,
    pub command: String,
    pub requested_by: String,
    pub agent: bool,
    pub reply: ui::framework::EffectReply,
}

impl RunQueue {
    pub fn ask(&self, asked: Asked) {
        self.inner.borrow_mut().waiting.push(asked);
    }

    pub fn take(&self) -> Vec<Asked> {
        std::mem::take(&mut self.inner.borrow_mut().waiting)
    }

    pub fn set_pane(&self, pane: &str) {
        pane.clone_into(&mut self.inner.borrow_mut().pane);
    }

    #[must_use]
    pub fn runs(&self, pane: &str) -> bool {
        let own = &self.inner.borrow().pane;
        !own.is_empty() && own == pane
    }
}

#[must_use]
pub fn new_key() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    Prompt,
    Started {
        command: String,
        cwd: String,
        number: Option<u64>,
    },
    Ended {
        status: i32,
        number: Option<u64>,
    },
    JobStarted {
        number: u64,
        job: u32,
        command: String,
        cwd: String,
    },
    JobOutput {
        number: u64,
        open: bool,
    },
    JobEnded {
        number: u64,
        status: i32,
    },
    History(HistoryAsk),
    Held {
        agent: bool,
        refusal: Box<ui::tilo_actions::remote::host_account::Refusal>,
    },
    Released,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryAsk {
    pub skip: usize,
    pub limit: usize,
    pub matching: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Output(Vec<u8>),
    Mark(Mark),
}

#[derive(Debug)]
pub struct Marks {
    key: String,
    pending: Vec<u8>,
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

    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

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
        let mut agent = false;
        let mut refusal = None;
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
                Some(("a", value)) => agent = value == "1",
                Some(("r", value)) => refusal = Some(decode(value)?),
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
            ("W", _) => Some(Mark::Held {
                agent,
                refusal: Box::new(serde_json::from_str(&refusal?).ok()?),
            }),
            ("U", _) => Some(Mark::Released),
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinishedRun {
    pub command: String,
    pub cwd: String,
    pub number: Option<u64>,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub exit: i32,
    pub output: Vec<u8>,
    pub truncated: bool,
    pub job: Option<u32>,
    pub refusal: Option<Box<ui::tilo_actions::remote::host_account::Refusal>>,
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

#[derive(Debug, Default)]
pub struct Runs {
    open: Option<FinishedRun>,
    jobs: std::collections::BTreeMap<u64, FinishedRun>,
    job_output: Option<u64>,
}

impl Runs {
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
                        refusal: None,
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
            Piece::Mark(Mark::History(_) | Mark::Released) => None,
            Piece::Mark(Mark::Held { refusal, .. }) => {
                if let Some(run) = self.open.as_mut() {
                    run.refusal = Some(refusal.clone());
                }
                None
            }
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
                    refusal: None,
                });
                None
            }
            Piece::Mark(Mark::Ended { status, number }) => {
                let mut run = self.open.take()?;
                run.ended_ms = now_ms;
                run.exit = *status;
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

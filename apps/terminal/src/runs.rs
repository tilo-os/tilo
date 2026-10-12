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
    /// A command line starts running in `cwd`.
    Started { command: String, cwd: String },
    /// The command ended with `status`.
    Ended { status: i32 },
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
}

const START: &[u8] = b"\x1b]133;";

impl Marks {
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            pending: Vec::new(),
        }
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
    fn mark(&self, body: &[u8]) -> Option<Mark> {
        let body = std::str::from_utf8(body).ok()?;
        let mut fields = body.split(';');
        let kind = fields.next()?;
        let mut key = None;
        let mut command = None;
        let mut cwd = None;
        let mut status = None;
        for field in fields {
            match field.split_once('=') {
                Some(("k", value)) => key = Some(value),
                Some(("cmd", value)) => command = Some(decode(value)?),
                Some(("cwd", value)) => cwd = Some(decode(value)?),
                Some(_) => {}
                None => status = field.parse().ok(),
            }
        }
        if key != Some(self.key.as_str()) {
            return None;
        }
        match kind {
            "A" => Some(Mark::Prompt),
            "C" => Some(Mark::Started {
                command: command?,
                cwd: cwd.unwrap_or_default(),
            }),
            "D" => Some(Mark::Ended {
                status: status.unwrap_or(0),
            }),
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
    pub started_ms: i64,
    pub ended_ms: i64,
    pub exit: i32,
    pub output: Vec<u8>,
    pub truncated: bool,
}

/// The run going on in a session.
#[derive(Debug, Default)]
pub struct Runs {
    open: Option<FinishedRun>,
}

impl Runs {
    /// Follow `piece`, at `now_ms`; a run its end mark finishes comes back.
    pub fn follow(&mut self, piece: &Piece, now_ms: i64) -> Option<FinishedRun> {
        match piece {
            Piece::Output(bytes) => {
                if let Some(run) = &mut self.open {
                    let room = MAX_OUTPUT_BYTES.saturating_sub(run.output.len());
                    if bytes.len() > room {
                        run.truncated = true;
                    }
                    run.output
                        .extend_from_slice(bytes.get(..room.min(bytes.len())).unwrap_or_default());
                }
                None
            }
            Piece::Mark(Mark::Started { command, cwd }) => {
                self.open = Some(FinishedRun {
                    command: command.clone(),
                    cwd: cwd.clone(),
                    started_ms: now_ms,
                    ended_ms: now_ms,
                    exit: 0,
                    output: Vec::new(),
                    truncated: false,
                });
                None
            }
            Piece::Mark(Mark::Ended { status }) => {
                let mut run = self.open.take()?;
                run.ended_ms = now_ms;
                run.exit = *status;
                Some(run)
            }
            Piece::Mark(Mark::Prompt) => {
                self.open = None;
                None
            }
        }
    }

    /// Whether a run is going on.
    #[must_use]
    pub fn running(&self) -> bool {
        self.open.is_some()
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
            "\x1b]133;A;k={key}\x07$ \x1b]133;C;k={key};cmd={};cwd={}\x07hello\r\n\
             \x1b]133;D;0;k=forged\x07more\r\n\x1b]133;D;3;k={key}\x07\x1b]133;A;k={key}\x07$ ",
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
            }),
            1,
        );
        runs.follow(&Piece::Output(vec![b'y'; MAX_OUTPUT_BYTES + 10]), 2);
        let run = runs
            .follow(&Piece::Mark(Mark::Ended { status: 130 }), 3)
            .unwrap();
        assert_eq!(run.output.len(), MAX_OUTPUT_BYTES);
        assert!(run.truncated);
        assert_eq!((run.started_ms, run.ended_ms, run.exit), (1, 3, 130));
    }
}

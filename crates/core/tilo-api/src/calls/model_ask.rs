//! One turn of the person's own model, asked of this computer's daemon: the account's
//! `account/models#ask` with `{workspace, request}`, invoked over the daemon's socket and followed
//! while the model writes it.
//!
//! While the model writes, the invocation's newest item carries the answer so far (`{"text",
//! "reasoning"}`, never a delta): what was not told yet of the newest one is the next delta. The
//! output is the whole turn, `{"Ok": …}` or `{"Err": …}` as serde writes a `Result`.

use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use tilo_action_line::LineClient;
use tilo_actions::remote::{
    ActionProvider, Caller, InvokeRequest, ReceiptStatus, RemoteActionTarget, UpdateMark,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_clock::time::Duration;

use super::Refusal;

/// The longest one turn is followed.
const TURN: Duration = Duration::from_secs(150);

/// One turn of the person's own model, `request` the conversation as `ModelAsk` describes it,
/// asked in the workspace this computer serves. `told` hears each `text` and `reasoning` delta
/// as the model writes it; the answer is the whole turn.
///
/// # Errors
/// No daemon listens here ([`Refusal`] `unavailable`), or the ask was refused or stopped.
pub async fn ask_model(
    request: &Value,
    told: &mut (dyn FnMut(&str, &Value) + Send),
) -> Result<Value> {
    let unavailable = |why: String| {
        anyhow::Error::new(Refusal {
            code: "unavailable".into(),
            message: why,
        })
    };
    let socket = super::daemon_socket().map_err(unavailable)?;
    let stream = tilo_files::native::socket::connect(&socket)
        .map_err(|error| unavailable(format!("no daemon answers here: {error}")))?;
    let client = LineClient::of_stream(stream)?;
    let calls = super::Calls::over(Arc::new(client.clone())).await?;
    let workspace = calls
        .served_workspace()
        .ok_or_else(|| anyhow!("this computer serves no workspace to ask your model in"))?;
    let mut arguments = Map::new();
    arguments.insert("workspace".into(), json!(workspace));
    arguments.insert("request".into(), request.clone());
    let asked = InvokeRequest::plain(
        RemoteActionTarget {
            provider: crate::account::PROVIDER.into(),
            address: ActionAddress::new(crate::account::MODELS, "ask"),
        },
        arguments,
    );
    let invocation = asked.invocation;
    // The daemon decides the ask as whoever connects; the caller here names nobody.
    let caller = Caller::local_person("");
    let refused = |why: String| {
        anyhow::Error::new(Refusal {
            code: "refused".into(),
            message: why,
        })
    };
    let receipt = client
        .invoke(&caller, asked)
        .await
        .ok_or_else(|| unavailable("the daemon did not answer".into()))?
        .map_err(|error| refused(error.to_string()))?;
    let mut tell = |kind: &str, data: Value| {
        told(kind, &data);
        true
    };
    // Followed on the client itself, not through a wait that keeps it: an ask let go of drops
    // its connection, and the daemon hears it left.
    follow(
        &client,
        &caller,
        invocation,
        receipt.status,
        "the daemon",
        &mut tell,
    )
    .await
    .map(Option::unwrap_or_default)
    .map_err(|(code, message)| {
        anyhow::Error::new(Refusal {
            code: code.into(),
            message,
        })
    })
}

/// Follow the ask `invocation` of `provider`, whose receipt said `status`, to its end, telling
/// `told` each `text` and `reasoning` delta as the model writes it: the whole turn, or `None`
/// once `told` answers `false` (nobody listens any more). `answering` names who answers it, for
/// when it stops.
///
/// # Errors
/// The ask was refused or stopped (`refused`), or nothing answered it in time (`unavailable`).
pub(crate) async fn follow<P: ActionProvider + Sync>(
    provider: &P,
    caller: &Caller,
    invocation: InvocationId,
    mut status: ReceiptStatus,
    answering: &str,
    told: &mut (dyn FnMut(&str, Value) -> bool + Send),
) -> Result<Option<Value>, (&'static str, String)> {
    let now = tilo_clock::now();
    let until = now.checked_add(TURN).unwrap_or(now);
    let mut seen = UpdateMark::Accepted;
    let mut shown = Shown::default();
    // Whether a look made after the provider said nothing more would come was made.
    let mut past_the_end = false;
    let stopped = || ("unavailable", format!("{answering} stopped answering"));
    loop {
        match status {
            ReceiptStatus::Accepted if tilo_clock::now() >= until => {
                return Err(("unavailable", "the model took too long".into()));
            }
            ReceiptStatus::Accepted => {}
            ReceiptStatus::Completed { output } => {
                let answer = output.value.unwrap_or_default();
                // The whole turn replaces the last item: what it wrote after the newest item
                // read is told before it.
                return Ok(shown.tell(&written_in(&answer), told).then_some(answer));
            }
            ReceiptStatus::Failed { error } => return Err(("refused", error.message)),
            ReceiptStatus::Rejected { error } => return Err(("refused", error.to_string())),
            ReceiptStatus::Cancelled { .. } => {
                return Err(("refused", "the ask was stopped".into()));
            }
        }
        // Looked at before a wait is asked for: a wait is for what comes after what was read,
        // and one asked for beside a look that turns out to hold a change is a wait whose
        // answer is thrown away — the provider moved for a caller that never read it, and what
        // it wrote next is then read in one piece with that change instead of on its own.
        // Over a connection the wait still carries what was last read, so nothing that lands
        // between this look and the wait is lost.
        let state = provider.observe(caller, &invocation).ok_or_else(stopped)?;
        if UpdateMark::of(&state.update) == seen {
            // Nothing new: wait for the invocation to move, then look again.
            match provider.next_change(caller, &invocation) {
                Some(change) => {
                    change.await.ok_or_else(stopped)?;
                    past_the_end = false;
                }
                // A provider that answers no wait at all has seen the invocation end: the look
                // after it reads where the turn ended. Only a look that still shows nothing is
                // the provider having gone quiet.
                None if !past_the_end => past_the_end = true,
                None => return Err(stopped()),
            }
            continue;
        }
        past_the_end = false;
        seen = UpdateMark::of(&state.update);
        if let InvocationUpdate::Item { value, .. } = &state.update
            && !shown.tell(value, told)
        {
            return Ok(None);
        }
        status = state.receipt().status;
    }
}

/// The words and reasoning of a whole turn `answer`, `{"Ok": {"message": {"content": [...]}}}`,
/// as an item carries an answer so far.
fn written_in(answer: &Value) -> Value {
    let (mut text, mut reasoning) = (String::new(), String::new());
    let parts = answer
        .pointer("/Ok/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten();
    for part in parts {
        let said = part.get("text").and_then(Value::as_str).unwrap_or_default();
        match part.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(said),
            Some("reasoning") => reasoning.push_str(said),
            _ => {}
        }
    }
    json!({"text": text, "reasoning": reasoning})
}

/// How much of the answer so far was told.
#[derive(Default)]
struct Shown {
    text: usize,
    reasoning: usize,
}

impl Shown {
    /// Tell `told` what the answer so far `value` adds to what was told: whether it still
    /// listens.
    fn tell(&mut self, value: &Value, told: &mut (dyn FnMut(&str, Value) -> bool + Send)) -> bool {
        for kind in ["reasoning", "text"] {
            let shown = if kind == "text" {
                &mut self.text
            } else {
                &mut self.reasoning
            };
            let Some(said) = value.get(kind).and_then(Value::as_str) else {
                continue;
            };
            let Some(more) = said.get(*shown..).filter(|more| !more.is_empty()) else {
                continue;
            };
            *shown = said.len();
            if !told(kind, json!({"text": more})) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::BufReader;
    use std::sync::Mutex;
    use tilo_action_line_serve::serve_lines;
    use tilo_actions::ActionOutput;
    use tilo_actions::remote::{
        ActionCallError, ActionProvider, CataloguePage, CatalogueQuery, Completer,
        InvocationReceipt, InvocationState, InvokeArgument, Pending, pending,
    };

    fn heard() -> (Vec<(String, Value)>, Shown) {
        (Vec::new(), Shown::default())
    }

    /// The whole turn tells what its last item did not.
    #[test]
    fn the_whole_turn_tells_the_rest() {
        let (mut heard, mut shown) = heard();
        let mut told = |kind: &str, data: Value| {
            heard.push((kind.to_owned(), data));
            true
        };
        shown.tell(&json!({"text": "on ", "reasoning": ""}), &mut told);
        let whole = json!({"Ok": {"message": {"role": "assistant", "content": [
            {"type": "text", "text": "on Tuesday"},
            {"type": "tool_call", "id": "1", "name": "x", "input": {}},
        ]}}});
        shown.tell(&written_in(&whole), &mut told);
        assert_eq!(
            heard,
            [
                ("text".to_owned(), json!({"text": "on "})),
                ("text".to_owned(), json!({"text": "Tuesday"})),
            ]
        );
    }

    /// Each snapshot tells only what it adds, so a missed item loses nothing.
    #[test]
    fn a_snapshot_tells_only_what_it_adds() {
        let (mut heard, mut shown) = heard();
        let mut told = |kind: &str, data: Value| {
            heard.push((kind.to_owned(), data));
            true
        };
        assert!(shown.tell(&json!({"text": "Hel", "reasoning": ""}), &mut told));
        assert!(shown.tell(&json!({"text": "Hello wor", "reasoning": "hm"}), &mut told));
        assert!(shown.tell(&json!({"text": "Hello wor", "reasoning": "hm"}), &mut told));
        assert_eq!(
            heard,
            [
                ("text".to_owned(), json!({"text": "Hel"})),
                ("reasoning".to_owned(), json!({"text": "hm"})),
                ("text".to_owned(), json!({"text": "lo wor"})),
            ]
        );
    }

    /// The whole turn the stand-in's model answers.
    fn whole_turn(text: &str) -> Value {
        json!({"Ok": {"message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
                      "stop_reason": "end_turn"}})
    }

    /// Each ask's updates, and which of them it has reached.
    type Turns = HashMap<InvocationId, (Vec<InvocationUpdate>, Option<usize>)>;

    /// A stand-in for this computer's daemon: the account lists one workspace, `own`, and the
    /// model writes its answer a word per wait, then the whole turn. With `stall_first`, the first
    /// ask writes one word and nothing more, and the next answers `retry answer`.
    #[derive(Clone, Default)]
    struct Stand {
        words: usize,
        stall_first: bool,
        asked: Arc<Mutex<Vec<Map<String, Value>>>>,
        turns: Arc<Mutex<Turns>>,
        stalled: Arc<Mutex<Vec<Completer<()>>>>,
    }

    impl Stand {
        fn steps(&self, invocation: InvocationId, first: bool) -> Vec<InvocationUpdate> {
            let item = |sequence: u64, text: &str| InvocationUpdate::Item {
                invocation,
                sequence,
                value: json!({"text": text}),
            };
            let done = |text: &str| InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(whole_turn(text)),
                    resources: Vec::new(),
                },
            };
            match (self.stall_first, first) {
                (true, true) => vec![item(1, "partial")],
                (true, false) => vec![item(1, "retry answer"), done("retry answer")],
                (false, _) => (1..=self.words)
                    .map(|count| item(count as u64, &"word ".repeat(count)))
                    .chain([done(&"word ".repeat(self.words))])
                    .collect(),
            }
        }
    }

    impl ActionProvider for Stand {
        fn query(
            &self,
            _: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Ok(CataloguePage::from_rows(
                "r1".into(),
                Vec::new(),
                &query,
            )))
        }

        fn invoke(
            &self,
            _: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            let invocation = request.invocation;
            let args = match request.argument {
                InvokeArgument::Json(args) => args,
                InvokeArgument::None | InvokeArgument::Text(_) => Map::new(),
            };
            let address = &request.target.address;
            let status = match (address.resource.as_str(), address.action.as_str()) {
                (crate::account::WORKSPACES, "list") => ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(json!({"workspaces": [{"id": "own"}]})),
                        resources: Vec::new(),
                    },
                },
                (crate::account::MODELS, "ask") => {
                    let first = {
                        let mut asked = self.asked.lock().unwrap();
                        asked.push(args);
                        asked.len() == 1
                    };
                    let steps = self.steps(invocation, first);
                    self.turns.lock().unwrap().insert(invocation, (steps, None));
                    ReceiptStatus::Accepted
                }
                _ => return Pending::ready(Err(ActionCallError::UnknownAction)),
            };
            Pending::ready(Ok(InvocationReceipt { invocation, status }))
        }

        fn observe(&self, _: &Caller, id: &InvocationId) -> Option<InvocationState> {
            let (steps, at) = self.turns.lock().unwrap().get(id)?.clone();
            let update = at.map_or(InvocationUpdate::Accepted { invocation: *id }, |at| {
                steps[at].clone()
            });
            Some(InvocationState::new(*id, update))
        }

        /// Each wait moves the turn on by one update; past the last, it never moves again.
        fn next_change(&self, _: &Caller, id: &InvocationId) -> Option<Pending<()>> {
            let moved = {
                let mut turns = self.turns.lock().unwrap();
                let (steps, at) = turns.get_mut(id)?;
                let next = at.map_or(0, |at| at.saturating_add(1));
                let moved = next < steps.len();
                if moved {
                    *at = Some(next);
                }
                drop(turns);
                moved
            };
            if moved {
                return Some(Pending::ready(()));
            }
            let (stall, waits) = pending();
            self.stalled.lock().unwrap().push(stall);
            Some(waits)
        }

        fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(Ok(()))
        }
    }

    /// `stand` answering at this computer's daemon socket, in a home of its own: what each
    /// connection that ends says when it does.
    fn standing(
        stand: Stand,
    ) -> (
        tilo_test_env::ScopedEnv,
        tempfile::TempDir,
        std::sync::mpsc::Receiver<()>,
    ) {
        let home = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path())
            .set("XDG_STATE_HOME", home.path().join("state"))
            .set("TILO_IDENTITY_ROOT", home.path().join("identity"));
        let socket = super::super::daemon_socket().unwrap();
        let listener = tilo_files::native::socket::bind(&socket).unwrap();
        let (left, gone) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (stand, left) = (stand.clone(), left.clone());
                std::thread::spawn(move || {
                    let output = stream.try_clone().unwrap();
                    serve_lines(
                        |_| Ok((stand.clone(), Caller::local_person("ada"))),
                        &tilo_actions::remote::HostedContentPuts::default(),
                        None,
                        BufReader::new(stream),
                        output,
                    );
                    let _ = left.send(());
                });
            }
        });
        (env, home, gone)
    }

    /// Every word is told as the model writes it, and the whole turn is the answer; the ask
    /// names the workspace the daemon serves and carries the conversation as it was asked.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn each_word_is_told_as_the_model_writes_it_then_the_whole_turn() {
        let stand = Stand {
            words: 5,
            ..Stand::default()
        };
        let (_env, _home, _gone) = standing(stand.clone());
        let mut told = Vec::new();
        let answer = ask_model(&json!({"max_output_tokens": 64}), &mut |kind, data| {
            told.push((kind.to_owned(), data["text"].as_str().unwrap().to_owned()));
        })
        .await
        .unwrap();
        assert_eq!(told, vec![("text".to_owned(), "word ".to_owned()); 5]);
        assert_eq!(answer, whole_turn(&"word ".repeat(5)));
        let asked = stand.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0]["workspace"], "own");
        assert_eq!(asked[0]["request"]["max_output_tokens"], 64);
    }

    /// A provider that answers no wait at all after a look that still read the word before the
    /// end, as one having seen the invocation end does, and whose next look reads where the
    /// turn ended: the ask reads the end rather than taking the absent wait for silence.
    struct EndedWhileLooking {
        looks: Mutex<Vec<Option<InvocationState>>>,
        waits: Mutex<Vec<Option<()>>>,
    }

    impl ActionProvider for EndedWhileLooking {
        fn query(
            &self,
            _: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Ok(CataloguePage::from_rows(
                "r1".into(),
                Vec::new(),
                &query,
            )))
        }

        fn invoke(
            &self,
            _: &Caller,
            _: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            Pending::ready(Err(ActionCallError::UnknownAction))
        }

        fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
            self.looks.lock().unwrap().remove(0)
        }

        fn next_change(&self, _: &Caller, _: &InvocationId) -> Option<Pending<()>> {
            self.waits
                .lock()
                .unwrap()
                .remove(0)
                .map(|()| Pending::ready(()))
        }

        fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(Ok(()))
        }
    }

    /// A provider whose waits run out before its look shows the end still reads the whole turn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wait_that_answers_at_the_end_still_reads_the_whole_turn() {
        let invocation = InvocationId::new();
        let state = |update| InvocationState::new(invocation, update);
        let item = state(InvocationUpdate::Item {
            invocation,
            sequence: 1,
            value: json!({"text": "word "}),
        });
        let done = state(InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: Some(whole_turn("word ")),
                resources: Vec::new(),
            },
        });
        let provider = EndedWhileLooking {
            // The word is read before the end is seen: the wait asked for after that look
            // answers nothing, and the look after it reads where the turn ended.
            looks: Mutex::new(vec![
                Some(state(InvocationUpdate::Accepted { invocation })),
                Some(item.clone()),
                Some(item),
                Some(done),
            ]),
            waits: Mutex::new(vec![Some(()), None, None, None]),
        };
        let mut told = Vec::new();
        let answer = follow(
            &provider,
            &Caller::local_person("ada"),
            invocation,
            ReceiptStatus::Accepted,
            "the daemon",
            &mut |kind, data| {
                told.push((kind.to_owned(), data));
                true
            },
        )
        .await
        .unwrap()
        .expect("the whole turn");
        assert_eq!(answer, whole_turn("word "));
        assert_eq!(told, vec![("text".to_owned(), json!({"text": "word "}))]);
    }

    /// An ask let go of in the middle closes its connection, so the daemon hears it left, and
    /// the next ask is answered to its end.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_ask_let_go_of_leaves_and_the_next_is_answered() {
        let (_env, _home, gone) = standing(Stand {
            stall_first: true,
            ..Stand::default()
        });
        let (first_word, heard) = tokio::sync::oneshot::channel();
        let mut first_word = Some(first_word);
        let request = json!({});
        let mut told = |_: &str, data: &Value| {
            assert_eq!(data["text"], "partial");
            if let Some(first_word) = first_word.take() {
                let _ = first_word.send(());
            }
        };
        let asking = ask_model(&request, &mut told);
        tokio::select! {
            answered = asking => panic!("a stalled ask answered: {answered:?}"),
            _ = heard => {}
        }
        gone.recv_timeout(tilo_test_env::hang_bound())
            .expect("the daemon heard the ask leave");
        let mut told = Vec::new();
        let again = ask_model(&json!({}), &mut |_, data| {
            told.push(data["text"].as_str().unwrap().to_owned());
        })
        .await
        .unwrap();
        assert_eq!(told, ["retry answer"]);
        assert_eq!(again, whole_turn("retry answer"));
    }
}

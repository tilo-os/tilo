//! One program of a task, run for a host that serves the task-programs step
//! ([`tilo_actions::remote::task_programs`]): a desktop session and the device daemon alike run
//! it here, off the caller's pass, and answer what it said as the step's outcome.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_actions::remote::task_programs::{self as wire, Go};
use tilo_actions::remote::{ActionProvider, Outcome, Pending};

use crate::{McpServer, TaskProgram};

/// Run `program` on `api`, the session's own API, gone `go`: the program waits on the task's host and on
/// every call it makes, never on the caller's thread. A `stop` set while it runs is answered at
/// once, so the person hears that it ended, and the API refuses its further calls.
///
/// # Errors
/// No thread could take the program.
pub fn run<H>(
    api: Arc<McpServer<H>>,
    program: String,
    go: Go,
    this: Option<String>,
    stop: Option<Arc<AtomicBool>>,
) -> Result<Pending<Outcome>, String>
where
    H: ActionProvider + Send + Sync + 'static,
{
    let (complete, answered) = tilo_actions::remote::pending();
    std::thread::Builder::new()
        .name("task-program".into())
        .spawn(move || {
            let (ended, answer) = std::sync::mpsc::channel();
            let running = std::thread::Builder::new()
                .name("task-program-run".into())
                .spawn(move || {
                    let answered = match tilo_rt::try_app() {
                        Ok(runtime) => {
                            match runtime.block_on(api.task_program(
                                &program,
                                go,
                                this.as_deref(),
                            )) {
                                Ok(ran) => Ok(outcome(ran)),
                                Err(error) => unavailable(error.to_string()),
                            }
                        }
                        Err(error) => unavailable(error.to_string()),
                    };
                    // Nobody left to hear it is a run the person stopped: let go.
                    let _ = ended.send(answered);
                });
            if let Err(error) = running {
                complete.complete(unavailable(error.to_string()));
                return;
            }
            // Wait for the program, or for the person's Stop, whichever comes first.
            loop {
                match answer.recv_timeout(std::time::Duration::from_millis(50)) {
                    Ok(outcome) => {
                        complete.complete(outcome);
                        return;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        complete.complete(unavailable(
                            "the program's host ended without an answer".to_owned(),
                        ));
                        return;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if stop
                            .as_ref()
                            .is_some_and(|stop| stop.load(Ordering::Acquire))
                        {
                            complete.complete(stopped());
                            return;
                        }
                    }
                }
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(answered)
}

/// The program could not start or run at all.
fn unavailable(message: String) -> Outcome {
    Err(Box::new(tilo_actions::ActionFailure {
        code: "task_program_unavailable".into(),
        message,
        details: None,
        retryable: true,
    }))
}

/// The person stopped the program before it ended.
fn stopped() -> Outcome {
    Err(Box::new(tilo_actions::ActionFailure {
        code: "stopped".into(),
        message: "the person stopped this line".into(),
        details: None,
        retryable: false,
    }))
}

/// What a program answered, as the step's output.
fn outcome(ran: TaskProgram) -> tilo_actions::ActionOutput {
    let answer = wire::ProgramAnswer {
        text: ran.text,
        person_text: ran.person_text,
        failed: ran.failed,
        answer: ran.answer,
        refs: ran.refs,
    };
    tilo_actions::ActionOutput {
        value: Some(serde_json::to_value(answer).unwrap_or_default()),
        resources: Vec::new(),
    }
}

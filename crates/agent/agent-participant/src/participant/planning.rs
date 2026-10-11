//! The agent's planner on a thread of its own.
//!
//! A model plan takes seconds. On the agent's loop it would hold back
//! everything else the loop does in that time: remote actions go
//! unanswered, presence goes stale, a pause or stop goes unseen and keys
//! and desktop changes wait. So the loop hands each plan to [`Planning`]'s
//! thread and goes on; the thread wakes the loop
//! ([`AgentControl::wake`]) the moment the plan is ready, and the loop
//! takes it on its next pump.
//!
//! A plan may read the records it is about while it is made (a model's
//! `read_view`). The records live on the loop, so the planner's thread asks
//! the loop for each read ([`ReadAsk`]), wakes it, and waits for the answer;
//! the loop answers on its next pump, free to as it is not planning itself.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use tilo_actions::ActionOutput;
use tilo_actions::door::ActionAddress;
use tilo_browser_peer::DocumentRoute;

use crate::control::AgentControl;
use crate::planner::{PlanContext, Planner, Reads, Step, observation};

/// One plan asked of the planner's thread.
struct Job {
    task: String,
    text: String,
    context: PlanContext,
    continues: bool,
}

/// A plan the planner's thread made.
pub(super) struct Planned {
    /// The task it is for.
    pub task: String,
    /// What was planned: the task's text, or the rest of it.
    pub text: String,
    /// It plans the rest of a running task (a `Continue` step), not a new
    /// task.
    pub continues: bool,
    pub steps: Result<Vec<Step>, String>,
    /// What the plan's reads found, as observations.
    pub read: Vec<String>,
    /// When the planner returned it.
    pub ready_at: Instant,
}

/// A read the planner's thread asks of the loop while it plans.
pub(super) struct ReadAsk {
    pub route: DocumentRoute,
    pub address: ActionAddress,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// Where the loop answers.
    pub reply: mpsc::Sender<Result<ActionOutput, String>>,
}

/// The planner's thread, and the plan it is making.
pub(super) struct Planning {
    /// Dropped with the participant, which ends the thread after the plan
    /// it is making.
    jobs: mpsc::Sender<Job>,
    plans: mpsc::Receiver<Planned>,
    /// Reads the plan being made asks of the loop.
    reads: mpsc::Receiver<ReadAsk>,
    /// The task whose plan the thread is making.
    pending: Option<String>,
}

/// [`Reads`] on the planner's thread: each read is asked of the loop.
struct LoopReads {
    asks: mpsc::Sender<ReadAsk>,
    control: Arc<AgentControl>,
    found: Vec<String>,
}

impl Reads for LoopReads {
    fn read(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, String> {
        let (reply, answer) = mpsc::channel();
        self.asks
            .send(ReadAsk {
                route: route.clone(),
                address: address.clone(),
                arguments: arguments.clone(),
                reply,
            })
            .map_err(|_| "the agent stopped".to_string())?;
        self.control.wake();
        let output = answer
            .recv()
            .map_err(|_| "the agent stopped before it read".to_string())??;
        if let Some(seen) = observation(&route.app_id, address, &output) {
            self.found.push(seen);
        }
        Ok(output)
    }
}

impl Planning {
    /// Start `planner` on its own thread; each plan it finishes, and each
    /// read it asks, wakes `control`'s loop.
    pub(super) fn start(
        mut planner: Box<dyn Planner>,
        control: Arc<AgentControl>,
    ) -> Result<Self, String> {
        let (jobs, asked) = mpsc::channel::<Job>();
        let (made, plans) = mpsc::channel();
        let (read_asks, reads) = mpsc::channel();
        std::thread::Builder::new()
            .name("agent-plan".into())
            .spawn(move || {
                while let Ok(job) = asked.recv() {
                    let mut loop_reads = LoopReads {
                        asks: read_asks.clone(),
                        control: Arc::clone(&control),
                        found: Vec::new(),
                    };
                    let steps = planner.plan_reading(&job.text, &job.context, &mut loop_reads);
                    let answer = Planned {
                        task: job.task,
                        text: job.text,
                        continues: job.continues,
                        steps,
                        read: loop_reads.found,
                        ready_at: Instant::now(),
                    };
                    if made.send(answer).is_err() {
                        return;
                    }
                    control.wake();
                }
            })
            .map_err(|error| format!("the agent's planner did not start: {error}"))?;
        Ok(Self {
            jobs,
            plans,
            reads,
            pending: None,
        })
    }

    /// Whether a plan is being made.
    pub(super) const fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Ask for the steps of `text` for task `task` (the rest of it when
    /// `continues`).
    ///
    /// # Errors
    ///
    /// The planner's thread is gone.
    pub(super) fn ask(
        &mut self,
        task: String,
        text: String,
        context: PlanContext,
        continues: bool,
    ) -> Result<(), String> {
        self.jobs
            .send(Job {
                task: task.clone(),
                text,
                context,
                continues,
            })
            .map_err(|_| "the agent's planner stopped".to_string())?;
        self.pending = Some(task);
        Ok(())
    }

    /// The reads the plan being made asks now, for the loop to answer.
    pub(super) fn asked_reads(&self) -> Vec<ReadAsk> {
        self.reads.try_iter().collect()
    }

    /// The plan made since the last call, if one is ready. A thread that
    /// died while planning fails the plan it was making.
    pub(super) fn take(&mut self) -> Option<Planned> {
        let planned = match self.plans.try_recv() {
            Ok(planned) => planned,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Planned {
                task: self.pending.clone()?,
                text: String::new(),
                continues: false,
                steps: Err("the agent's planner stopped".into()),
                read: Vec::new(),
                ready_at: Instant::now(),
            },
        };
        if self.pending.as_deref() == Some(planned.task.as_str()) {
            self.pending = None;
        }
        Some(planned)
    }
}

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
//! A picture the plan shows its model is asked of the loop the same way
//! ([`LookAsk`]): the room's content store is the loop's.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use tilo_actions::ActionOutput;
use tilo_actions::door::ActionAddress;
use tilo_actions::door::ActionDescriptor;
use tilo_browser_peer::DocumentRoute;

use crate::control::AgentControl;
use crate::planner::{
    Answered, Obligations, PlanContext, PlanRole, Planner, Reads, Step, answered, rendered_picture,
};

/// One plan asked of the planner's thread.
struct Job {
    task: String,
    text: String,
    context: PlanContext,
    continues: bool,
    role: Option<PlanRole>,
}

/// A plan the planner's thread made.
pub(super) struct Planned {
    pub task: String,
    /// What was planned: the task's text, or the rest of it.
    pub text: String,
    /// It plans the rest of a running task (a `Continue` step), not a new
    /// task.
    pub continues: bool,
    pub steps: Result<Vec<Step>, String>,
    /// What the plan's reads found, as observations, each with the app
    /// of the record read and whether it carries content from outside the
    /// user's data.
    pub read: Vec<(String, Answered)>,
    /// The pictures the plan's reads rendered, by content address.
    pub rendered: Vec<String>,
    /// When the planner returned it.
    pub ready_at: Instant,
    /// Whether a model made it (not steps written out or stated outright).
    pub by_model: bool,
    /// The checklist it declared.
    pub obligations: Obligations,
    /// Who made it, in a coordinated task.
    pub role: Option<PlanRole>,
}

/// A read the planner's thread asks of the loop while it plans.
pub(super) struct ReadAsk {
    pub route: DocumentRoute,
    pub address: ActionAddress,
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// Where the loop answers.
    pub reply: ReplyAsk,
}

/// What the loop answers a read with, and the operation's declaration as it
/// published it (what the answer's contract says about its content).
pub(super) type ReplyAsk = mpsc::Sender<(Result<ActionOutput, String>, Option<ActionDescriptor>)>;

/// The bytes of content the planner's thread asks of the loop while it
/// plans: a picture to show its model.
pub(super) struct LookAsk {
    pub hash: String,
    /// Where the loop answers.
    pub reply: mpsc::Sender<Result<Vec<u8>, String>>,
}

/// The planner's thread, and the plan it is making.
pub(super) struct Planning {
    /// Dropped with the participant, which ends the thread after the plan
    /// it is making.
    jobs: mpsc::Sender<Job>,
    plans: mpsc::Receiver<Planned>,
    /// Reads the plan being made asks of the loop.
    reads: mpsc::Receiver<ReadAsk>,
    /// Content the plan being made asks of the loop.
    looks: mpsc::Receiver<LookAsk>,
    /// The task whose plan the thread is making.
    pending: Option<String>,
}

/// [`Reads`] on the planner's thread: each read is asked of the loop.
struct LoopReads {
    asks: mpsc::Sender<ReadAsk>,
    looks: mpsc::Sender<LookAsk>,
    control: Arc<AgentControl>,
    found: Vec<(String, Answered)>,
    rendered: Vec<String>,
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
        let (output, descriptor) = answer
            .recv()
            .map_err(|_| "the agent stopped before it read".to_string())?;
        let output = output?;
        if let Some(answered) = answered(&route.app_id, address, &output, descriptor.as_ref()) {
            self.found.push((route.app_id.clone(), answered));
        }
        if let Some(picture) = rendered_picture(&output) {
            self.rendered.push(picture);
        }
        Ok(output)
    }

    fn look(&mut self, hash: &str) -> Result<Vec<u8>, String> {
        let (reply, answer) = mpsc::channel();
        self.looks
            .send(LookAsk {
                hash: hash.to_owned(),
                reply,
            })
            .map_err(|_| "the agent stopped".to_string())?;
        self.control.wake();
        answer
            .recv()
            .map_err(|_| "the agent stopped before it looked".to_string())?
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
        let (look_asks, looks) = mpsc::channel();
        // Named as the agent's own thread: a model call is logged under the
        // name of the thread that makes it (`TILO_AGENT_USAGE_LOG`), and
        // its usage belongs to the agent.
        let name = std::thread::current()
            .name()
            .unwrap_or("agent-plan")
            .to_owned();
        std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                while let Ok(job) = asked.recv() {
                    let mut loop_reads = LoopReads {
                        asks: read_asks.clone(),
                        looks: look_asks.clone(),
                        control: Arc::clone(&control),
                        found: Vec::new(),
                        rendered: Vec::new(),
                    };
                    let steps = crate::model::as_role(job.role.clone(), || {
                        planner.plan_reading(&job.text, &job.context, &mut loop_reads)
                    });
                    let answer = Planned {
                        task: job.task,
                        text: job.text,
                        continues: job.continues,
                        steps,
                        read: loop_reads.found,
                        rendered: loop_reads.rendered,
                        ready_at: Instant::now(),
                        by_model: planner.last_planned_by_model(),
                        obligations: planner.take_obligations(),
                        role: job.role,
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
            looks,
            pending: None,
        })
    }

    /// Whether a plan is being made.
    pub(super) const fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Ask for the steps of `text` for task `task` (the rest of it when
    /// `continues`), made by `role` in a coordinated task.
    pub(super) fn ask(
        &mut self,
        (task, text): (String, String),
        context: PlanContext,
        continues: bool,
        role: Option<PlanRole>,
    ) -> Result<(), String> {
        self.jobs
            .send(Job {
                task: task.clone(),
                text,
                context,
                continues,
                role,
            })
            .map_err(|_| "the agent's planner stopped".to_string())?;
        self.pending = Some(task);
        Ok(())
    }

    /// The reads the plan being made asks now, for the loop to answer.
    pub(super) fn asked_reads(&self) -> Vec<ReadAsk> {
        self.reads.try_iter().collect()
    }

    /// The content the plan being made asks now, for the loop to answer.
    pub(super) fn asked_looks(&self) -> Vec<LookAsk> {
        self.looks.try_iter().collect()
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
                rendered: Vec::new(),
                ready_at: Instant::now(),
                by_model: false,
                obligations: Obligations::default(),
                role: None,
            },
        };
        if self.pending.as_deref() == Some(planned.task.as_str()) {
            self.pending = None;
        }
        Some(planned)
    }
}

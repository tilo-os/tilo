//! The agent's planner on a thread of its own.
//!
//! A model plan takes seconds. On the agent's loop it would hold back
//! everything else the loop does in that time: remote actions go
//! unanswered, presence goes stale, a pause or stop goes unseen and keys
//! and desktop changes wait. So the loop hands each plan to [`Planning`]'s
//! thread and goes on; the thread wakes the loop
//! ([`AgentControl::wake`]) the moment the plan is ready, and the loop
//! takes it on its next pump.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use crate::control::AgentControl;
use crate::planner::{PlanContext, Planner, Step};

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
    /// It plans the rest of a running task (a `Continue` step), not a new
    /// task.
    pub continues: bool,
    pub steps: Result<Vec<Step>, String>,
    /// When the planner returned it.
    pub ready_at: Instant,
}

/// The planner's thread, and the plan it is making.
pub(super) struct Planning {
    /// Dropped with the participant, which ends the thread after the plan
    /// it is making.
    jobs: mpsc::Sender<Job>,
    plans: mpsc::Receiver<Planned>,
    /// The task whose plan the thread is making.
    pending: Option<String>,
}

impl Planning {
    /// Start `planner` on its own thread; each plan it finishes wakes
    /// `control`'s loop.
    pub(super) fn start(
        mut planner: Box<dyn Planner>,
        control: Arc<AgentControl>,
    ) -> Result<Self, String> {
        let (jobs, asked) = mpsc::channel::<Job>();
        let (made, plans) = mpsc::channel();
        std::thread::Builder::new()
            .name("agent-plan".into())
            .spawn(move || {
                while let Ok(job) = asked.recv() {
                    let steps = planner.plan(&job.text, &job.context);
                    let planned = Planned {
                        task: job.task,
                        continues: job.continues,
                        steps,
                        ready_at: Instant::now(),
                    };
                    if made.send(planned).is_err() {
                        return;
                    }
                    control.wake();
                }
            })
            .map_err(|error| format!("the agent's planner did not start: {error}"))?;
        Ok(Self {
            jobs,
            plans,
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

    /// The plan made since the last call, if one is ready. A thread that
    /// died while planning fails the plan it was making.
    pub(super) fn take(&mut self) -> Option<Planned> {
        let planned = match self.plans.try_recv() {
            Ok(planned) => planned,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Planned {
                task: self.pending.clone()?,
                continues: false,
                steps: Err("the agent's planner stopped".into()),
                ready_at: Instant::now(),
            },
        };
        if self.pending.as_deref() == Some(planned.task.as_str()) {
            self.pending = None;
        }
        Some(planned)
    }
}

//! Run receipts under `target/dev-runs/<id>/run.json`, in the same spirit as
//! `tools/dev.py`: written for every command, never overwriting earlier
//! evidence, carrying per-phase timings and no ambient environment.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use serde_json::{Value, json};

/// Iteration-speed goals. A receipt flags any phase that exceeds its goal.
pub const GOALS: &[(&str, f64, &str)] = &[
    ("build", 60.0, "warm component build"),
    ("apply", 10.0, "swap and session restart"),
    ("boot", 30.0, "cached VM boot to SSH"),
];

#[derive(Debug, Clone, Serialize)]
pub struct Phase {
    pub name: String,
    pub seconds: f64,
}

pub struct Receipt {
    dir: PathBuf,
    started: Instant,
    started_at: String,
    command: Vec<String>,
    phases: Vec<Phase>,
    fields: serde_json::Map<String, Value>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn round(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}

impl Receipt {
    /// `base` is the worktree root, or a per-user data dir outside one.
    pub fn new(base: &Path, command: Vec<String>) -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string();
        Self {
            dir: base.join("target").join("dev-runs").join(id),
            started: Instant::now(),
            started_at: now(),
            command,
            phases: Vec::new(),
            fields: serde_json::Map::new(),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join("run.json")
    }

    pub fn set(&mut self, key: &str, value: impl Serialize) {
        self.fields.insert(
            key.into(),
            serde_json::to_value(value).unwrap_or(Value::Null),
        );
    }

    /// Time `f` as the named phase.
    pub fn phase<T>(&mut self, name: &str, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let value = f();
        self.phases.push(Phase {
            name: name.into(),
            seconds: round(start.elapsed().as_secs_f64()),
        });
        value
    }

    pub fn record(&mut self, name: &str, seconds: f64) {
        self.phases.push(Phase {
            name: name.into(),
            seconds: round(seconds),
        });
    }

    /// `name 1.2s, name 3.4s` for the phases recorded so far.
    pub fn summary(&self) -> String {
        self.phases
            .iter()
            .map(|p| format!("{} {:.1}s", p.name, p.seconds))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Phases over their goal, as human sentences.
    pub fn slow(&self) -> Vec<String> {
        exceeded(&self.phases)
    }

    /// Write the receipt. Failure to write is reported, never fatal: the
    /// command's own outcome matters more than its paperwork.
    pub fn finish(mut self, outcome: Result<&Value, &Value>) -> Option<PathBuf> {
        let (status, key, value) = match outcome {
            Ok(result) => ("success", "result", result.clone()),
            Err(error) => ("failure", "error", error.clone()),
        };
        let mut receipt = json!({
            "tool": "tilo-vm",
            "version": env!("CARGO_PKG_VERSION"),
            "command": self.command,
            "status": status,
            "started_at": self.started_at,
            "finished_at": now(),
            "elapsed_seconds": round(self.started.elapsed().as_secs_f64()),
            "phases": self.phases,
            "goals": GOALS.iter().map(|(phase, limit, what)| json!({"phase": phase, "seconds": limit, "goal": what})).collect::<Vec<_>>(),
            "slow": exceeded(&self.phases),
        });
        if let Some(object) = receipt.as_object_mut() {
            object.append(&mut self.fields);
            object.insert(key.into(), value);
        }
        let path = self.path();
        let written = std::fs::create_dir_all(self.dir.parent().unwrap_or(&self.dir))
            .and_then(|()| std::fs::create_dir(&self.dir))
            .and_then(|()| std::fs::write(&path, format!("{receipt:#}\n")));
        match written {
            Ok(()) => Some(path),
            Err(error) => {
                eprintln!(
                    "tilo-vm: could not write receipt {}: {error}",
                    path.display()
                );
                None
            }
        }
    }
}

fn exceeded(phases: &[Phase]) -> Vec<String> {
    phases
        .iter()
        .filter_map(|phase| {
            let (_, limit, what) = GOALS.iter().find(|(name, _, _)| *name == phase.name)?;
            (phase.seconds > *limit)
                .then(|| format!("{what} took {:.1}s (goal < {limit:.0}s)", phase.seconds))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Receipts record phases and flag slow ones, and never overwrite.
    #[test]
    fn receipts_record_phases_flag_slow_ones_and_never_overwrite() {
        // Phase: receipt_records_phases_and_flags_slow_ones.
        {
            let dir = tempfile::tempdir().unwrap();
            let mut receipt = Receipt::new(dir.path(), vec!["tilo-vm".into(), "swap".into()]);
            receipt.phase("assemble", || ());
            receipt.phases.push(Phase {
                name: "build".into(),
                seconds: 75.0,
            });
            receipt.set("slot", "a");
            assert_eq!(
                receipt.slow(),
                ["warm component build took 75.0s (goal < 60s)"]
            );
            let path = receipt.finish(Ok(&json!({"generation": 3}))).unwrap();
            assert!(path.starts_with(dir.path().join("target/dev-runs")));
            let written: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(written["status"], "success");
            assert_eq!(written["slot"], "a");
            assert_eq!(written["result"]["generation"], 3);
            assert_eq!(written["phases"][0]["name"], "assemble");
            assert_eq!(written["slow"].as_array().unwrap().len(), 1);
        }

        // Phase: receipts_never_overwrite.
        {
            let dir = tempfile::tempdir().unwrap();
            let a = Receipt::new(dir.path(), vec![])
                .finish(Ok(&json!({})))
                .unwrap();
            let b = Receipt::new(dir.path(), vec![])
                .finish(Err(&json!({"error": "x"})))
                .unwrap();
            assert_ne!(a, b);
        }
    }
}

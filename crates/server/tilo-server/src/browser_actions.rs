//! What hosted Browser runs free, and the card it asks for past that.
//!
//! The server runs a workspace's Browser page actions on the host's pool, and
//! counts them per account and billing period in a log beside its state
//! ([`BrowserMeter`]): [`BrowserMeter::admit`] before an action runs,
//! [`BrowserMeter::record`] the moment one ran. The plans file
//! (`TILO_HOSTED_PLANS`, the one the hosted-VM service admits by) sets the
//! free page actions of an account without a card
//! ([`Sells::browser_free_actions`](linux_vm::hosted::plan::Sells::browser_free_actions));
//! the meter reads it on every call, so an operator changes the allowance
//! without a restart.
//!
//! An account the plans file names has a card on file and is never stopped.
//! Any other account may run its period's free page actions; past them,
//! [`BrowserMeter::admit`] answers the refusal whoever ran the action reads:
//! [`Reason::NoPaymentMethod`](tilo_actions::remote::server_account::Reason::NoPaymentMethod),
//! the free count in words, and the provider's page to add a card. A host
//! whose plans file sells nothing counts nothing and refuses nothing.

use std::path::PathBuf;
use std::sync::Mutex;

use linux_vm::hosted::plan::{self, Plans};
use tilo_actions::remote::server_account;
use tilo_server_account::browser_actions::{Log, PageAction};
use tilo_server_account::{ACTIONS_LOG_ENV, PLANS_ENV, period};

/// The page actions of hosted Browser, counted in the log the host's
/// environment names.
pub struct BrowserMeter {
    /// The plans file, reread on every admission.
    plans: PathBuf,
    /// The page actions the account ran.
    log: Log,
    /// One line of the log at a time, so threads never interleave their
    /// writes.
    recording: Mutex<()>,
}

impl BrowserMeter {
    /// The meter that reads `plans` and keeps its count in `log`, a file
    /// the server puts in its state directory.
    #[must_use]
    pub fn new(plans: PathBuf, log: PathBuf) -> Self {
        Self {
            plans,
            log: Log::new(log),
            recording: Mutex::new(()),
        }
    }

    /// The meter the host's environment names: the plans file
    /// ([`PLANS_ENV`]) and the log of the page actions it counts
    /// ([`ACTIONS_LOG_ENV`]). `None` without a log: the host runs no hosted
    /// Browser, or counts none of its actions.
    ///
    /// # Errors
    ///
    /// A log is named and the plans file is not.
    pub fn from_env() -> Result<Option<Self>, String> {
        let Some(log) = std::env::var_os(ACTIONS_LOG_ENV) else {
            return Ok(None);
        };
        let plans = std::env::var_os(PLANS_ENV).ok_or_else(|| {
            format!("{ACTIONS_LOG_ENV} is set, so {PLANS_ENV} must name the plans file")
        })?;
        Ok(Some(Self::new(PathBuf::from(plans), PathBuf::from(log))))
    }

    /// Whether `account` may run a page action at `now_ms`, and why not when
    /// it may not: the free page actions of its billing period, or the card
    /// that continues past them.
    ///
    /// # Errors
    ///
    /// The account has no card on file and ran every free page action its
    /// billing period has.
    pub fn admit(&self, account: &str, now_ms: u64) -> Result<(), server_account::Refusal> {
        let plans = Plans::load(&self.plans).unwrap_or_else(|error| {
            tracing::error!(
                plans = %self.plans.display(),
                %error,
                "the plans file does not read; Browser runs unmetered"
            );
            Plans::default()
        });
        let used = self.used(&plans, account, now_ms);
        plan::admit_browser(&plans, account, used)
            .map_err(|refusal| refusal.typed(plans.sells.as_ref(), account))
    }

    /// Count a page action of `account` in `workspace` at `now_ms`. A log
    /// that cannot be written is the caller's to tell: the action still ran,
    /// and the next admission counts it only if this came back `Ok`.
    ///
    /// # Errors
    ///
    /// The log cannot be written.
    pub fn record(&self, account: &str, workspace: &str, now_ms: u64) -> std::io::Result<()> {
        let _recording = self
            .recording
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.log.record(&PageAction {
            account: account.to_owned(),
            workspace: workspace.to_owned(),
            at_ms: now_ms,
        })
    }

    /// The page actions `account` ran in its billing period around `now_ms`.
    fn used(&self, plans: &Plans, account: &str, now_ms: u64) -> u64 {
        let (from_ms, to_ms) = period(plans, account, now_ms);
        self.log.count(account, from_ms, to_ms)
    }
}

/// The log is what the server places Browser with: a page action is
/// admitted for the account of the workspace it is booked to, counted at the
/// moment it ran.
impl crate::hosted_browser::BrowserMeter for BrowserMeter {
    fn admit(&self, account: &str) -> Result<(), server_account::Refusal> {
        BrowserMeter::admit(self, account, tilo_clock::unix_now_ms())
    }

    fn record(&self, workspace: &str, account: &str) {
        // The action ran: the log says so, and one it cannot write is
        // said rather than taken out on the caller.
        if let Err(error) =
            BrowserMeter::record(self, account, workspace, tilo_clock::unix_now_ms())
        {
            tracing::warn!(%error, %account, %workspace, "a Browser page action was not counted");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONTH_START_MS: u64 = 1_793_491_200_000; // 2026-11-01T00:00:00Z
    /// 2026-11-08T12:00:00Z
    const NOW_MS: u64 = MONTH_START_MS + 8 * 86_400_000 + 43_200_000;

    fn plans(dir: &std::path::Path, free: Option<u32>) -> PathBuf {
        let path = dir.join("plans.json");
        let mut sells = serde_json::json!({
            "host": "tilo.sh",
            "plans": [],
            "add_payment_method": "https://tilo.sh/billing/card?account={account}",
        });
        if let Some(free) = free {
            sells["browser_free_actions"] = serde_json::json!(free);
        }
        std::fs::write(&path, serde_json::json!({ "sells": sells }).to_string()).unwrap();
        path
    }

    tilo_test_env::scenario! {
        fn the_browser_meter_counts_each_accounts_page_actions;

        phase the_count_survives_a_reopen_and_rolls_over_with_the_period;
        {
            let dir = tempfile::tempdir().unwrap();
            let plans = plans(dir.path(), Some(2));
            let log = dir.path().join("browser-actions.jsonl");
            {
                let meter = BrowserMeter::new(plans.clone(), log.clone());
                assert_eq!(meter.admit("acct-1", NOW_MS), Ok(()));
                meter.record("acct-1", "ws1", NOW_MS).unwrap();
                meter.record("acct-1", "ws2", NOW_MS).unwrap();
            }
            // phase: a new meter on the same log counts what the old one did
            {
                let meter = BrowserMeter::new(plans.clone(), log.clone());
                let refusal = meter.admit("acct-1", NOW_MS).unwrap_err();
                assert_eq!(
                    refusal.message,
                    "Browser runs 2 page actions a month free on tilo.sh, and this account used \
                 them. Add a card to continue."
                );
                assert_eq!(
                    refusal
                        .step(server_account::NextStepKind::AddPaymentMethod)
                        .and_then(|step| step.url.as_deref()),
                    Some("https://tilo.sh/billing/card?account=acct-1")
                );
                assert_eq!(refusal.reason, server_account::Reason::NoPaymentMethod);
            }
            // phase: a new period counts nothing
            {
                let meter = BrowserMeter::new(plans, log);
                let next_month = MONTH_START_MS + 31 * 86_400_000;
                assert_eq!(meter.admit("acct-1", next_month), Ok(()));
            }
        }

        phase accounts_record_concurrently_without_losing_counts;
        {
            let dir = tempfile::tempdir().unwrap();
            let meter = std::sync::Arc::new(BrowserMeter::new(
                plans(dir.path(), Some(201)),
                dir.path().join("browser-actions.jsonl"),
            ));
            let threads: Vec<_> = (0..8)
                .map(|thread| {
                    let meter = std::sync::Arc::clone(&meter);
                    std::thread::spawn(move || {
                        for action in 0..25 {
                            meter
                                .record("acct-1", &format!("ws{action}"), NOW_MS)
                                .unwrap();
                            meter
                                .record("acct-2", &format!("ws{thread}"), NOW_MS)
                                .unwrap();
                        }
                    })
                })
                .collect();
            for thread in threads {
                thread.join().unwrap();
            }
            let log = Log::new(dir.path().join("browser-actions.jsonl"));
            let (from_ms, to_ms) = (MONTH_START_MS, MONTH_START_MS + 30 * 86_400_000);
            assert_eq!(log.count("acct-1", from_ms, to_ms), 200);
            assert_eq!(log.count("acct-2", from_ms, to_ms), 200);
            assert_eq!(meter.admit("acct-1", NOW_MS), Ok(()));
            assert_eq!(meter.admit("acct-2", NOW_MS), Ok(()));
        }

        phase a_host_that_sells_nothing_counts_nothing;
        {
            let dir = tempfile::tempdir().unwrap();
            let log = dir.path().join("browser-actions.jsonl");
            let meter = BrowserMeter::new(dir.path().join("plans.json"), log.clone());
            assert_eq!(meter.admit("acct-1", NOW_MS), Ok(()));
            meter.record("acct-1", "ws1", NOW_MS).unwrap();
            assert_eq!(meter.admit("acct-1", NOW_MS), Ok(()));
            // and an account the file names has a card, so it is never stopped
            let plans = plans(dir.path(), Some(0));
            let meter = BrowserMeter::new(plans, log);
            assert!(meter.admit("acct-1", NOW_MS).is_err());
            let mut file: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.path().join("plans.json")).unwrap()).unwrap();
            file["accounts"] = serde_json::json!({
                "acct-1": {"max_running": 1, "max_vms": 3, "sizes": ["small"]}
            });
            std::fs::write(
                dir.path().join("plans.json"),
                serde_json::to_vec(&file).unwrap(),
            )
            .unwrap();
            assert_eq!(meter.admit("acct-1", NOW_MS), Ok(()));
        }
    }
}

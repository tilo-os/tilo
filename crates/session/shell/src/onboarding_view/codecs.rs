//! The optional media codecs step: whether an install was asked for, how
//! far it got, and the watch on the codecs extension's status.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::TryRecvError;

use os_actions::codec_pack::{CodecAction, PackState};

use super::OnboardingView;
use crate::shell::i18n::t_args;

impl OnboardingView {
    /// Whether the page still offers its own Install (or Try again) choice:
    /// nothing runs and nothing was asked yet, or the last try failed.
    pub(super) fn codec_choice_offered(&self) -> bool {
        let failed = self.codec_error.is_some() || self.codec_status.state == PackState::Failed;
        (failed && self.codec_running.is_none()) || !self.codec_install_requested()
    }

    pub(super) fn codec_install_requested(&self) -> bool {
        self.codec_running.is_some()
            || self.codec_error.is_some()
            || self.codec_status.state != PackState::NotInstalled
    }

    pub(super) fn start_codec_install(&mut self) {
        let Some(setup) = self.codec_setup.as_ref() else {
            return;
        };
        if self.codec_running.is_some() {
            return;
        }
        if setup.watch_system && self.codec_watching.is_none() {
            let changed = Arc::downgrade(&self.codec_changed);
            self.codec_watching = os_actions::codec_pack::watch(move || {
                if let Some(changed) = changed.upgrade() {
                    changed.store(true, Ordering::Release);
                }
            })
            .ok();
        }
        let runner = Arc::clone(&setup.runner);
        let changed = Arc::clone(&self.codec_changed);
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("onboarding-codecs".into())
            .spawn(move || {
                // The install outlives setup if the person moves on; its
                // result then reaches them as a notification instead.
                let _ = tx.send(runner(CodecAction::Install));
                changed.store(true, Ordering::Release);
            });
        match spawned {
            Ok(_) => {
                self.codec_error = None;
                self.codec_worker_succeeded = None;
                self.codec_running = Some(rx);
            }
            Err(error) => {
                self.codec_error = Some(t_args(
                    "onboarding-codecs-installer-failed",
                    &[("error", error.to_string().into())],
                ));
            }
        }
        self.invalidation.structure();
    }

    /// Pick up the worker's result and status-file changes.
    pub(super) fn refresh_codec_state(&mut self) {
        let Some(setup) = self.codec_setup.as_ref() else {
            return;
        };
        let mut changed = self.codec_changed.swap(false, Ordering::AcqRel);
        if let Some(events) = self.codec_running.as_ref() {
            match events.try_recv() {
                Ok(result) => {
                    self.codec_worker_succeeded = Some(result.is_ok());
                    self.codec_error = result.err();
                    self.codec_running = None;
                    changed = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.codec_worker_succeeded = Some(false);
                    self.codec_running = None;
                    changed = true;
                }
            }
        }
        if changed {
            let status = (setup.status)();
            if status != self.codec_status {
                self.codec_status = status;
            }
            self.invalidation.structure();
        }
    }

    /// Block until the install started from the page returns (tests).
    #[cfg(test)]
    pub(super) fn wait_for_codec_install(&mut self) {
        if let Some(events) = self.codec_running.as_ref()
            && let Ok(result) = events.recv()
        {
            self.codec_worker_succeeded = Some(result.is_ok());
            self.codec_error = result.err();
            self.codec_running = None;
            self.codec_changed.store(true, Ordering::Release);
        }
    }
}

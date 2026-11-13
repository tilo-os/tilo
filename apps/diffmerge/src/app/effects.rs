//! Answering Diff & Merge's effects.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, Live};
use ui::portal::SaveDestination;

use super::{label_and_text, one_sided};
use crate::actions::{DiffmergeEffect, diff_block_fingerprint};
use crate::doc::Session;
use crate::resource::session;

pub(crate) enum Asked {
    View(DiffmergeEffect),
    Choose,
    Sample,
    Opened(Result<(), String>),
    Notice(String),
    Folders(crate::folder::FolderComparison),
    Stage(Option<String>),
}

pub(crate) struct Store {
    pub(super) live: Option<Live<Session>>,
    pub(super) saved_place: Option<SaveDestination>,
    pub(super) saved_text: Option<String>,
    pub(super) open_reply: Option<EffectReply>,
    asked: Vec<Asked>,
    pub(super) wake: Option<ui::WakeHandle>,
}

pub(crate) type Shared = Rc<RefCell<Store>>;

impl Store {
    pub(super) fn shared() -> Shared {
        Rc::new(RefCell::new(Self {
            live: None,
            saved_place: None,
            saved_text: None,
            open_reply: None,
            asked: Vec::new(),
            wake: None,
        }))
    }

    pub(crate) fn ask(&mut self, asked: Asked) {
        self.asked.push(asked);
        if let Some(wake) = &self.wake {
            wake.wake();
        }
    }

    pub(super) fn take_asked(&mut self) -> Vec<Asked> {
        std::mem::take(&mut self.asked)
    }

    pub(super) fn merged(&self) -> Option<String> {
        let state = self.live.as_ref()?.get();
        state.merge.is_some().then(|| state.merged.clone())
    }

    /// Whether `fingerprint` names the change block at `block` of the
    /// comparison the document holds.
    fn shows_block(&self, block: usize, fingerprint: &str) -> bool {
        let Some(live) = &self.live else {
            return false;
        };
        let state = live.get();
        state.comparison.as_ref().is_some_and(|comparison| {
            diff_block_fingerprint(&comparison.model(), block).as_deref() == Some(fingerprint)
        })
    }
}

pub(crate) fn answer(
    store: &Shared,
    effect: DiffmergeEffect,
    cx: &mut EffectCx<Session>,
) -> EffectAnswer {
    match effect {
        DiffmergeEffect::Show { block, fingerprint } => {
            let shown = usize::try_from(block)
                .map_err(|_| "this change target is no longer available".to_owned())?;
            if !store.borrow().shows_block(shown, &fingerprint) {
                return Err(
                    "the comparison changed; reopen this change from the current results".into(),
                );
            }
            store
                .borrow_mut()
                .ask(Asked::View(DiffmergeEffect::Show { block, fingerprint }));
            Ok(Effected::done())
        }
        DiffmergeEffect::ShowDiff
        | DiffmergeEffect::ShowMerge
        | DiffmergeEffect::PreviousChange
        | DiffmergeEffect::NextChange => {
            store.borrow_mut().ask(Asked::View(effect));
            Ok(Effected::done())
        }
        DiffmergeEffect::LoadSample => {
            let dispatcher = cx.dispatcher();
            dispatcher.group(|| {
                for invoke in crate::resource::sample_invocations() {
                    dispatcher.submit(invoke);
                }
            });
            store.borrow_mut().ask(Asked::Sample);
            Ok(Effected::done())
        }
        DiffmergeEffect::OpenFiles {
            left: Some(left),
            right: Some(right),
        } => {
            let store = Rc::clone(store);
            Ok(cx.thread(
                move |_| -> Result<session::OpenComparison, String> {
                    let (left_label, left) = label_and_text(&left)?;
                    let (right_label, right) = label_and_text(&right)?;
                    Ok(session::OpenComparison {
                        left_label,
                        left,
                        right_label,
                        right,
                    })
                },
                move |(), opened, cx| show_opened(&store, opened, cx),
            ))
        }
        DiffmergeEffect::OpenFiles {
            left: None,
            right: None,
        } => {
            let mut store = store.borrow_mut();
            if store.open_reply.is_some() {
                return Err("another file operation is already running".into());
            }
            store.open_reply = Some(cx.reply());
            store.ask(Asked::Choose);
            Ok(Effected::Pending)
        }
        DiffmergeEffect::OpenFiles { .. } => {
            Err("left and right paths must be supplied together".into())
        }
        DiffmergeEffect::OpenOne { path, side } => {
            let store = Rc::clone(store);
            Ok(cx.thread(
                move |_progress| label_and_text(&path),
                move |(), opened, cx| {
                    let opened = opened.map(|(label, text)| one_sided(&side, label, text));
                    show_opened(&store, opened, cx)
                },
            ))
        }
        DiffmergeEffect::SaveMerged { path } => super::save::answer_save(store, cx, path, false),
        DiffmergeEffect::SaveMergedAs => super::save::answer_save(store, cx, None, true),
        DiffmergeEffect::CompareFolders { left, right } => {
            super::folders::answer_compare_folders(store, cx, left, right)
        }
    }
}

fn show_opened(
    store: &Shared,
    opened: Result<session::OpenComparison, String>,
    cx: &EffectCx<Session>,
) -> EffectAnswer {
    let answer = match opened {
        Ok(opened) => {
            cx.submit(Session::OPEN_COMPARISON.with(opened));
            Ok(())
        }
        Err(reason) => Err(reason),
    };
    store.borrow_mut().ask(Asked::Opened(answer.clone()));
    answer.map(|()| Effected::done())
}

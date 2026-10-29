//! Answering Diff & Merge's effects.
//!
//! The view's effect handler shares a [`Store`] with the window: what this
//! device keeps about saving the merged result, the open-files effect a
//! chooser answers, and what an effect asks of the window, which it does on
//! its next frame ([`Asked`]). What an effect opens goes into the document
//! through its dispatcher; the window follows the document.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, Live};
use ui::portal::SaveDestination;

use super::{label_and_text, one_sided};
use crate::actions::{DiffmergeEffect, diff_block_fingerprint};
use crate::doc::Session;
use crate::resource::session;

/// What an effect asked of the window, which it does on its next frame.
pub(crate) enum Asked {
    /// Show a view of what is open: a mode, the next or previous change, or
    /// one change block.
    View(DiffmergeEffect),
    /// Open the host's file chooser for the open-files effect waiting on it.
    Choose,
    /// The samples were opened.
    Sample,
    /// An effect opened a comparison, or says why it could not.
    Opened(Result<(), String>),
    /// One line about a save or a folder read, for the status bar.
    Notice(String),
    /// Two folders were compared: their listing.
    Folders(crate::folder::FolderComparison),
    /// A capture's scenario, by name (`None` for a plain capture).
    Stage(Option<String>),
}

/// What the window and the view's effect handler share.
pub(crate) struct Store {
    /// The session as the document holds it; `None` for a window outside
    /// the framework.
    pub(super) live: Option<Live<Session>>,
    /// Where the merged result was last written, so Save writes there again
    /// without asking. Kept on this device: a place is a fact about here,
    /// not about the document.
    pub(super) saved_place: Option<SaveDestination>,
    /// The merged text as it was when it was last written: equal to the
    /// document's text means nothing is waiting to be saved.
    pub(super) saved_text: Option<String>,
    /// The caller of the open-files effect a chooser is answering.
    pub(super) open_reply: Option<EffectReply>,
    /// What effects asked of the window since its last frame.
    asked: Vec<Asked>,
    /// Wakes the window to do what was asked.
    pub(super) wake: Option<ui::WakeHandle>,
}

/// The store as the window and the view's effect handler share it.
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

    /// Ask the window for `asked` on its next frame.
    pub(crate) fn ask(&mut self, asked: Asked) {
        self.asked.push(asked);
        if let Some(wake) = &self.wake {
            wake.wake();
        }
    }

    /// What effects asked of the window since it last looked.
    pub(super) fn take_asked(&mut self) -> Vec<Asked> {
        std::mem::take(&mut self.asked)
    }

    /// The merged result while a merge is open, as the document holds it.
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

/// Answer one of the app's effects, from a toolbar button, the palette, an
/// agent or a script.
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

/// Open a comparison whose files were read, or say why they could not be.
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

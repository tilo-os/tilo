//! "Keep this display change?" for display tools.
//!
//! A tool's configuration (`wlr-output-management`) that could leave the
//! person unable to see or reach their screen applies, and then the session
//! asks whether to keep it; without an answer it reverts after
//! [`CONFIRM_FOR`]. A tool the person trusts skips the question unless the
//! change leaves no display on. Right after the person reverted a layout, a
//! tool cannot put the same layout straight back.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use smithay::output::{Mode, Output};

use crate::output::{ModeRequest, OutputChange, find_mode};
use crate::state::State;

/// How long the person has to keep a risky change before it reverts.
pub const CONFIRM_FOR: Duration = Duration::from_secs(15);
/// How long a reverted layout stays refused from the tool that sent it.
const REAPPLY_AFTER_REVERT: Duration = Duration::from_secs(60);

/// Whether a tool's display change can apply without asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChangeRisk {
    /// Applies directly, the way every other compositor treats the tool.
    Safe,
    /// Could leave the person unable to see or reach their screen: they are
    /// asked to keep it, unless they trust the tool.
    Risky,
    /// Leaves no display on. Asked about whoever the tool is.
    NoDisplay,
}

/// An output as it is now, for [`assess_change`].
#[derive(Clone, Debug)]
pub(crate) struct HeadNow {
    pub name: String,
    pub enabled: bool,
    pub mode: Option<Mode>,
    /// The modes the connector lists.
    pub modes: Vec<Mode>,
    pub scale: f64,
}

/// The modes and scales an output has shown in a layout that was kept.
#[derive(Clone, Debug, Default)]
pub(crate) struct Shown {
    modes: Vec<Mode>,
    scales: Vec<f64>,
}

impl Shown {
    /// Enough for every scale step and a monitor's usual modes; the oldest
    /// entry makes room for a new one.
    const LIMIT: usize = 32;

    fn has_mode(&self, mode: Mode) -> bool {
        self.modes.contains(&mode)
    }

    fn has_scale(&self, scale: f64) -> bool {
        self.scales.iter().any(|known| same_scale(*known, scale))
    }

    fn note(&mut self, mode: Option<Mode>, scale: f64) {
        if let Some(mode) = mode.filter(|mode| !self.has_mode(*mode)) {
            if self.modes.len() >= Self::LIMIT {
                self.modes.remove(0);
            }
            self.modes.push(mode);
        }
        if !self.has_scale(scale) {
            if self.scales.len() >= Self::LIMIT {
                self.scales.remove(0);
            }
            self.scales.push(scale);
        }
    }
}

fn same_scale(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-3
}

/// How risky applying `changes` to `heads` is.
///
/// Risky is: a mode the connector does not list; turning off a display the
/// person is using (`watched`); or giving a display they are using a mode
/// or scale it has never shown in a layout that was kept. Everything else
/// (arranging, rotating, lighting up a newly plugged monitor at a listed
/// mode) is safe.
pub(crate) fn assess_change(
    changes: &[OutputChange],
    heads: &[HeadNow],
    watched: &HashSet<String>,
    shown: &HashMap<String, Shown>,
) -> ChangeRisk {
    let change_of = |head: &HeadNow| changes.iter().find(|change| change.name == head.name);
    let leaves_one_on = heads
        .iter()
        .any(|head| change_of(head).map_or(head.enabled, |change| change.enabled));
    if !leaves_one_on {
        return ChangeRisk::NoDisplay;
    }
    let risky = heads.iter().any(|head| {
        let Some(change) = change_of(head) else {
            return false;
        };
        let mode = change
            .mode
            .map(|request: ModeRequest| find_mode(&head.modes, request));
        if change.enabled && mode.is_some_and(|mode| mode.is_none()) {
            return true;
        }
        if !head.enabled || !watched.contains(&head.name) {
            return false;
        }
        if !change.enabled {
            return true;
        }
        let history = shown.get(&head.name);
        let new_mode = mode
            .flatten()
            .filter(|mode| Some(*mode) != head.mode)
            .is_some_and(|mode| !history.is_some_and(|h| h.has_mode(mode)));
        let new_scale = change
            .scale
            .filter(|scale| !same_scale(*scale, head.scale))
            .is_some_and(|scale| !history.is_some_and(|h| h.has_scale(scale)));
        new_mode || new_scale
    });
    if risky {
        ChangeRisk::Risky
    } else {
        ChangeRisk::Safe
    }
}

/// What the session hears about display tools' changes.
#[derive(Clone, Debug, PartialEq)]
pub enum DisplayEvent {
    /// A tool's change applied and the person must keep it within
    /// `within`, or it reverts: show the question, answer with
    /// [`crate::Compositor::keep_display_change`] or
    /// [`crate::Compositor::revert_display_change`].
    Confirm {
        id: u64,
        /// The executable of the tool, when known.
        tool: Option<PathBuf>,
        within: Duration,
    },
    /// The change asked about went back (answered, or the time ran out).
    Reverted { id: u64 },
    /// A tool's change is kept: remember it as the person's configuration.
    Kept { changes: Vec<OutputChange> },
    /// A tool tried to put back the layout the person just reverted; it was
    /// refused. Told once per revert.
    RefusedReapply { tool: Option<PathBuf> },
}

/// A change waiting for the person's answer.
#[derive(Debug)]
struct Asked {
    id: u64,
    tool: Option<PathBuf>,
    plan: Vec<OutputChange>,
    /// What puts the outputs back as they were before the question.
    undo: Vec<OutputChange>,
    deadline: Instant,
}

/// A layout the person reverted.
#[derive(Debug)]
struct Reverted {
    tool: Option<PathBuf>,
    plan: Vec<OutputChange>,
    at: Instant,
    told: bool,
}

/// Tools' display changes: trusted tools, pending questions, recent reverts.
#[derive(Debug, Default)]
pub(crate) struct DisplayChanges {
    trusted: HashSet<PathBuf>,
    shown: HashMap<String, Shown>,
    asked: Option<Asked>,
    reverted: Option<Reverted>,
    events: Vec<DisplayEvent>,
    next_id: u64,
}

impl DisplayChanges {
    pub(crate) fn set_trusted(&mut self, tools: impl IntoIterator<Item = PathBuf>) {
        self.trusted = tools.into_iter().collect();
    }

    pub(crate) fn take_events(&mut self) -> Vec<DisplayEvent> {
        std::mem::take(&mut self.events)
    }

    /// Remember what `heads` show now as shown in a kept layout.
    pub(crate) fn note_shown(&mut self, heads: &[HeadNow]) {
        for head in heads.iter().filter(|head| head.enabled) {
            self.shown
                .entry(head.name.clone())
                .or_default()
                .note(head.mode, head.scale);
        }
    }

    pub(crate) fn risk(
        &self,
        changes: &[OutputChange],
        heads: &[HeadNow],
        watched: &HashSet<String>,
    ) -> ChangeRisk {
        assess_change(changes, heads, watched, &self.shown)
    }

    /// Whether `tool` sending `plan` repeats a layout the person just
    /// reverted. The first repeat tells the session.
    pub(crate) fn refuses_reapply(
        &mut self,
        tool: Option<&PathBuf>,
        plan: &[OutputChange],
        now: Instant,
    ) -> bool {
        let Some(reverted) = self.reverted.as_mut() else {
            return false;
        };
        if now.duration_since(reverted.at) > REAPPLY_AFTER_REVERT {
            self.reverted = None;
            return false;
        }
        if reverted.tool.as_ref() != tool || reverted.plan != plan {
            return false;
        }
        if !reverted.told {
            reverted.told = true;
            self.events.push(DisplayEvent::RefusedReapply {
                tool: tool.cloned(),
            });
        }
        true
    }

    /// Whether a change of `risk` from `tool` is kept without asking.
    pub(crate) fn keeps_unasked(&self, risk: ChangeRisk, tool: Option<&PathBuf>) -> bool {
        match risk {
            ChangeRisk::Safe => true,
            ChangeRisk::Risky => tool.is_some_and(|tool| self.trusted.contains(tool)),
            ChangeRisk::NoDisplay => false,
        }
    }

    /// An applied change is kept.
    pub(crate) fn kept(&mut self, plan: Vec<OutputChange>, heads: &[HeadNow]) {
        self.note_shown(heads);
        self.events.push(DisplayEvent::Kept { changes: plan });
    }

    /// An applied change waits for the person. A question still open keeps
    /// its way back, so reverting returns to the layout before either.
    pub(crate) fn ask(
        &mut self,
        tool: Option<PathBuf>,
        plan: Vec<OutputChange>,
        mut undo: Vec<OutputChange>,
        now: Instant,
    ) -> u64 {
        if let Some(earlier) = self.asked.take() {
            undo.retain(|change| !earlier.undo.iter().any(|old| old.name == change.name));
            undo.extend(earlier.undo);
            self.events.push(DisplayEvent::Reverted { id: earlier.id });
        }
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        self.events.push(DisplayEvent::Confirm {
            id,
            tool: tool.clone(),
            within: CONFIRM_FOR,
        });
        self.asked = Some(Asked {
            id,
            tool,
            plan,
            undo,
            deadline: now.checked_add(CONFIRM_FOR).unwrap_or(now),
        });
        id
    }

    /// The person kept change `id`; false when it is not the open question.
    pub(crate) fn keep(&mut self, id: u64, heads: &[HeadNow]) -> bool {
        if self.asked.as_ref().is_none_or(|asked| asked.id != id) {
            return false;
        }
        let Some(asked) = self.asked.take() else {
            return false;
        };
        self.kept(asked.plan, heads);
        true
    }

    /// The person reverted change `id`: what puts the outputs back.
    pub(crate) fn revert(&mut self, id: u64, now: Instant) -> Option<Vec<OutputChange>> {
        if self.asked.as_ref().is_none_or(|asked| asked.id != id) {
            return None;
        }
        let asked = self.asked.take()?;
        self.events.push(DisplayEvent::Reverted { id });
        self.reverted = Some(Reverted {
            tool: asked.tool,
            plan: asked.plan,
            at: now,
            told: false,
        });
        Some(asked.undo)
    }

    /// The open question's way back once its time ran out.
    pub(crate) fn due(&mut self, now: Instant) -> Option<Vec<OutputChange>> {
        let id = self
            .asked
            .as_ref()
            .filter(|asked| now >= asked.deadline)?
            .id;
        self.revert(id, now)
    }
}

impl State {
    /// Every output as it is now.
    pub(crate) fn heads_now(&self) -> Vec<HeadNow> {
        let head = |output: &Output, enabled: bool| HeadNow {
            name: output.name(),
            enabled,
            mode: output.current_mode(),
            modes: output.modes(),
            scale: output.current_scale().fractional_scale(),
        };
        self.space
            .outputs()
            .map(|output| head(output, true))
            .chain(
                self.disabled_outputs
                    .iter()
                    .map(|output| head(output, false)),
            )
            .collect()
    }

    /// The outputs the person is using: under the pointer, and showing the
    /// window with keyboard focus.
    pub(crate) fn watched_outputs(&self) -> HashSet<String> {
        let mut watched: HashSet<String> = self
            .space
            .output_under(self.pointer)
            .map(Output::name)
            .collect();
        let focus = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        if let Some(focus) = focus {
            let window = self.space.elements().find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == &focus)
            });
            if let Some(window) = window {
                watched.extend(
                    self.space
                        .outputs_for_element(window)
                        .iter()
                        .map(Output::name),
                );
            }
        }
        watched
    }

    /// The changes that put the outputs `changes` names back as they are
    /// now.
    pub(crate) fn undo_for(&self, changes: &[OutputChange]) -> Vec<OutputChange> {
        changes
            .iter()
            .filter_map(|change| {
                if let Some(output) = self
                    .space
                    .outputs()
                    .find(|output| output.name() == change.name)
                {
                    let mode = output.current_mode().map(|mode| ModeRequest {
                        size: (mode.size.w, mode.size.h),
                        refresh_mhz: Some(mode.refresh),
                    });
                    let position = self
                        .space
                        .output_geometry(output)
                        .map(|geometry| (geometry.loc.x, geometry.loc.y));
                    return Some(OutputChange {
                        name: change.name.clone(),
                        enabled: true,
                        mode,
                        position,
                        scale: Some(output.current_scale().fractional_scale()),
                        transform: Some(output.current_transform()),
                    });
                }
                self.disabled_outputs
                    .iter()
                    .any(|output| output.name() == change.name)
                    .then(|| OutputChange::disable(change.name.clone()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use smithay::utils::Transform;

    use super::*;

    fn mode(w: i32, h: i32, refresh: i32) -> Mode {
        Mode {
            size: (w, h).into(),
            refresh,
        }
    }

    fn two_heads() -> Vec<HeadNow> {
        let head = |name: &str, modes: Vec<Mode>| HeadNow {
            name: name.into(),
            enabled: true,
            mode: modes.first().copied(),
            modes,
            scale: 1.0,
        };
        vec![
            head("eDP-1", vec![mode(2560, 1600, 60_000)]),
            head(
                "DP-1",
                vec![mode(2560, 1440, 144_000), mode(2560, 1440, 59_951)],
            ),
        ]
    }

    fn watching(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn monitor(change: OutputChange) -> Vec<OutputChange> {
        vec![OutputChange::enable("eDP-1"), change]
    }

    fn at_mode(w: i32, h: i32, refresh: i32) -> ModeRequest {
        ModeRequest {
            size: (w, h),
            refresh_mhz: Some(refresh),
        }
    }

    fn risk(
        changes: &[OutputChange],
        watched: &[&str],
        shown: &HashMap<String, Shown>,
    ) -> ChangeRisk {
        assess_change(changes, &two_heads(), &watching(watched), shown)
    }

    #[test]
    fn arranging_and_listed_modes_apply_directly() {
        let changes = monitor(OutputChange {
            mode: Some(at_mode(2560, 1440, 59_951)),
            position: Some((-2560, 0)),
            transform: Some(Transform::_90),
            scale: Some(1.25),
            ..OutputChange::enable("DP-1")
        });
        let none = HashMap::new();
        assert_eq!(risk(&changes, &["eDP-1"], &none), ChangeRisk::Safe);
        // Turning off a display nobody is using is safe too.
        let off = monitor(OutputChange::disable("DP-1"));
        assert_eq!(risk(&off, &["eDP-1"], &none), ChangeRisk::Safe);
    }

    #[test]
    fn an_unlisted_mode_is_risky_even_where_nobody_looks() {
        let changes = monitor(OutputChange {
            mode: Some(at_mode(2560, 1440, 75_000)),
            ..OutputChange::enable("DP-1")
        });
        assert_eq!(
            risk(&changes, &["eDP-1"], &HashMap::new()),
            ChangeRisk::Risky
        );
    }

    #[test]
    fn turning_off_the_display_in_use_is_risky() {
        let off = monitor(OutputChange::disable("DP-1"));
        assert_eq!(risk(&off, &["DP-1"], &HashMap::new()), ChangeRisk::Risky);
    }

    #[test]
    fn a_mode_or_scale_the_display_in_use_never_showed_is_risky_until_it_has() {
        let new_mode = monitor(OutputChange {
            mode: Some(at_mode(2560, 1440, 59_951)),
            ..OutputChange::enable("DP-1")
        });
        let new_scale = monitor(OutputChange {
            scale: Some(1.5),
            ..OutputChange::enable("DP-1")
        });
        let same_again = monitor(OutputChange {
            mode: Some(at_mode(2560, 1440, 144_000)),
            scale: Some(1.0),
            ..OutputChange::enable("DP-1")
        });
        let none = HashMap::new();
        assert_eq!(risk(&new_mode, &["DP-1"], &none), ChangeRisk::Risky);
        assert_eq!(risk(&new_scale, &["DP-1"], &none), ChangeRisk::Risky);
        assert_eq!(risk(&same_again, &["DP-1"], &none), ChangeRisk::Safe);

        let mut shown = Shown::default();
        shown.note(Some(mode(2560, 1440, 59_951)), 1.5);
        let shown = HashMap::from([("DP-1".to_owned(), shown)]);
        assert_eq!(risk(&new_mode, &["DP-1"], &shown), ChangeRisk::Safe);
        assert_eq!(risk(&new_scale, &["DP-1"], &shown), ChangeRisk::Safe);
    }

    #[test]
    fn a_change_with_every_display_off_is_its_own_risk() {
        let off = vec![
            OutputChange::disable("eDP-1"),
            OutputChange::disable("DP-1"),
        ];
        assert_eq!(risk(&off, &[], &HashMap::new()), ChangeRisk::NoDisplay);
    }

    #[test]
    fn only_trusted_tools_skip_the_question_and_never_with_every_display_off() {
        let mut changes = DisplayChanges::default();
        let tool = PathBuf::from("/usr/bin/kanshi");
        assert!(changes.keeps_unasked(ChangeRisk::Safe, None));
        assert!(!changes.keeps_unasked(ChangeRisk::Risky, Some(&tool)));
        changes.set_trusted([tool.clone()]);
        assert!(changes.keeps_unasked(ChangeRisk::Risky, Some(&tool)));
        assert!(!changes.keeps_unasked(ChangeRisk::Risky, None));
        assert!(!changes.keeps_unasked(ChangeRisk::NoDisplay, Some(&tool)));
    }

    #[test]
    fn an_unanswered_change_reverts_and_its_layout_is_refused_for_a_while() {
        let mut changes = DisplayChanges::default();
        let tool = Some(PathBuf::from("/usr/bin/wlr-randr"));
        let plan = vec![OutputChange::disable("DP-1")];
        let undo = vec![OutputChange::enable("DP-1")];
        let start = Instant::now();
        let id = changes.ask(tool.clone(), plan.clone(), undo.clone(), start);
        assert!(changes.due(start + Duration::from_secs(14)).is_none());
        assert_eq!(changes.due(start + CONFIRM_FOR), Some(undo));
        assert!(!changes.keep(id, &[]), "the question is gone");

        let later = start + Duration::from_secs(30);
        assert!(changes.refuses_reapply(tool.as_ref(), &plan, later));
        assert!(changes.refuses_reapply(tool.as_ref(), &plan, later));
        assert!(!changes.refuses_reapply(None, &plan, later), "another tool");
        assert!(!changes.refuses_reapply(tool.as_ref(), &plan, start + Duration::from_secs(80)));
        let events = changes.take_events();
        assert_eq!(
            events,
            vec![
                DisplayEvent::Confirm {
                    id,
                    tool: tool.clone(),
                    within: CONFIRM_FOR
                },
                DisplayEvent::Reverted { id },
                DisplayEvent::RefusedReapply { tool },
            ],
            "the refusal is told once"
        );
    }

    #[test]
    fn a_second_question_reverts_to_the_layout_before_the_first() {
        let mut changes = DisplayChanges::default();
        let now = Instant::now();
        let first_undo = vec![OutputChange {
            scale: Some(1.0),
            ..OutputChange::enable("DP-1")
        }];
        changes.ask(None, vec![], first_undo.clone(), now);
        let second_undo = vec![
            OutputChange {
                scale: Some(2.0),
                ..OutputChange::enable("DP-1")
            },
            OutputChange::enable("eDP-1"),
        ];
        let id = changes.ask(None, vec![], second_undo, now);
        let undo = changes.revert(id, now).expect("open question");
        assert!(undo.contains(&first_undo[0]));
        assert!(undo.contains(&OutputChange::enable("eDP-1")));
        assert_eq!(undo.len(), 2);
    }

    #[test]
    fn a_kept_change_counts_as_shown() {
        let mut changes = DisplayChanges::default();
        let mut heads = two_heads();
        heads[1].scale = 1.5;
        let id = changes.ask(
            None,
            vec![OutputChange::enable("DP-1")],
            vec![],
            Instant::now(),
        );
        assert!(changes.keep(id, &heads));
        let again = monitor(OutputChange {
            scale: Some(1.5),
            ..OutputChange::enable("DP-1")
        });
        assert_eq!(
            changes.risk(&again, &two_heads(), &watching(&["DP-1"])),
            ChangeRisk::Safe
        );
        assert!(matches!(
            changes.take_events().last(),
            Some(DisplayEvent::Kept { .. })
        ));
    }
}

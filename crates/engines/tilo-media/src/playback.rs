//! One watching or listening session's outputs: where it plays, how loudly, and how far each
//! output stands from the others.
//!
//! A [`PlaybackOutputs`] is the list a session holds; the app's actions edit it by continuation
//! (adding appends, moving replaces, removing takes out one). [`Playout`] applies a list to the
//! host that really plays it: the computer's own sound devices (through [`crate::audio_output`])
//! and cast sessions on a television or speaker. It takes a **list**, never one target, so a
//! video and a song play through the same piece.

/// Where one output plays.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OutputKind {
    /// A sound device of this computer or of a paired device, by the id the audio graph names it.
    Sink { device: String },
    /// A television or speaker reached by a cast protocol (`Miracast`, `Google Cast`, `AirPlay`,
    /// `FCast`), by the address the discovery found it at.
    Cast { protocol: String, target: String },
}

/// One output of a playback: where it plays, how loudly, and how it stands.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Output {
    /// What the app calls this output, stable while it lasts (`speaker`, `tv`).
    pub id: String,
    pub kind: OutputKind,
    /// What a person reads it as: a speaker's description or a receiver's name.
    pub label: String,
    /// `0.0..=1.0`; zero is silent.
    pub volume: f32,
    /// How late or early this output plays, in milliseconds, against the first output.
    pub sync_ms: i64,
    pub state: OutputState,
}

/// Where one output has got to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OutputState {
    /// Connecting, or sending the first frames.
    #[default]
    Connecting,
    Playing,
    Failed,
}

/// The outputs of one playback in order: the first is the one the others follow.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaybackOutputs {
    outputs: Vec<Output>,
}

/// What changed between two output lists.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputChanges {
    /// Outputs that went away, by id.
    pub removed: Vec<String>,
    /// Outputs that began, in order.
    pub started: Vec<Output>,
    /// Outputs still playing whose volume, sync or state moved.
    pub updated: Vec<Output>,
    /// The order the outputs stand in now, by id.
    pub order: Vec<String>,
}

impl PlaybackOutputs {
    #[must_use]
    pub fn new(outputs: Vec<Output>) -> Self {
        Self { outputs }
    }

    #[must_use]
    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Output> {
        self.outputs.iter().find(|output| output.id == id)
    }

    /// Add `output`, appending it after the outputs already there. An id already in the list is
    /// the one output it is: the new one replaces it in place, so a second device with the same
    /// name is no second output.
    pub fn add(&mut self, output: Output) {
        if let Some(at) = self.outputs.iter().position(|held| held.id == output.id)
            && let Some(held) = self.outputs.get_mut(at)
        {
            *held = output;
        } else {
            self.outputs.push(output);
        }
    }

    /// Move `output` to `at`, replacing whatever stands there. A position past the end appends
    /// it: continuation never fails on where a person dropped it.
    pub fn move_to(&mut self, output: Output, at: usize) {
        if let Some(held) = self.outputs.iter().position(|held| held.id == output.id) {
            self.outputs.remove(held);
        }
        let at = at.min(self.outputs.len());
        self.outputs.insert(at, output);
    }

    /// Take the output `id` out; the one it returns is what the playout stops.
    pub fn remove(&mut self, id: &str) -> Option<Output> {
        let at = self.outputs.iter().position(|output| output.id == id)?;
        Some(self.outputs.remove(at))
    }

    /// Set the output `id`'s volume, `0.0..=1.0`.
    pub fn set_volume(&mut self, id: &str, volume: f32) {
        if let Some(output) = self.outputs.iter_mut().find(|output| output.id == id) {
            output.volume = volume.clamp(0.0, 1.0);
        }
    }

    /// Set the output `id`'s sync against the first output, in whole milliseconds.
    pub fn set_sync(&mut self, id: &str, sync_ms: i64) {
        if let Some(output) = self.outputs.iter_mut().find(|output| output.id == id) {
            output.sync_ms = sync_ms;
        }
    }

    /// What a playout does to reach `next` from this list.
    #[must_use]
    pub fn diff(&self, next: &Self) -> OutputChanges {
        let mut changes = OutputChanges {
            order: next
                .outputs
                .iter()
                .map(|output| output.id.clone())
                .collect(),
            ..OutputChanges::default()
        };
        for held in &self.outputs {
            if next.get(&held.id).is_none() {
                changes.removed.push(held.id.clone());
            }
        }
        for output in &next.outputs {
            match self.get(&output.id) {
                None => changes.started.push(output.clone()),
                Some(held) if held != output => changes.updated.push(output.clone()),
                Some(_) => {}
            }
        }
        changes
    }
}

/// What a host does to play a playback's outputs. The test host records calls; the real one opens
/// sound-device streams and projector sessions.
pub trait OutputHost {
    /// Start playing on `output`. An output already playing is [`OutputHost::update`]d, not
    /// started again.
    ///
    /// # Errors
    /// Why it could not start (a device gone, a receiver refusing).
    fn start(&mut self, output: &Output) -> Result<(), String>;
    /// Change a playing output's volume, sync or reported state in place.
    ///
    /// # Errors
    /// Why it could not be changed.
    fn update(&mut self, output: &Output) -> Result<(), String>;
    /// Stop playing on `id`.
    fn stop(&mut self, id: &str);
}

/// One playback's outputs, applied to the host that plays them.
pub struct Playout<H: OutputHost> {
    host: H,
    current: PlaybackOutputs,
}

impl<H: OutputHost> Playout<H> {
    #[must_use]
    pub fn new(host: H) -> Self {
        Self {
            host,
            current: PlaybackOutputs::default(),
        }
    }

    #[must_use]
    pub fn current(&self) -> &PlaybackOutputs {
        &self.current
    }

    #[must_use]
    pub fn host(&self) -> &H {
        &self.host
    }

    /// The host, for the sound the playback hands it and for the changes it reports that nobody
    /// asked for.
    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    /// Move the host to `next`. Stopped outputs go first, then started ones, then in-place
    /// changes, so a device freed by one output can take the next.
    ///
    /// # Errors
    /// The outputs that would not start or change, each said with why. The ones that did are
    /// applied and kept.
    pub fn apply(&mut self, next: &PlaybackOutputs) -> Result<(), Vec<String>> {
        let changes = self.current.diff(next);
        let mut failures = Vec::new();
        for id in &changes.removed {
            self.host.stop(id);
        }
        for output in &changes.started {
            if let Err(why) = self.host.start(output) {
                failures.push(format!("{}: {why}", output.id));
            }
        }
        for output in &changes.updated {
            if let Err(why) = self.host.update(output) {
                failures.push(format!("{}: {why}", output.id));
            }
        }
        self.current = next.clone();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures)
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "a test compares the exact fraction it set"
)]
mod tests {
    use super::*;

    fn sink(id: &str, device: &str) -> Output {
        Output {
            id: id.to_owned(),
            kind: OutputKind::Sink {
                device: device.to_owned(),
            },
            label: device.to_owned(),
            volume: 1.0,
            sync_ms: 0,
            state: OutputState::Playing,
        }
    }

    fn cast(id: &str, target: &str) -> Output {
        Output {
            id: id.to_owned(),
            kind: OutputKind::Cast {
                protocol: "Google Cast".to_owned(),
                target: target.to_owned(),
            },
            label: target.to_owned(),
            volume: 1.0,
            sync_ms: 0,
            state: OutputState::Connecting,
        }
    }

    #[test]
    fn adding_appends_and_a_repeated_id_replaces_in_place() {
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in"));
        outputs.add(cast("tv", "Living room"));
        assert_eq!(
            outputs
                .outputs()
                .iter()
                .map(|o| o.id.as_str())
                .collect::<Vec<_>>(),
            ["speaker", "tv"]
        );
        outputs.add(sink("speaker", "Desk"));
        assert_eq!(outputs.outputs().len(), 2);
        assert_eq!(outputs.get("speaker").unwrap().label, "Desk");
        assert_eq!(outputs.get("speaker").unwrap().state, OutputState::Playing);
    }

    #[test]
    fn moving_replaces_the_position_and_removing_takes_the_output_out() {
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in"));
        outputs.add(cast("tv", "Living room"));
        outputs.add(sink("desk", "Desk"));
        let tv = outputs.get("tv").unwrap().clone();
        outputs.move_to(tv, 0);
        assert_eq!(
            outputs
                .outputs()
                .iter()
                .map(|o| o.id.as_str())
                .collect::<Vec<_>>(),
            ["tv", "speaker", "desk"]
        );
        let desk = outputs.get("desk").unwrap().clone();
        outputs.move_to(desk, 99);
        assert_eq!(
            outputs
                .outputs()
                .iter()
                .map(|o| o.id.as_str())
                .collect::<Vec<_>>(),
            ["tv", "speaker", "desk"]
        );
        assert!(outputs.remove("speaker").is_some());
        assert!(outputs.remove("speaker").is_none());
        assert_eq!(outputs.outputs().len(), 2);
    }

    #[test]
    fn a_volume_is_a_fraction_and_the_sync_is_whole_milliseconds() {
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in"));
        outputs.set_volume("speaker", 2.0);
        assert_eq!(outputs.get("speaker").unwrap().volume, 1.0);
        outputs.set_volume("speaker", -1.0);
        assert_eq!(outputs.get("speaker").unwrap().volume, 0.0);
        outputs.set_sync("speaker", -120);
        assert_eq!(outputs.get("speaker").unwrap().sync_ms, -120);
    }

    #[test]
    fn a_diff_starts_what_began_and_updates_what_moved() {
        let mut before = PlaybackOutputs::default();
        before.add(sink("speaker", "Built-in"));
        before.add(cast("tv", "Living room"));
        let mut after = before.clone();
        after.remove("speaker");
        after.add(sink("desk", "Desk"));
        after.set_volume("tv", 0.5);
        let changes = before.diff(&after);
        assert_eq!(changes.removed, ["speaker"]);
        assert_eq!(
            changes
                .started
                .iter()
                .map(|o| o.id.as_str())
                .collect::<Vec<_>>(),
            ["desk"]
        );
        assert_eq!(
            changes
                .updated
                .iter()
                .map(|o| o.id.as_str())
                .collect::<Vec<_>>(),
            ["tv"]
        );
        assert_eq!(changes.order, ["tv", "desk"]);
    }

    /// A host that records what it was asked, and refuses what it is told to.
    #[derive(Default)]
    struct Recorder {
        calls: Vec<String>,
        refuse: Option<String>,
    }

    impl OutputHost for Recorder {
        fn start(&mut self, output: &Output) -> Result<(), String> {
            self.calls.push(format!("start {}", output.id));
            match &self.refuse {
                Some(id) if id == &output.id => Err("gone".to_owned()),
                _ => Ok(()),
            }
        }

        fn update(&mut self, output: &Output) -> Result<(), String> {
            self.calls
                .push(format!("update {} {:.2}", output.id, output.volume));
            Ok(())
        }

        fn stop(&mut self, id: &str) {
            self.calls.push(format!("stop {id}"));
        }
    }

    #[test]
    fn the_playout_starts_stops_and_retunes_the_list_it_is_given() {
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in"));
        let mut playout = Playout::new(Recorder::default());
        playout.apply(&outputs).expect("the first output starts");
        assert_eq!(playout.host().calls, ["start speaker"]);
        let mut next = outputs.clone();
        next.add(cast("tv", "Living room"));
        next.set_volume("speaker", 0.4);
        playout
            .apply(&next)
            .expect("the second output starts and the first turns down");
        assert_eq!(
            playout.host().calls,
            ["start speaker", "start tv", "update speaker 0.40"]
        );
        let mut fewer = next.clone();
        fewer.remove("tv");
        playout.apply(&fewer).expect("the television goes");
        assert_eq!(
            playout.host().calls.last().map(String::as_str),
            Some("stop tv")
        );
    }

    #[test]
    fn an_output_that_will_not_start_is_said_and_the_rest_plays() {
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in"));
        outputs.add(cast("tv", "Living room"));
        let mut playout = Playout::new(Recorder {
            refuse: Some("tv".to_owned()),
            ..Recorder::default()
        });
        let failed = playout.apply(&outputs).expect_err("the television refuses");
        assert_eq!(failed, ["tv: gone"]);
        assert_eq!(playout.host().calls, ["start speaker", "start tv"]);
    }
}

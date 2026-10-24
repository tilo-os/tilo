//! The project's verbs, and the editor as the app's surface.
//!
//! Fields change through the field edits every path offers
//! (`Project::paths().clips.item(&id).opacity.set(0.5)`); the editor's own
//! gestures edit its local copy of the project and submit the edits between
//! that copy and where it started ([`ui::tilo_resource::Path::edits`]). The
//! methods below are the operations that are more than setting a field —
//! splitting, trimming, rippling, linking, re-timing — and run the same
//! editing math the editor does, so an agent and a person make the same cut.

use ui::framework::{ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{self, Id, Invoke, Path, Refusal, resource};
use ui::widget::Element;
use uuid::Uuid;

use crate::app::{EMPTY_CAPTURE, NARROW_CAPTURE, ShotScenario};
use crate::model::{
    ClipId, EffectKind, MAX_GAIN_DB, MIN_GAIN_DB, MediaItem, MediaKind, Project, TextSource, Track,
    TrackKind, key,
};
use crate::param::Param;

/// The clip, track, source or marker an id names.
fn uuid(id: &Id) -> Result<Uuid, Refusal> {
    id.as_str()
        .parse()
        .map_err(|_| Refusal::new(format!("`{id}` names nothing in this project")))
}

/// A time on the timeline an edit can happen at.
fn instant(at: f64) -> Result<f64, Refusal> {
    if at.is_finite() && at >= 0.0 {
        Ok(at)
    } else {
        Err(Refusal::new("a time at or after the start"))
    }
}

#[resource(
    kind = "video-editor.project",
    label = "Project",
    params = video_project,
    search = crate::search::project_entries,
    read_content = crate::agent_timeline::read_timeline,
    facts = crate::facts::facts,
    answer = crate::agent_timeline::answer,
)]
impl Project {
    /// Set the frame size.
    #[action(id = "set-frame-size")]
    pub fn resize_frame(
        &mut self,
        #[param("Frame width in pixels; clamped to 16..8192 and rounded even")] width: u32,
        #[param("Frame height in pixels; clamped to 16..8192 and rounded even")] height: u32,
    ) {
        self.set_frame_size(width, height);
    }

    /// Set the frame rate.
    #[action(id = "set-frame-rate")]
    pub fn retime_frames(&mut self, #[param("Frames per second; clamped to 1..240")] fps: f64) {
        self.set_fps(fps);
    }

    /// Set the render range.
    #[action(id = "set-render-range")]
    pub fn render_range(
        &mut self,
        #[param("Seconds where a render begins")] start: f64,
        #[param("Seconds where it ends; later than the start")] end: f64,
    ) -> Result<(), Refusal> {
        if !(start.is_finite() && end.is_finite() && end > start) {
            return Err(Refusal::new("a render range ends after it begins"));
        }
        // Out first: marking in drops an out mark at or before it.
        self.set_mark_out(end);
        self.set_mark_in(start);
        Ok(())
    }

    /// Mark where a render begins.
    #[action]
    pub fn mark_in(&mut self, #[param("Seconds on the timeline")] at: f64) -> Result<(), Refusal> {
        self.set_mark_in(instant(at)?);
        Ok(())
    }

    /// Mark where a render ends.
    #[action]
    pub fn mark_out(&mut self, #[param("Seconds on the timeline")] at: f64) -> Result<(), Refusal> {
        self.set_mark_out(instant(at)?);
        Ok(())
    }

    /// Clear the in and out marks.
    #[action(id = "clear-marks")]
    pub fn clear_render_range(&mut self) {
        self.clear_marks();
    }

    /// Add a title.
    ///
    /// Answers the new source's id: place it with add-clip.
    #[action(answers)]
    pub fn add_title(
        &mut self,
        #[param("The words the title shows; empty reads as “Title”")] text: String,
        #[param("The words' colour, #rrggbb or a colour word; white when omitted")] colour: Option<
            String,
        >,
        #[param("Letter height as a fraction of the frame height, 0.02 to 0.5; 0.12 when omitted")]
        size: Option<f64>,
        #[param("left, center or right; center when omitted")] align: Option<String>,
    ) -> Result<(), Refusal> {
        let words = text.trim();
        let content = if words.is_empty() { "Title" } else { words };
        let mut source = TextSource::new(content);
        crate::agent_timeline::TitleStyle {
            text: None,
            colour,
            size,
            align,
        }
        .apply(&mut source)?;
        let palette = u8::try_from(self.media.len() % 7).unwrap_or(0);
        self.media.push(MediaItem {
            id: tilo_resource::fresh_uuid(),
            name: content.lines().next().unwrap_or("Title").to_owned(),
            kind: MediaKind::Text,
            // Long enough to be trimmed down rather than stretched.
            duration: 5.0,
            palette,
            // A title has no bytes: nothing to hydrate or fetch.
            path: String::new(),
            text: Some(source),
            ..MediaItem::default()
        });
        Ok(())
    }

    /// Restyle a title.
    #[action(answers, item = media)]
    pub fn set_title(
        &mut self,
        media: &Id,
        #[param("New words; the bin name follows their first line")] text: Option<String>,
        #[param("The words' colour, #rrggbb or a colour word")] colour: Option<String>,
        #[param("Letter height as a fraction of the frame height, 0.02 to 0.5")] size: Option<f64>,
        #[param("left, center or right")] align: Option<String>,
    ) -> Result<(), Refusal> {
        self.agent_set_title(
            media,
            &crate::agent_timeline::TitleStyle {
                text,
                colour,
                size,
                align,
            },
        )
    }

    /// Add a clip.
    ///
    /// Places a bin source, by id or name, on a track: by default the first
    /// track of its kind, after the clips already there, its whole length.
    /// A track named V1, A2 and so on that the project lacks is made.
    /// Answers the new clip's id.
    #[action(id = "add-clip", answers)]
    pub fn add_named_clip(
        &mut self,
        #[param(flatten)] placing: crate::agent_timeline::ClipPlacement,
    ) -> Result<(), Refusal> {
        self.agent_add_clip(&placing).map(drop)
    }

    /// Move a clip, and the clips linked to it, to a time.
    #[action(id = "move-clip", answers, item = clips)]
    pub fn shift_clip(
        &mut self,
        clip: &Id,
        #[param("Seconds on the timeline where it starts")] at: f64,
        #[param("Another track of its kind, by id or name; its own when omitted")] track: Option<
            String,
        >,
    ) -> Result<(), Refusal> {
        let id = self.editable_link_group(clip)?;
        self.agent_move_clip(id, at, track.as_deref())
    }

    /// Set a clip's fades, opacity, volume and transition.
    #[action(answers, item = clips)]
    pub fn set_clip(
        &mut self,
        clip: &Id,
        #[param("Seconds of fade from black (and silence) at its head")] fade_in: Option<f64>,
        #[param("Seconds of fade to black (and silence) at its tail")] fade_out: Option<f64>,
        #[param("Opacity from 0 to 1")] opacity: Option<f64>,
        #[param("Volume from 0 to 2; 1 is as recorded")] volume: Option<f64>,
        #[param("Transition into it: none, dissolve, fade, dip or wipe")] transition: Option<
            String,
        >,
        #[param("Seconds the transition lasts")] transition_duration: Option<f64>,
    ) -> Result<(), Refusal> {
        let id = self.editable(clip)?;
        let settings = crate::agent_timeline::ClipSettings {
            fade_in,
            fade_out,
            opacity,
            volume,
            transition,
            transition_duration,
        };
        self.agent_set_clip(id, &settings)
    }

    /// Remove a clip, and the clips linked to it.
    #[action(id = "remove-clip", answers, item = clips, destructive)]
    pub fn remove_clip_closing(
        &mut self,
        clip: &Id,
        #[param("Close the gap it leaves by moving later clips on its track back")] ripple: Option<
            bool,
        >,
    ) -> Result<(), Refusal> {
        let id = self.editable_link_group(clip)?;
        if ripple.unwrap_or(false) {
            self.ripple_delete_linked(id);
        } else {
            self.delete_linked(id);
        }
        Ok(())
    }

    /// Add a timeline marker.
    #[action(id = "add-marker")]
    pub fn place_marker(
        &mut self,
        #[param("Seconds from the start")] at: f64,
        #[param("Marker name; empty reads as “Marker”")] name: String,
    ) -> Result<(), Refusal> {
        let at = instant(at)?;
        let name = name.trim();
        self.add_marker(at, if name.is_empty() { "Marker" } else { name }, 1);
        Ok(())
    }

    /// Remove a timeline marker.
    #[action(id = "remove-marker", item = markers, destructive)]
    pub fn delete_marker(&mut self, marker: &Id) -> Result<(), Refusal> {
        if self.remove_marker(uuid(marker)?) {
            Ok(())
        } else {
            Err(Refusal::new("no such marker"))
        }
    }

    /// Add an empty track.
    #[action]
    pub fn add_track(&mut self, #[param("Video or audio")] kind: TrackKind) {
        self.new_lane(kind);
    }

    /// Put a track's gain and pan back to unity and centre.
    #[action(item = tracks)]
    pub fn reset_mix(&mut self, track: &Id) -> Result<(), Refusal> {
        let id = uuid(track)?;
        let track = self.tracks.find_mut(&key(id)).ok_or("no such track")?;
        track.gain_db = 0.0;
        track.pan = 0.0;
        Ok(())
    }

    /// Bring every lane named down by the same amount, keeping the balance.
    #[action]
    pub fn trim_lanes(
        &mut self,
        #[param("The tracks to bring down")] tracks: Vec<Id>,
        #[param("Decibels to bring each down by")] db: f64,
        #[param(
            "Decibels to leave below full scale besides; 0 lands the measured peak exactly at \
             full scale"
        )]
        headroom: f64,
    ) -> Result<(), Refusal> {
        if !(db.is_finite() && headroom.is_finite()) {
            return Err(Refusal::new("decibels are finite numbers"));
        }
        let lanes = tracks.iter().map(uuid).collect::<Result<Vec<_>, _>>()?;
        // Negative headroom would leave the mix clipping, which is what this
        // exists to end.
        let trimmed = self.lanes_trimmed_by(&lanes, db + headroom.max(0.0));
        if trimmed.is_empty() {
            return Err(Refusal::new("nothing to trim"));
        }
        for next in trimmed {
            if let Some(track) = self.tracks.find_mut(&key(next.id)) {
                *track = next;
            }
        }
        Ok(())
    }

    /// Delete a track and its clips.
    #[action(item = tracks, destructive)]
    pub fn delete_track(&mut self, track: &Id) -> Result<(), Refusal> {
        let id = uuid(track)?;
        if self.tracks.take(track).is_none() {
            return Err(Refusal::new("no such track"));
        }
        self.clips.retain(|clip| clip.track != id);
        Ok(())
    }

    /// Place a source on a track.
    #[action(item = media)]
    pub fn place_clip(
        &mut self,
        media: &Id,
        #[param("The track to place it on")] track: Id,
        #[param("Seconds from the start")] start: f64,
    ) -> Result<(), Refusal> {
        let track = uuid(&track)?;
        if self.track(track).is_none() {
            return Err(Refusal::new("no such track"));
        }
        self.add_clip(track, uuid(media)?, instant(start)?)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such source"))
    }

    /// Delete a source and every clip of it.
    #[action(id = "delete-media", item = media, destructive)]
    pub fn remove_media(&mut self, media: &Id) -> Result<(), Refusal> {
        let id = uuid(media)?;
        if self.media.take(media).is_none() {
            return Err(Refusal::new("no such source"));
        }
        self.clips.retain(|clip| clip.media != id);
        Ok(())
    }

    /// Use a source's frame size and rate for the project.
    #[action(id = "use-as-project-format", item = media)]
    pub fn adopt_media_format(&mut self, media: &Id) -> Result<(), Refusal> {
        let source = self.media.find(media).cloned().ok_or("no such source")?;
        if source.width == 0 || source.height == 0 {
            return Err(Refusal::new("the source has no picture size"));
        }
        self.set_frame_size(source.width, source.height);
        if source.fps > 0.0 {
            self.set_fps(source.fps);
        }
        Ok(())
    }

    /// Delete a clip and the clips linked to it.
    #[action(id = "delete", item = clips, destructive)]
    pub fn remove_clip(&mut self, clip: &Id) -> Result<(), Refusal> {
        let id = self.editable_link_group(clip)?;
        self.delete_linked(id);
        Ok(())
    }

    /// Delete a clip and close the gap it leaves.
    #[action(id = "ripple-delete", item = clips, destructive)]
    pub fn close_gap(&mut self, clip: &Id) -> Result<(), Refusal> {
        let id = self.editable_link_group(clip)?;
        self.ripple_delete_linked(id);
        Ok(())
    }

    /// Cut a clip and its linked clips in two.
    #[action(id = "split-clip", item = clips)]
    pub fn cut(
        &mut self,
        clip: &Id,
        #[param("Seconds on the timeline where it is cut")] at: f64,
    ) -> Result<(), Refusal> {
        let id = self.editable_link_group(clip)?;
        if self.split_linked(id, instant(at)?).is_empty() {
            return Err(Refusal::new("the clip does not cover that time"));
        }
        Ok(())
    }

    /// Move a clip's start, and its linked clips', to a time.
    #[action(id = "trim-in-clip", item = clips)]
    pub fn trim_head(
        &mut self,
        clip: &Id,
        #[param("Seconds on the timeline the clip starts at")] at: f64,
    ) -> Result<(), Refusal> {
        let id = self.editable_link_group(clip)?;
        let at = instant(at)?;
        let clip = self.covering(id, at)?;
        let delta = (at - clip.start) * clip.speed.rate();
        self.trim_in_linked(id, delta);
        Ok(())
    }

    /// Move a clip's end, and its linked clips', to a time.
    #[action(id = "trim-out-clip", item = clips)]
    pub fn trim_tail(
        &mut self,
        clip: &Id,
        #[param("Seconds on the timeline the clip ends at")] at: f64,
    ) -> Result<(), Refusal> {
        let id = self.editable_link_group(clip)?;
        let at = instant(at)?;
        let clip = self.covering(id, at)?;
        let delta = (at - clip.end()) * clip.speed.rate();
        self.trim_out_linked(id, delta);
        Ok(())
    }

    /// Set a clip's playback speed.
    #[action(id = "set-speed", item = clips)]
    pub fn retime(
        &mut self,
        clip: &Id,
        #[param("Playback rate: 1 is normal, 0.5 half speed, 2 double")] rate: f64,
    ) -> Result<(), Refusal> {
        let id = self.editable_link_group(clip)?;
        if !rate.is_finite() || rate <= 0.0 {
            return Err(Refusal::new("a playback rate is a positive number"));
        }
        let reverse = self.clip(id).is_some_and(|c| c.speed.reverse);
        self.set_speed_linked(id, rate, reverse);
        Ok(())
    }

    /// Play a clip backwards, or forwards again.
    #[action(id = "toggle-reverse", item = clips)]
    pub fn reverse(&mut self, clip: &Id) -> Result<(), Refusal> {
        let id = self.editable_link_group(clip)?;
        let (rate, reverse) = self
            .clip(id)
            .map_or((1.0, false), |c| (c.speed.rate(), c.speed.reverse));
        self.set_speed_linked(id, rate, !reverse);
        Ok(())
    }

    /// Play a clip at normal speed.
    #[action(id = "reset-speed", item = clips)]
    pub fn normal_speed(&mut self, clip: &Id) -> Result<(), Refusal> {
        let id = self.editable_link_group(clip)?;
        self.set_speed_linked(id, 1.0, false);
        Ok(())
    }

    /// Set one of a clip's animatable values where it plays at a time.
    #[action(item = clips)]
    pub fn set_param(
        &mut self,
        clip: &Id,
        #[param("What to set")] param: Param,
        #[param("Seconds on the timeline; a keyframed value is keyed there")] at: f64,
        #[param("The value")] value: f64,
    ) -> Result<(), Refusal> {
        let id = self.editable(clip)?;
        let at = instant(at)?;
        if !value.is_finite() {
            return Err(Refusal::new("a value is a finite number"));
        }
        let clip = self.clip_mut(id).ok_or("no such clip")?;
        clip.set_param_at(param, at, value);
        Ok(())
    }

    /// Add an effect to a clip.
    #[action(item = clips)]
    pub fn add_effect(
        &mut self,
        clip: &Id,
        #[param("The effect")] effect: EffectKind,
    ) -> Result<(), Refusal> {
        let id = self.editable(clip)?;
        self.clip_mut(id).ok_or("no such clip")?.add_effect(effect);
        Ok(())
    }

    /// Remove an effect from a clip.
    #[action(item = clips, destructive)]
    pub fn remove_effect(
        &mut self,
        clip: &Id,
        #[param("The effect's id")] effect: Id,
    ) -> Result<(), Refusal> {
        let id = self.editable(clip)?;
        let effect = uuid(&effect)?;
        if self
            .clip_mut(id)
            .ok_or("no such clip")?
            .remove_effect_id(effect)
        {
            Ok(())
        } else {
            Err(Refusal::new("no such effect on the clip"))
        }
    }

    /// Move an effect up or down a clip's stack.
    #[action(item = clips)]
    pub fn move_effect(
        &mut self,
        clip: &Id,
        #[param("The effect's id")] effect: Id,
        #[param("Towards the top of the stack")] up: bool,
    ) -> Result<(), Refusal> {
        let id = self.editable(clip)?;
        let effect = uuid(&effect)?;
        if self
            .clip_mut(id)
            .ok_or("no such clip")?
            .move_effect_id(effect, up)
        {
            Ok(())
        } else {
            Err(Refusal::new("the effect cannot move that way"))
        }
    }

    /// Set how strongly one of a clip's effects applies.
    #[action(item = clips)]
    pub fn set_effect_amount(
        &mut self,
        clip: &Id,
        #[param("The effect's id")] effect: Id,
        #[param("Strength from 0 to 1")] amount: f64,
    ) -> Result<(), Refusal> {
        if !(0.0..=1.0).contains(&amount) {
            return Err(Refusal::new("an effect's strength is between 0 and 1"));
        }
        let id = self.editable(clip)?;
        let effect = uuid(&effect)?;
        let target = self
            .clip_mut(id)
            .ok_or("no such clip")?
            .effects
            .find_mut(&key(effect))
            .ok_or("no such effect on the clip")?;
        target.amount = amount;
        Ok(())
    }

    /// Remove every effect from a clip.
    #[action(item = clips, destructive)]
    pub fn clear_effects(&mut self, clip: &Id) -> Result<(), Refusal> {
        let id = self.editable(clip)?;
        self.clip_mut(id).ok_or("no such clip")?.clear_effects();
        Ok(())
    }

    /// Stop a clip's linked clips following its edits.
    #[action(item = clips)]
    pub fn unlink(&mut self, clip: &Id) -> Result<(), Refusal> {
        let id = self.editable(clip)?;
        let group = self.link_group(id);
        if self.unlink_clips(&group).is_empty() {
            return Err(Refusal::new("the clip is not linked"));
        }
        Ok(())
    }

    /// Split a clip's sound off onto an audio clip linked to it.
    #[action(id = "link-audio", item = clips)]
    pub fn split_audio(&mut self, clip: &Id) -> Result<(), Refusal> {
        let id = self.editable(clip)?;
        if !self.can_split_audio(id) {
            return Err(Refusal::new("the clip has no sound to split off"));
        }
        let audio_track = if let Some(track) = self.placement_track(TrackKind::Audio, None) {
            track
        } else {
            let track = Track {
                id: tilo_resource::fresh_uuid(),
                name: "A1".to_owned(),
                kind: TrackKind::Audio,
                index: 1,
                ..Track::default()
            };
            let id = track.id;
            self.tracks.push(track);
            id
        };
        self.add_linked_audio(id, audio_track)
            .map(drop)
            .ok_or_else(|| Refusal::new("the clip has no sound to split off"))
    }

    /// Link a clip with another, so they move, trim and re-time together.
    #[action(item = clips)]
    pub fn link_with(
        &mut self,
        clip: &Id,
        #[param("The clip to link it with")] other: Id,
    ) -> Result<(), Refusal> {
        let id = self.editable(clip)?;
        let other = uuid(&other)?;
        if self.clip(other).is_none() || other == id {
            return Err(Refusal::new("name another clip of this project"));
        }
        let mut members = self.link_group(id);
        for member in self.link_group(other) {
            if !members.contains(&member) {
                members.push(member);
            }
        }
        self.link_clips(&members)
            .map(drop)
            .ok_or_else(|| Refusal::new("those clips cannot be linked"))
    }
}

impl Project {
    /// The clip `id` names, when it may be edited.
    fn editable(&self, clip: &Id) -> Result<ClipId, Refusal> {
        let id = uuid(clip)?;
        if self.clip(id).is_none() {
            return Err(Refusal::new("no such clip in this project"));
        }
        if !self.clip_editable(id) {
            return Err(Refusal::new("the clip's track is locked"));
        }
        Ok(id)
    }

    /// Linked edits must not bypass a partner's track lock or leave the
    /// picture and sound out of sync by editing only the unlocked half.
    fn editable_link_group(&self, clip: &Id) -> Result<ClipId, Refusal> {
        let id = self.editable(clip)?;
        if self
            .link_group(id)
            .iter()
            .any(|member| !self.clip_editable(*member))
        {
            return Err(Refusal::new("a linked clip's track is locked"));
        }
        Ok(id)
    }

    /// The clip `id`, when it covers `at`.
    fn covering(&self, id: ClipId, at: f64) -> Result<&crate::model::Clip, Refusal> {
        self.clip(id)
            .filter(|clip| clip.covers(at))
            .ok_or_else(|| Refusal::new("the clip does not cover that time"))
    }
}

/// A track's level stays between off and four times unity, its pan inside
/// the stereo field.
impl Track {
    pub(crate) fn valid(&self) -> Result<(), Refusal> {
        if !(MIN_GAIN_DB..=MAX_GAIN_DB).contains(&self.gain_db) {
            return Err(Refusal::new(format!(
                "a track's gain is between {MIN_GAIN_DB} and {MAX_GAIN_DB} dB"
            )));
        }
        if !(-1.0..=1.0).contains(&self.pan) {
            return Err(Refusal::new("a track's pan is between -1 and 1"));
        }
        Ok(())
    }
}

/// The edits that replace the whole project with `project`: what opening a
/// project file or staging a capture does.
#[must_use]
pub fn load_project(project: &Project) -> ui::tilo_resource::Edit<Project> {
    Path::<Project, Project>::root().set(project.clone())
}

/// The effect that reveals what `item` names in `project` — a marker,
/// track, clip or source, or an effect or marker on a clip
/// (`<clip>/effects/<effect>`, `<clip>/markers/<marker>`, see
/// [`crate::search`]) — refusing one that no longer exists.
///
/// # Errors
/// Returns that the item is gone.
pub fn reveal_effect(
    project: &Project,
    item: &Id,
) -> Result<crate::actions::VideoEditorEffect, Refusal> {
    use crate::actions::VideoEditorEffect;
    let gone = || Refusal::new("that item is no longer in the project");
    let mut parts = item.as_str().split('/');
    let id = parts
        .next()
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(gone)?;
    match (parts.next(), parts.next()) {
        (None, _) => {}
        (Some("effects"), Some(effect)) => {
            let clip = project.clip(id).ok_or_else(gone)?;
            let effect = Uuid::parse_str(effect).map_err(|_| gone())?;
            clip.effects
                .iter()
                .find(|e| e.id == effect)
                .ok_or_else(gone)?;
            return Ok(VideoEditorEffect::RevealEffect {
                clip: id.to_string(),
                effect: effect.to_string(),
            });
        }
        (Some("markers"), Some(marker)) => {
            let clip = project.clip(id).ok_or_else(gone)?;
            let marker = Uuid::parse_str(marker).map_err(|_| gone())?;
            clip.markers
                .iter()
                .find(|m| m.id == marker)
                .ok_or_else(gone)?;
            return Ok(VideoEditorEffect::RevealMarker {
                marker: marker.to_string(),
                clip: Some(id.to_string()),
            });
        }
        _ => return Err(gone()),
    }
    let id_text = id.to_string();
    if project.clip(id).is_some() {
        Ok(VideoEditorEffect::RevealClip { clip: id_text })
    } else if project.track(id).is_some() {
        Ok(VideoEditorEffect::RevealTrack { track: id_text })
    } else if project.media.iter().any(|m| m.id == id) {
        Ok(VideoEditorEffect::RevealMedia { media: id_text })
    } else if project.markers.iter().any(|m| m.id == id) {
        Ok(VideoEditorEffect::RevealMarker {
            marker: id_text,
            clip: None,
        })
    } else {
        Err(gone())
    }
}

ui::embedded_catalog!(static CATALOG, ["en-US"]);

/// The editor as a framework app: one surface, the whole window.
pub struct VideoEditor;

impl ResourceApp for VideoEditor {
    type Resource = Project;
    const ID: &'static str = "so.tilo.video-editor";
    // Each window shows the document it opened, never another window's.
    const WINDOW_RESOURCE: bool = true;
    const TITLE: &'static str = "Video Editor";
    const ICON: Option<&'static str> = Some("video-editor");
    /// A media file the editor is launched on (`video-editor CLIP`) is
    /// imported into the project and placed on the timeline, as a pick from
    /// the import chooser is (see the surface's `attach`).
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Project>) -> Element {
        // A search hit or an agent's `open` reveals the item it names.
        let dispatcher = cx.dispatcher();
        let live = cx.live().clone();
        cx.on_open_item(move |item| {
            let effect = reveal_effect(&live.get(), item)?;
            dispatcher.effect(&effect);
            Ok(())
        });
        cx.surface(crate::VideoEditorApp::new())
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn scenarios() -> &'static [&'static str] {
        static SCENARIOS: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
            ShotScenario::ALL_NAMES
                .iter()
                .copied()
                .chain([EMPTY_CAPTURE, NARROW_CAPTURE])
                .collect()
        });
        SCENARIOS.as_slice()
    }

    /// The narrow capture is below the bin's collapse breakpoint, so it
    /// shows the overlay-flap layout.
    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario == Some(NARROW_CAPTURE) {
            (720, 820)
        } else {
            (1280, 800)
        }
    }

    /// The demo project's sources resolve to the stand-ins bundled with the
    /// crate, so a capture reads local bytes without a workspace mount.
    fn fixture(_fixture: &mut ui::framework::Fixture<Project>) {
        let bundled = crate::media::bundled_sample_media();
        crate::media::serve_sample_media(bundled.exists().then_some(bundled));
    }

    /// Every capture but the blank one opens a project through the same
    /// edit opening a project file makes: the bundled demo project, or the
    /// composed one; the editor stages the rest (see
    /// `VideoEditorApp::stage_capture`).
    fn scenario(name: Option<&str>) -> Vec<Invoke<Project>> {
        let project = match name {
            Some(EMPTY_CAPTURE) => return Vec::new(),
            Some("composed") => crate::app::composed_project(),
            _ => crate::seed::sample_project(),
        };
        Invoke::edit(&load_project(&project)).map_or_else(
            |error| {
                tracing::warn!(%error, "the scenario's project does not encode");
                Vec::new()
            },
            |invoke| vec![invoke],
        )
    }

    fn label(project: &Project) -> String {
        if project.name.is_empty() {
            "Project".to_owned()
        } else {
            project.name.clone()
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// The walk's slices: a drive of one scenario walks dozens of actions,
    /// so each slice is its own test and they run side by side.
    const SLICES: usize = 4;

    /// Every action the app publishes is reachable by address in a drive of
    /// every `SLICES`th scenario from `slice`: none is advertised yet
    /// handled only by a click. The scenarios stage fixtures; the app
    /// someone opens acts on this machine, so it is not walked.
    fn walk(slice: usize) {
        for scenario in super::VideoEditor::scenarios()
            .iter()
            .skip(slice)
            .step_by(SLICES)
        {
            let mut app = TestApp::<super::VideoEditor>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_0() {
        walk(0);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_1() {
        walk(1);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_2() {
        walk(2);
    }

    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action_3() {
        walk(3);
    }
}

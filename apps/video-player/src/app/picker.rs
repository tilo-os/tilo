//! The app's side of the file chooser: the request it remembers, the generation that tells an old answer from the current one, the first frame it waits for, and what a failure says.

use super::*;

impl PlayerApp {
    /// Kick off a file-picker open, answering `reply` once a picture of the
    /// chosen video decodes. A second request is refused rather than
    /// replacing the first, which would make its answer race.
    pub(super) fn trigger_open(&mut self, reply: Option<EffectReply>) -> Result<(), String> {
        if self.portal_open.is_some() {
            return Err("a video picker is already open".into());
        }
        let generation = self.portal_generation.checked_add(1).ok_or_else(|| {
            "video-picker request generation exhausted; restart Video Player".to_string()
        })?;
        self.portal_generation = generation;
        self.open_error = None;
        self.missing_codec = None;
        self.portal_open = Some(PendingPortalOpen {
            channel: if tilo_paths::availability(tilo_paths::Feature::NativePaths)
                == tilo_paths::Availability::Available
            {
                PickerChannel::Path(video_open_request().send())
            } else {
                PickerChannel::Contents(video_open_request().send_contents())
            },
            generation,
            reply,
        });
        self.dirty = true;
        Ok(())
    }

    /// Drop a picker receiver that no longer represents the user's intent.
    /// The picker may still answer, but nothing receives it.
    pub(super) fn cancel_pending_portal(&mut self, message: &str) {
        let Some(pending) = self.portal_open.take() else {
            return;
        };
        if let Some(reply) = pending.reply {
            reply.finish(Err(message.into()));
        }
    }

    /// Answer `reply` once the open video's first picture decodes, or with
    /// why it did not.
    pub(super) fn await_open_frame(&mut self, reply: EffectReply) {
        if let Some(previous) = self.media_open.take() {
            previous
                .reply
                .finish(Err("a newer video open replaced this one".into()));
        }
        let Some(source_id) = self.current_source_id.clone() else {
            reply.finish(Err("the video did not open".into()));
            return;
        };
        self.media_open = Some(PendingMediaOpen {
            reply,
            source_id,
            deadline: ui::time::now() + OPEN_FRAME_TIMEOUT,
        });
    }

    /// Answer the pending open: a picture of its video decoded, another
    /// video replaced it, or the decoder ran out of time.
    pub(super) fn poll_open_frame(&mut self) {
        let Some(pending) = self.media_open.as_ref() else {
            return;
        };
        let outcome = if self.current_source_id.as_deref() != Some(pending.source_id.as_str()) {
            Some(Err(
                "The requested video was superseded before decoding a frame.".to_string(),
            ))
        } else if self.player.latest_frame().is_some() {
            self.record_current_history();
            Some(Ok(()))
        } else if ui::time::now() >= pending.deadline {
            let message = "The decoder did not produce a first frame in time.".to_string();
            self.open_error = Some(message.clone());
            self.dirty = true;
            Some(Err(message))
        } else {
            None
        };
        if let Some(outcome) = outcome
            && let Some(pending) = self.media_open.take()
        {
            pending
                .reply
                .finish(outcome.map(|()| ui::tilo_actions::ActionOutput::default()));
        }
    }

    /// Fail the pending open with the decoder's `message`.
    pub(super) fn fail_open(&mut self, message: String) {
        if let Some(pending) = self.media_open.take() {
            pending.reply.finish(Err(message));
        }
    }

    /// Answer `effect`, invoked by a control, the palette, an agent or a
    /// script. Opening answers once a picture decodes.
    pub(crate) fn answer_effect(
        &mut self,
        effect: VideoEffect,
        cx: &mut EffectCx<PlayerResource, Self>,
    ) -> EffectAnswer {
        if effect.requires_media() && !self.player.has_media() {
            return Err("open a video before using this playback control".into());
        }
        match effect {
            VideoEffect::OpenVideo { path: None } => {
                self.trigger_open(Some(cx.reply()))?;
                Ok(Effected::Pending)
            }
            VideoEffect::OpenVideo { path: Some(path) } => {
                self.open_named(&path)?;
                self.await_open_frame(cx.reply());
                Ok(Effected::Pending)
            }
            VideoEffect::PlayQueued { item } => {
                self.open_local_entry(&item)?;
                self.await_open_frame(cx.reply());
                Ok(Effected::Pending)
            }
            effect => self.perform(effect).map(|()| Effected::done()),
        }
    }

    /// Open the video `path` names on this device: a file, or a stream's URL.
    pub(super) fn open_named(&mut self, path: &str) -> Result<(), String> {
        self.missing_codec = None;
        let source = source_for_argument(path);
        if let media::Source::LocalFile(file) | media::Source::Workspace(file) = &source
            && !file.is_file()
        {
            let message = format!("Video does not exist: {}", source_title(&source));
            self.open_error = Some(message.clone());
            self.dirty = true;
            return Err(message);
        }
        let title = source_title(&source);
        let queue_id = self.ensure_queued(source.clone(), title.clone());
        self.open_local_source(source, title, Some(queue_id))
    }
}

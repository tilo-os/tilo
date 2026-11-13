//! The network worker behind the window: starting a bounded request without stalling input, folding
//! a result into the model when one lands, and settling a page when the worker is saturated, stops,
//! or finds no source at all.

use std::collections::HashSet;

use crate::api::{Request, Update, Worker};
use crate::i18n::t;
use crate::model::Route;

use super::Tube;

impl Tube {
    pub(super) fn request(&mut self, req: Request) -> bool {
        if self.worker.is_none() {
            self.worker = Some(Worker::spawn(self.wake.clone()));
        }
        if let Some(w) = &self.worker
            && let Err(error) = w.request(req)
        {
            let (rejected, stopped) = match error {
                std::sync::mpsc::TrySendError::Full(request) => (request, false),
                std::sync::mpsc::TrySendError::Disconnected(request) => (request, true),
            };
            self.request_rejected(&rejected, stopped);
            return false;
        }
        true
    }

    pub(super) fn request_rejected(&mut self, request: &Request, stopped: bool) {
        const BUSY: &str = "Too many requests are pending. Please try again.";
        /// A stopped worker is Tube's own failure, not the network's: the device may well be
        /// online.
        const STOPPED: &str = "Tube's network worker stopped. Reopen Tube to load videos.";
        let message = if stopped { STOPPED } else { BUSY };
        if stopped {
            self.model.data_source = crate::model::DataSource {
                live: false,
                label: "worker stopped".into(),
            };
        }
        match request {
            Request::Search { query } => {
                if self.model.search_query == *query {
                    self.model.search_loading = false;
                    self.model.search_error = Some(message.to_string());
                }
            }
            Request::Feed => {
                self.model.feed_loading = false;
                self.model.feed_error = Some(message.to_string());
            }
            Request::Branding(_)
            | Request::VideoMeta(_)
            | Request::Channel(_)
            | Request::PlaylistPage(_)
            | Request::PlayVideo { .. }
            | Request::Captions { .. } => self.model.push_notice(message),
        }
        self.structure_dirty = true;
    }

    fn source_unreached(&mut self, why: &crate::api::Unreached) {
        use crate::api::Unreached;
        self.model.feed_loading = false;
        self.model.feed_error = None;
        let (label, search_error) = if *why == Unreached::Offline {
            self.model.sources_unreached = None;
            ("offline", t("tube-offline-title"))
        } else {
            self.model.sources_unreached = Some(why.clone());
            ("sources unreachable", t("tube-unreached-title"))
        };
        self.model.data_source = crate::model::DataSource {
            live: false,
            label: label.into(),
        };
        if self.model.search_loading {
            self.model.search_loading = false;
            self.model.search_error = Some(search_error);
        }
    }

    pub(super) fn request_live(&mut self, req: Request) {
        if self.online {
            self.request(req);
        }
    }

    pub(super) fn request_channel_if_stub(&mut self, channel_id: &str) {
        if channel_id.is_empty() || self.requested_channels.contains(channel_id) {
            return;
        }
        let stub = self
            .model
            .channel(channel_id)
            .is_none_or(|c| c.avatar_url.is_empty() || c.subscribers == 0);
        if stub {
            self.requested_channels.insert(channel_id.to_string());
            self.request_live(Request::Channel(channel_id.to_string()));
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per worker Update variant; the length is the number of \
                  update kinds, not accidental complexity in any one arm"
    )]
    pub(super) fn pump(&mut self) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        let updates = worker.poll();
        if updates.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut fetch_channels: Vec<String> = Vec::new();
        for u in updates {
            match u {
                Update::Sponsor(id, segs) => {
                    let shown = watching(&self.model.route, &id);
                    if let Some(v) = self.model.videos.iter_mut().find(|v| v.id == id) {
                        v.sponsor = segs;
                        changed |= shown;
                    }
                }
                Update::DeArrow {
                    video,
                    title,
                    thumbnail_url,
                } => {
                    if let Some(v) = self.model.videos.iter_mut().find(|v| v.id == video) {
                        if title.is_some() {
                            v.dearrow_title = title;
                        }
                        v.dearrow_thumb = thumbnail_url.is_some();
                        changed = true;
                    }
                    if let Some(url) = thumbnail_url
                        && !url.is_empty()
                    {
                        self.model.dearrow_thumb_url.insert(video, url);
                        changed = true;
                    }
                }
                Update::Dislikes {
                    video,
                    count,
                    likes,
                } => {
                    changed |= watching(&self.model.route, &video);
                    if let Some(likes) = likes
                        && let Some(v) = self.model.videos.iter_mut().find(|v| v.id == video)
                        && v.likes == 0
                    {
                        v.likes = likes;
                    }
                    self.model.dislikes.insert(video, count);
                }
                Update::Comments { video, comments } => {
                    let shown = watching(&self.model.route, &video);
                    if let Some(v) = self.model.videos.iter_mut().find(|v| v.id == video) {
                        v.comments = comments;
                        changed |= shown;
                    }
                }
                Update::SearchResults {
                    query,
                    videos,
                    playlists,
                    channels,
                } => {
                    self.model.search_loading = false;
                    self.model.search_error = None;
                    let ids: Vec<String> = videos.iter().map(|v| v.id.clone()).collect();
                    for nv in videos {
                        self.model
                            .ensure_channel_stub(&nv.channel_id, &nv.channel_name);
                        self.model.adopt_channel_avatar(
                            &nv.channel_id,
                            &nv.channel_name,
                            &nv.channel_avatar_url,
                        );
                        self.model.upsert_video(nv);
                    }
                    let playlist_ids: Vec<String> =
                        playlists.iter().map(|p| p.id.clone()).collect();
                    for p in playlists {
                        self.model
                            .ensure_channel_stub(&p.channel_id, &p.channel_name);
                        self.model.upsert_playlist(p);
                    }
                    let channel_ids: Vec<String> = channels.iter().map(|c| c.id.clone()).collect();
                    for c in channels {
                        self.model
                            .adopt_channel_avatar(&c.id, &c.name, &c.avatar_url);
                        if let Some(ex) = self.model.channels.iter_mut().find(|x| x.id == c.id) {
                            if ex.avatar_url.is_empty() {
                                ex.avatar_url = c.avatar_url;
                            }
                            if ex.subscribers == 0 {
                                ex.subscribers = c.subscribers;
                            }
                            if ex.handle == ex.id && !c.handle.is_empty() {
                                ex.handle = c.handle;
                            }
                            ex.verified = ex.verified || c.verified;
                        } else {
                            self.model.channels.push(c);
                        }
                    }
                    self.model.reconcile_channel_avatars();
                    query.clone_into(&mut self.model.search_hits_query);
                    self.model.search_hits = ids;
                    self.model.search_playlist_hits = playlist_ids;
                    self.model.search_channel_hits = channel_ids;
                    if matches!(&self.model.route, Route::Search(_)) {
                        self.model.route = Route::Search(query);
                    }
                    let mut seen = HashSet::new();
                    let stub_cids: Vec<String> = self
                        .model
                        .search_hits
                        .iter()
                        .filter_map(|id| self.model.video(id))
                        .map(|v| v.channel_id.clone())
                        .filter(|cid| {
                            !cid.is_empty()
                                && seen.insert(cid.clone())
                                && self
                                    .model
                                    .channel(cid)
                                    .is_none_or(|c| c.avatar_url.is_empty())
                        })
                        .take(6)
                        .collect();
                    for cid in stub_cids {
                        fetch_channels.push(cid);
                    }
                    changed = true;
                }
                Update::PlaylistPage { playlist, videos } => {
                    for v in videos {
                        self.model
                            .ensure_channel_stub(&v.channel_id, &v.channel_name);
                        self.model
                            .note_channel_avatar(&v.channel_id, &v.channel_avatar_url);
                        self.model.upsert_video(v);
                    }
                    self.model
                        .ensure_channel_stub(&playlist.channel_id, &playlist.channel_name);
                    self.model.upsert_playlist(*playlist);
                    changed = true;
                }
                Update::Feed(vids) => {
                    self.model.adopt_live_catalogue(vids, "live (rustypipe)");
                    self.model.feed_loading = false;
                    self.model.feed_error = None;
                    self.model.sources_unreached = None;
                    changed = true;
                }
                Update::VideoMeta { video, related } => {
                    let id = video.id.clone();
                    let channel_id = video.channel_id.clone();
                    self.model
                        .ensure_channel_stub(&video.channel_id, &video.channel_name);
                    self.model.adopt_channel_avatar(
                        &video.channel_id,
                        &video.channel_name,
                        &video.channel_avatar_url,
                    );
                    self.model.upsert_video(*video);
                    if !channel_id.is_empty()
                        && self
                            .model
                            .channel(&channel_id)
                            .is_none_or(|c| c.avatar_url.is_empty() || c.subscribers == 0)
                    {
                        fetch_channels.push(channel_id);
                    }
                    let ids: Vec<String> = related.iter().map(|v| v.id.clone()).collect();
                    for rv in related {
                        self.model
                            .ensure_channel_stub(&rv.channel_id, &rv.channel_name);
                        self.model.adopt_channel_avatar(
                            &rv.channel_id,
                            &rv.channel_name,
                            &rv.channel_avatar_url,
                        );
                        self.model.upsert_video(rv);
                    }
                    self.model.reconcile_channel_avatars();
                    if !ids.is_empty() {
                        self.model.related_of = id;
                        self.model.related_hits = ids;
                    }
                    changed = true;
                }
                Update::ChannelPage { channel, uploads } => {
                    self.model.adopt_channel_avatar(
                        &channel.id,
                        &channel.name,
                        &channel.avatar_url,
                    );
                    self.model.upsert_channel(*channel);
                    for uv in uploads {
                        self.model
                            .ensure_channel_stub(&uv.channel_id, &uv.channel_name);
                        self.model.adopt_channel_avatar(
                            &uv.channel_id,
                            &uv.channel_name,
                            &uv.channel_avatar_url,
                        );
                        self.model.upsert_video(uv);
                    }
                    self.model.reconcile_channel_avatars();
                    changed = true;
                }
                Update::Status { live, label } => {
                    if self.model.data_source.live != live || self.model.data_source.label != label
                    {
                        self.model.data_source = crate::model::DataSource { live, label };
                        changed = true;
                    }
                }
                Update::StreamResolved {
                    video,
                    url,
                    has_audio,
                    quality,
                } => {
                    if self.load_resolved_stream(video, url, has_audio, &quality) {
                        changed = true;
                    }
                }
                Update::Captions {
                    video,
                    tracks,
                    language,
                    lines,
                    failure,
                } => {
                    if watching(&self.model.route, &video) {
                        self.model.captions_of = video;
                        self.model.caption_tracks = tracks;
                        match (language, lines, failure) {
                            (Some(_), Some(lines), _) => {
                                self.model.captions = lines;
                                self.model.captions_notice = None;
                            }
                            (Some(_), None, why) => {
                                self.model.captions.clear();
                                self.model.captions_notice =
                                    Some(why.unwrap_or_else(|| t("tube-captions-unavailable")));
                            }
                            (None, _, _) => {
                                self.model.captions.clear();
                                self.model.captions_notice = None;
                                self.load_chosen_captions();
                            }
                        }
                        changed = true;
                    }
                }
                Update::Unplayable { video, said } => {
                    if matches!(&self.model.route, Route::Watch(id) if *id == video) {
                        self.model.playing = false;
                        self.model.unplayable = Some(crate::model::Unplayable { video, said });
                        changed = true;
                    }
                }
                Update::Unreached(why) => {
                    self.source_unreached(&why);
                    changed = true;
                }
                Update::Suggestions { query, videos } => {
                    self.take_suggestions(query, videos);
                }
                Update::Note(n) => {
                    tracing::debug!(note = %n, "tube worker note");
                }
            }
        }
        for cid in fetch_channels {
            self.request_channel_if_stub(&cid);
        }
        changed
    }
}

fn watching(route: &Route, video: &str) -> bool {
    matches!(route, Route::Watch(open) if open == video)
}

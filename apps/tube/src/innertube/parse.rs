//! Reading `InnerTube` answers into Tube's model.
//!
//! The answers are deep trees of named renderers whose nesting changes as
//! `YouTube` redesigns its pages, while the rows themselves (`videoRenderer`,
//! `channelRenderer`, `lockupViewModel`) change far less. So rows are found
//! by name wherever they sit, and a row missing a field gives a shorter row
//! rather than none.

use serde_json::Value;

use crate::backend::yt_thumb_url;
use crate::model::{Channel, Video};

/// A search answer's rows.
#[derive(Debug, Default)]
pub struct Found {
    pub videos: Vec<Video>,
    pub channels: Vec<Channel>,
}

/// A stream the media element plays from one URL: picture and sound in
/// one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub url: String,
    /// `YouTube`'s quality label, e.g. "360p".
    pub quality: String,
}

/// The visitor id an answer hands out, which later requests carry.
pub fn visitor(answer: &Value) -> Option<String> {
    answer
        .pointer("/responseContext/visitorData")
        .and_then(Value::as_str)
        .filter(|visitor| !visitor.is_empty())
        .map(str::to_string)
}

/// The videos and creators a search answered, in its order, each once.
pub fn search(answer: &Value, category: &str) -> Found {
    let contents = answer.get("contents").unwrap_or(answer);
    let mut found = Found::default();
    let mut seen = std::collections::HashSet::new();
    for row in rows(contents, &["videoRenderer", "channelRenderer"]) {
        if let Some(video) = row
            .get("videoRenderer")
            .and_then(|r| video_row(r, category))
        {
            if seen.insert(video.id.clone()) {
                found.videos.push(video);
            }
        } else if let Some(channel) = row.get("channelRenderer").and_then(channel_row)
            && seen.insert(channel.id.clone())
        {
            found.channels.push(channel);
        }
    }
    found
}

/// A watch page: the video, from the watch-next answer and, when there is
/// one, the player answer (which alone knows the length and the full
/// description), and the videos it recommends next.
pub fn watch(next: &Value, player: Option<&Value>, id: &str) -> Option<(Video, Vec<Video>)> {
    let primary = first(next, "videoPrimaryInfoRenderer");
    let secondary = first(next, "videoSecondaryInfoRenderer");
    let owner = secondary.and_then(|s| first(s, "videoOwnerRenderer"));
    let details = player.and_then(|p| p.get("videoDetails"));
    let detail = |key: &str| {
        details
            .and_then(|d| d.get(key))
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    let title = match primary.map(|p| text(p.get("title"))) {
        Some(title) if !title.is_empty() => title,
        _ => detail("title").to_string(),
    };
    if title.is_empty() {
        return None;
    }
    let (channel_name, channel_id) = owner
        .map(|o| byline(o.get("title")))
        .filter(|(name, _)| !name.is_empty())
        .unwrap_or_else(|| {
            (
                detail("author").to_string(),
                detail("channelId").to_string(),
            )
        });
    let views = primary
        .and_then(|p| p.pointer("/viewCount/videoViewCountRenderer/viewCount"))
        .map(|v| count(&text(Some(v))))
        .filter(|views| *views > 0)
        .unwrap_or_else(|| detail("viewCount").parse().unwrap_or(0));
    let live = details
        .and_then(|d| d.get("isLive"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let description = match detail("shortDescription") {
        "" => secondary
            .and_then(|s| s.pointer("/attributedDescription/content"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        full => full.to_string(),
    };
    let video = Video {
        id: id.to_string(),
        title,
        dearrow_title: None,
        channel_seed: channel_id.clone(),
        channel_avatar_url: owner
            .and_then(|o| o.pointer("/thumbnail/thumbnails"))
            .map(last_url)
            .unwrap_or_default(),
        channel_id,
        channel_name,
        views,
        published: primary
            .map(|p| text(p.get("relativeDateText")))
            .unwrap_or_default(),
        duration: detail("lengthSeconds").parse().unwrap_or(0),
        description,
        likes: likes(next),
        seed: id.to_string(),
        thumbnail_url: yt_thumb_url(id),
        category: "Video".into(),
        dearrow_thumb: false,
        sponsor: Vec::new(),
        comments: Vec::new(),
        live,
    };
    let related = next
        .pointer("/contents/twoColumnWatchNextResults/secondaryResults")
        .map(|recommended| {
            rows(recommended, &["lockupViewModel", "compactVideoRenderer"])
                .into_iter()
                .filter_map(|row| {
                    row.get("lockupViewModel")
                        .and_then(|r| lockup_row(r, "Related"))
                        .or_else(|| {
                            row.get("compactVideoRenderer")
                                .and_then(|r| video_row(r, "Related"))
                        })
                })
                .filter(|related| related.id != id)
                .collect()
        })
        .unwrap_or_default();
    Some((video, related))
}

/// `YouTube`'s reason for not playing, when the player answer refuses.
#[must_use]
pub fn refusal(player: &Value) -> Option<String> {
    let status = player
        .pointer("/playabilityStatus/status")
        .and_then(Value::as_str)?;
    if status == "OK" {
        return None;
    }
    Some(
        player
            .pointer("/playabilityStatus/reason")
            .and_then(Value::as_str)
            .map_or_else(|| status.to_owned(), str::to_owned),
    )
}

/// The progressive stream a player answer names, best first; or why the
/// answer refused (a bot check, a removed or private video).
///
/// `wanted` is the height a person asked for (`Some(480)` for 480p, `None`
/// for the best available): the best stream that does not exceed it, so
/// choosing a smaller size actually spends less data, and the smallest one
/// when the source has nothing that small.
pub fn stream(player: &Value, wanted: Option<u32>) -> Result<Stream, String> {
    let status = player
        .pointer("/playabilityStatus/status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status != "OK" {
        let reason = player
            .pointer("/playabilityStatus/reason")
            .and_then(Value::as_str)
            .unwrap_or("not playable");
        return Err(format!("{status}: {reason}"));
    }
    let mut formats: Vec<(u64, String, String)> = player
        .pointer("/streamingData/formats")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|format| {
            format
                .get("mimeType")
                .and_then(Value::as_str)
                .is_some_and(|mime| mime.starts_with("video/mp4"))
        })
        .filter_map(|format| {
            let url = format.get("url").and_then(Value::as_str)?;
            let height = format.get("height").and_then(Value::as_u64).unwrap_or(0);
            let quality = format
                .get("qualityLabel")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let rank = height.max(quality.trim_end_matches('p').parse().unwrap_or(0));
            Some((rank, url.to_string(), quality.to_string()))
        })
        .collect();
    formats.sort_by_key(|(rank, ..)| *rank);
    let under = wanted.and_then(|wanted| {
        let wanted = u64::from(wanted);
        formats.iter().rev().find(|(rank, ..)| *rank <= wanted)
    });
    // A source with nothing that small offers its smallest, and a request for
    // the best is the largest.
    let chosen = under
        .or_else(|| wanted.and_then(|_| formats.first()))
        .or_else(|| formats.last());
    chosen
        .map(|(_, url, quality)| Stream {
            url: url.clone(),
            quality: quality.clone(),
        })
        .ok_or_else(|| "no progressive stream".into())
}

/// The caption tracks a player answer lists: `captionTracks` under
/// `playerCaptionsTracklistRenderer`. A machine transcript (`kind="asr"`) is
/// marked so the menu can say so. An answer with no captions offers none.
pub fn caption_tracks(player: &Value) -> Vec<crate::captions::CaptionTrack> {
    player
        .pointer("/captions/playerCaptionsTracklistRenderer/captionTracks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|track| {
            let url = track
                .get("baseUrl")
                .or_else(|| track.get("url"))?
                .as_str()?;
            let language = track
                .get("languageCode")
                .and_then(Value::as_str)?
                .to_string();
            Some(crate::captions::CaptionTrack {
                label: track
                    .get("name")
                    .and_then(|name| {
                        name.get("simpleText")
                            .and_then(Value::as_str)
                            .or_else(|| name.get("runs")?.get(0)?.get("text")?.as_str())
                    })
                    .unwrap_or(&language)
                    .to_string(),
                automatic: track
                    .get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind == "asr"),
                // The timed text is served as WebVTT; asking for it explicitly
                // is what makes one parser serve both sources.
                url: format!("{url}&fmt=vtt"),
                language,
            })
        })
        .collect()
}

/// A channel page and its uploads, from a channel's videos tab.
pub fn channel(answer: &Value, id: &str) -> Option<(Channel, Vec<Video>)> {
    let meta = answer.pointer("/metadata/channelMetadataRenderer")?;
    let name = meta.get("title").and_then(Value::as_str)?.to_string();
    let header = first(answer, "pageHeaderViewModel");
    let parts: Vec<String> = header
        .map(|h| {
            rows(h, &["metadataParts"])
                .into_iter()
                .filter_map(|row| row.get("metadataParts")?.as_array().cloned())
                .flatten()
                .map(|part| text(part.get("text")))
                .collect()
        })
        .unwrap_or_default();
    let channel = Channel {
        id: id.to_string(),
        name: name.clone(),
        handle: parts
            .iter()
            .find(|part| part.starts_with('@'))
            .cloned()
            .unwrap_or_default(),
        subscribers: parts
            .iter()
            .find(|part| part.contains("subscriber"))
            .map_or(0, |part| count(part)),
        verified: false,
        description: meta
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        seed: id.to_string(),
        avatar_url: meta
            .pointer("/avatar/thumbnails")
            .map(last_url)
            .unwrap_or_default(),
        banner_url: header
            .and_then(|h| h.pointer("/banner/imageBannerViewModel/image/sources"))
            .map(last_url)
            .unwrap_or_default(),
    };
    let uploads = answer
        .get("contents")
        .map(|contents| {
            rows(contents, &["lockupViewModel", "videoRenderer"])
                .into_iter()
                .filter_map(|row| {
                    row.get("lockupViewModel")
                        .and_then(|r| lockup_row(r, "Uploads"))
                        .or_else(|| {
                            row.get("videoRenderer")
                                .and_then(|r| video_row(r, "Uploads"))
                        })
                })
                .map(|mut upload| {
                    if upload.channel_id.is_empty() {
                        upload.channel_id = id.to_string();
                        upload.channel_seed = id.to_string();
                    }
                    if upload.channel_name.is_empty() {
                        upload.channel_name.clone_from(&name);
                    }
                    if upload.channel_avatar_url.is_empty() {
                        upload.channel_avatar_url.clone_from(&channel.avatar_url);
                    }
                    upload
                })
                .collect()
        })
        .unwrap_or_default();
    Some((channel, uploads))
}

/// A search or list row (`videoRenderer`, `compactVideoRenderer`).
fn video_row(row: &Value, category: &str) -> Option<Video> {
    let id = row.get("videoId").and_then(Value::as_str)?;
    let (channel_name, channel_id) = ["longBylineText", "ownerText", "shortBylineText"]
        .iter()
        .map(|key| byline(row.get(*key)))
        .find(|(name, _)| !name.is_empty())
        .unwrap_or_default();
    let reach = text(row.get("viewCountText"));
    let live = reach.contains("watching")
        || row
            .get("badges")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|badge| {
                badge
                    .pointer("/metadataBadgeRenderer/style")
                    .and_then(Value::as_str)
                    == Some("BADGE_STYLE_TYPE_LIVE_NOW")
            });
    Some(Video {
        id: id.to_string(),
        title: text(row.get("title")),
        dearrow_title: None,
        channel_seed: channel_id.clone(),
        channel_id,
        channel_name,
        channel_avatar_url: row
            .pointer("/channelThumbnailSupportedRenderers/channelThumbnailWithLinkRenderer/thumbnail/thumbnails")
            .map(last_url)
            .unwrap_or_default(),
        views: count(&reach),
        published: text(row.get("publishedTimeText")),
        duration: clock(&text(row.get("lengthText"))),
        description: row
            .pointer("/detailedMetadataSnippets/0/snippetText")
            .or_else(|| row.get("descriptionSnippet"))
            .map(|snippet| text(Some(snippet)))
            .unwrap_or_default(),
        likes: 0,
        seed: id.to_string(),
        thumbnail_url: yt_thumb_url(id),
        category: category.to_string(),
        dearrow_thumb: false,
        sponsor: Vec::new(),
        comments: Vec::new(),
        live,
    })
}

/// A creator row in search.
fn channel_row(row: &Value) -> Option<Channel> {
    let id = row.get("channelId").and_then(Value::as_str)?;
    // The row names the handle where the subscriber count used to be, and
    // the subscriber count where the video count used to be.
    let lines = [
        text(row.get("subscriberCountText")),
        text(row.get("videoCountText")),
    ];
    Some(Channel {
        id: id.to_string(),
        name: text(row.get("title")),
        handle: lines
            .iter()
            .find(|line| line.starts_with('@'))
            .cloned()
            .unwrap_or_default(),
        subscribers: lines
            .iter()
            .find(|line| line.contains("subscriber"))
            .map_or(0, |line| count(line)),
        verified: row
            .get("ownerBadges")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|badge| {
                badge
                    .pointer("/metadataBadgeRenderer/style")
                    .and_then(Value::as_str)
                    .is_some_and(|style| style.starts_with("BADGE_STYLE_TYPE_VERIFIED"))
            }),
        description: text(row.get("descriptionSnippet")),
        seed: id.to_string(),
        avatar_url: row
            .pointer("/thumbnail/thumbnails")
            .map(last_url)
            .unwrap_or_default(),
        banner_url: String::new(),
    })
}

/// A row in `YouTube`'s newer layout (watch-next, channel tabs): a title and
/// lines of short parts, "Lofi Girl" / "1.2M views · 3 years ago".
fn lockup_row(row: &Value, category: &str) -> Option<Video> {
    if row.get("contentType").and_then(Value::as_str) != Some("LOCKUP_CONTENT_TYPE_VIDEO") {
        return None;
    }
    let id = row.get("contentId").and_then(Value::as_str)?;
    let meta = row.pointer("/metadata/lockupMetadataViewModel")?;
    let lines: Vec<Vec<String>> = meta
        .pointer("/metadata/contentMetadataViewModel/metadataRows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|line| {
            line.get("metadataParts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|part| text(part.get("text")))
                .filter(|part| !part.is_empty())
                .collect()
        })
        .collect();
    let mut video = Video {
        id: id.to_string(),
        title: text(meta.get("title")),
        dearrow_title: None,
        channel_id: String::new(),
        channel_name: String::new(),
        channel_seed: String::new(),
        channel_avatar_url: meta
            .pointer("/image/decoratedAvatarViewModel/avatar/avatarViewModel/image/sources")
            .map(last_url)
            .unwrap_or_default(),
        views: 0,
        published: String::new(),
        duration: 0,
        description: String::new(),
        likes: 0,
        seed: id.to_string(),
        thumbnail_url: yt_thumb_url(id),
        category: category.to_string(),
        dearrow_thumb: false,
        sponsor: Vec::new(),
        comments: Vec::new(),
        live: false,
    };
    for (index, part) in lines
        .iter()
        .enumerate()
        .flat_map(|(index, line)| line.iter().map(move |part| (index, part)))
    {
        if part.contains("watching") {
            video.live = true;
            video.views = count(part);
        } else if part.ends_with(" ago") || part.starts_with("Streamed ") {
            video.published.clone_from(part);
        } else if part.contains("view") || looks_like_count(part) {
            video.views = count(part);
        } else if index == 0 && video.channel_name.is_empty() && lines.len() > 1 {
            video.channel_name.clone_from(part);
        }
    }
    video.channel_id = first(meta, "browseEndpoint")
        .and_then(|endpoint| endpoint.get("browseId"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    video.channel_seed.clone_from(&video.channel_id);
    for badge in rows(
        row.get("contentImage").unwrap_or(&Value::Null),
        &["thumbnailBadgeViewModel"],
    ) {
        let badge = &badge["thumbnailBadgeViewModel"];
        let label = badge
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if badge.get("badgeStyle").and_then(Value::as_str)
            == Some("THUMBNAIL_OVERLAY_BADGE_STYLE_LIVE")
        {
            video.live = true;
        } else if video.duration == 0 {
            video.duration = clock(label);
        }
    }
    Some(video)
}

/// The like count the watch-next answer shows ("499K"), as a number.
fn likes(next: &Value) -> u64 {
    rows(next, &["likeCountEntity"])
        .into_iter()
        .filter_map(|entity| {
            entity["likeCountEntity"]
                .get("likeCountIfIndifferentNumber")
                .and_then(Value::as_str)
        })
        .map(count)
        .find(|likes| *likes > 0)
        .unwrap_or(0)
}

/// Every object under `tree` that has one of `keys`, in document order,
/// without looking inside a match.
fn rows<'a>(tree: &'a Value, keys: &[&str]) -> Vec<&'a Value> {
    fn walk<'a>(value: &'a Value, keys: &[&str], out: &mut Vec<&'a Value>) {
        match value {
            Value::Object(map) => {
                if keys.iter().any(|key| map.contains_key(*key)) {
                    out.push(value);
                    return;
                }
                for child in map.values() {
                    walk(child, keys, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, keys, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(tree, keys, &mut out);
    out
}

/// The first `key` object under `tree`.
fn first<'a>(tree: &'a Value, key: &str) -> Option<&'a Value> {
    rows(tree, &[key]).first().and_then(|row| row.get(key))
}

/// The text of a text node in any of its forms: `simpleText`, `runs` or
/// `content`.
fn text(node: Option<&Value>) -> String {
    let Some(node) = node else {
        return String::new();
    };
    if let Some(text) = node.as_str() {
        return text.to_string();
    }
    if let Some(text) = node
        .get("simpleText")
        .or_else(|| node.get("content"))
        .and_then(Value::as_str)
    {
        return text.to_string();
    }
    node.get("runs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|run| run.get("text").and_then(Value::as_str))
        .collect()
}

/// A byline's creator name and channel id.
fn byline(node: Option<&Value>) -> (String, String) {
    let run = node.and_then(|n| n.pointer("/runs/0"));
    let id = run
        .and_then(|r| r.pointer("/navigationEndpoint/browseEndpoint/browseId"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    (text(node), id.to_string())
}

/// The last (largest) image URL of a thumbnail list, made absolute.
fn last_url(images: &Value) -> String {
    let url = images
        .as_array()
        .and_then(|images| images.last())
        .and_then(|image| image.get("url"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    match url.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    }
}

/// Whether `part` is a bare count, "824K" or "1,204".
fn looks_like_count(part: &str) -> bool {
    let digits = part.trim_end_matches(['K', 'M', 'B']);
    !digits.is_empty()
        && digits
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
}

/// The number at the start of a count: "58,224,382 views", "1.2M views",
/// "20K watching", "15.8M subscribers". 0 when there is none ("No views").
fn count(text: &str) -> u64 {
    let word = text.split_whitespace().next().unwrap_or_default();
    let scaled = [('K', 1e3), ('M', 1e6), ('B', 1e9)]
        .into_iter()
        .find_map(|(suffix, scale)| word.strip_suffix(suffix).map(|number| (number, scale)));
    if let Some((number, scale)) = scaled {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a shown count is a small positive number"
        )]
        return number
            .parse::<f64>()
            .map_or(0, |n| (n * scale).round() as u64);
    }
    word.chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

/// Seconds in a clock length, "1:04:42" or "3:41"; 0 for anything else.
fn clock(text: &str) -> u32 {
    if text.is_empty() || !text.chars().all(|c| c.is_ascii_digit() || c == ':') {
        return 0;
    }
    text.split(':')
        .try_fold(0_u32, |total, part| {
            total.checked_mul(60)?.checked_add(part.parse().ok()?)
        })
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

//! Captions: the tracks a video offers and the words on screen while it plays.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptionTrack {
    pub language: String,
    pub label: String,
    pub url: String,
    #[serde(default)]
    pub automatic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptionCue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[must_use]
pub fn active_cue(cues: &[CaptionCue], position_ms: u64) -> Option<&CaptionCue> {
    let index = cues.partition_point(|cue| cue.start_ms <= position_ms);
    let cue = cues.get(index.checked_sub(1)?)?;
    (position_ms < cue.end_ms).then_some(cue)
}

#[must_use]
pub fn parse_vtt(text: &str) -> Vec<CaptionCue> {
    let mut cues: Vec<CaptionCue> = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let Some((start_ms, end_ms)) = timing(line) else {
            continue;
        };
        let mut words: Vec<String> = Vec::new();
        while let Some(next) = lines.peek() {
            let next = next.trim();
            if next.is_empty() || next.contains("-->") {
                break;
            }
            words.push(strip_markup(next));
            lines.next();
        }
        let content = words.join("\n");
        let content = content.trim();
        if content.is_empty() || end_ms <= start_ms {
            continue;
        }
        cues.push(CaptionCue {
            start_ms,
            end_ms,
            text: content.to_string(),
        });
    }
    cues.sort_by_key(|cue| (cue.start_ms, cue.end_ms));
    cues
}

fn timing(line: &str) -> Option<(u64, u64)> {
    let line = line.trim();
    let (start, rest) = line.split_once("-->")?;
    let end = rest.split_whitespace().next()?;
    Some((timestamp(start)?, timestamp(end)?))
}

fn timestamp(text: &str) -> Option<u64> {
    let text = text.trim().trim_end_matches(',');
    let mut parts = text.split(':').rev();
    let seconds = parts.next()?.parse::<f64>().ok()?;
    let minutes = parts.next()?.parse::<f64>().ok()?;
    let hours = parts
        .next()
        .map_or(Some(0.0), |hours| hours.parse::<f64>().ok())?;
    if !(0.0..3600.0).contains(&minutes) || !(0.0..60.0).contains(&seconds) {
        return None;
    }
    let total = hours * 3600.0 + minutes * 60.0 + seconds;
    if total.is_sign_negative() || !total.is_finite() {
        return None;
    }
    Some((total * 1000.0).round() as u64)
}

fn strip_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for character in text.chars() {
        match character {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            _ => out.push(character),
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "WEBVTT\n\
        Kind: captions\n\
        Language: en\n\n\
        STYLE\n\
        ::cue { color: lime }\n\n\
        NOTE a note that looks like text\n\n\
        1\n\
        00:00:01.000 --> 00:00:04.500 align:start position:10%\n\
        <v Host>Hello, and welcome\n\
        to the show.\n\n\
        2\n\
        00:00:04.500 --> 00:00:07.000\n\
        <i>Sponsor</i> break &amp; more\n\n\
        00:00:09.000 --> 00:00:11.000\n\
        Third cue, out of order\n";

    #[test]
    fn webvtt_reads_the_cues_without_its_blocks_or_its_markup() {
        let cues = parse_vtt(SAMPLE);
        assert_eq!(cues.len(), 3);
        assert_eq!(cues[0].start_ms, 1_000);
        assert_eq!(cues[0].end_ms, 4_500);
        assert_eq!(cues[0].text, "Hello, and welcome\nto the show.");
        assert_eq!(cues[1].text, "Sponsor break & more");
        assert_eq!(cues[2].start_ms, 9_000);
    }

    #[test]
    fn the_playhead_finds_the_line_it_is_inside() {
        let cues = parse_vtt(SAMPLE);
        assert_eq!(active_cue(&cues, 0).map(|cue| cue.text.as_str()), None);
        assert_eq!(
            active_cue(&cues, 2_000).map(|cue| cue.start_ms),
            Some(1_000)
        );
        assert_eq!(
            active_cue(&cues, 4_500).map(|cue| cue.start_ms),
            Some(4_500),
            "a cue's end is the next one's start"
        );
        assert_eq!(
            active_cue(&cues, 7_000).map(|cue| cue.text.as_str()),
            None,
            "nothing is said between cues"
        );
        assert_eq!(
            active_cue(&cues, 10_000).map(|cue| cue.start_ms),
            Some(9_000)
        );
    }

    #[test]
    fn short_and_broken_timings_are_read_or_left_out() {
        let cues = parse_vtt(
            "WEBVTT\n\n01:02.500 --> 01:05.000\nShort form\n\n\
             not a timing line\n\n\
             00:00:09.000 --> 00:00:09.000\nempty\n\n\
             00:00:99.000 --> 00:01:01.000\nimpossible seconds\n",
        );
        assert_eq!(cues.len(), 1);
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (62_500, 65_000));
        assert_eq!(cues[0].text, "Short form");
    }
}

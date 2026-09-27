//! Item ids ⇄ URLs, and the HTTP date format.
//!
//! A `WebDAV` item's id is its decoded path relative to the account root:
//! `""` is the root, `"Docs/a b.txt"` a file. Hrefs from the server are
//! resolved against the root and accepted only when they stay on its origin
//! and under its path, and when no segment decodes to something that would
//! change the path's meaning (`/`, `.`, `..`, controls).

use std::time::{Duration, SystemTime};

use cloud_source::ItemId;
use tilo_dav::Url;

/// The URL of `id` below `root` (which ends in `/`). Folders get a
/// trailing slash.
pub fn url_for(root: &Url, id: &ItemId, folder: bool) -> Option<Url> {
    let mut url = root.clone();
    if !id.as_str().is_empty() {
        let mut segments = url.path_segments_mut().ok()?;
        segments.pop_if_empty();
        for segment in id.as_str().split('/') {
            if !valid_segment(segment) {
                return None;
            }
            segments.push(segment);
        }
        if folder {
            segments.push("");
        }
    }
    Some(url)
}

/// The id of a server href, or `None` when it is outside the root or
/// unsafe.
pub fn id_for_href(root: &Url, href: &str) -> Option<ItemId> {
    let url = root.join(href).ok()?;
    if url.scheme() != root.scheme()
        || url.host_str() != root.host_str()
        || url.port_or_known_default() != root.port_or_known_default()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let root_path = root.path().trim_end_matches('/');
    let rest = url.path().strip_prefix(root_path)?;
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    let mut parts = Vec::new();
    for raw in rest.split('/').filter(|part| !part.is_empty()) {
        let decoded = percent_decode(raw)?;
        if !valid_segment(&decoded) {
            return None;
        }
        parts.push(decoded);
    }
    Some(ItemId::new(parts.join("/")))
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.contains('/')
        && !segment.chars().any(char::is_control)
}

/// The parent id of a non-root id.
pub fn parent_of(id: &ItemId) -> ItemId {
    match id.as_str().rsplit_once('/') {
        Some((parent, _)) => ItemId::new(parent),
        None => ItemId::new(""),
    }
}

/// The last segment of an id.
pub fn name_of(id: &ItemId) -> &str {
    id.as_str()
        .rsplit_once('/')
        .map_or(id.as_str(), |(_, name)| name)
}

/// The id of `name` inside `parent`, if `name` is a valid segment.
pub fn child_of(parent: &ItemId, name: &str) -> Option<ItemId> {
    if !valid_segment(name) {
        return None;
    }
    Some(if parent.as_str().is_empty() {
        ItemId::new(name)
    } else {
        ItemId::new(format!("{}/{name}", parent.as_str()))
    })
}

/// Percent-decoding to UTF-8; `None` when the decoded bytes are not UTF-8.
pub fn percent_decode(raw: &str) -> Option<String> {
    percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .ok()
        .map(std::borrow::Cow::into_owned)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Parse an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), the format
/// `getlastmodified` uses.
pub fn parse_http_date(value: &str) -> Option<SystemTime> {
    let rest = value.trim().split_once(", ")?.1;
    let mut parts = rest.split_whitespace();
    let day: u32 = parts.next()?.parse().ok()?;
    let month_name = parts.next()?;
    let month = u32::try_from(MONTHS.iter().position(|m| *m == month_name)?)
        .ok()?
        .checked_add(1)?;
    let year: i64 = parts.next()?.parse().ok()?;
    let mut clock = parts.next()?.split(':');
    let hour: u64 = clock.next()?.parse().ok()?;
    let minute: u64 = clock.next()?.parse().ok()?;
    let second: u64 = clock.next()?.parse().ok()?;
    if parts.next()? != "GMT" || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = u64::try_from(days_from_civil(year, month, day)?).ok()?;
    let secs = days
        .checked_mul(86_400)?
        .checked_add(hour.checked_mul(3_600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// Format a time as an IMF-fixdate.
pub fn format_http_date(time: SystemTime) -> String {
    let secs = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = secs.checked_div(86_400).unwrap_or(0);
    let rem = secs.checked_rem(86_400).unwrap_or(0);
    let (year, month, day) = civil_from_days(i64::try_from(days).unwrap_or(0));
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]
        .get(usize::try_from(days.checked_rem(7).unwrap_or(0)).unwrap_or(0))
        .copied()
        .unwrap_or("Thu");
    let month_name = MONTHS
        .get(usize::try_from(month.saturating_sub(1)).unwrap_or(0))
        .copied()
        .unwrap_or("Jan");
    format!(
        "{weekday}, {day:02} {month_name} {year} {:02}:{:02}:{:02} GMT",
        rem.checked_div(3_600).unwrap_or(0),
        rem.checked_rem(3_600)
            .and_then(|r| r.checked_div(60))
            .unwrap_or(0),
        rem.checked_rem(60).unwrap_or(0)
    )
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 {
        year.checked_sub(1)?
    } else {
        year
    };
    let era = y
        .checked_sub(if y >= 0 { 0 } else { 399 })?
        .checked_div(400)?;
    let yoe = y.checked_sub(era.checked_mul(400)?)?;
    let m = i64::from(month);
    let mp = if m > 2 {
        m.checked_sub(3)?
    } else {
        m.checked_add(9)?
    };
    let doy = mp
        .checked_mul(153)?
        .checked_add(2)?
        .checked_div(5)?
        .checked_add(i64::from(day))?
        .checked_sub(1)?;
    let doe = yoe
        .checked_mul(365)?
        .checked_add(yoe.checked_div(4)?)?
        .checked_sub(yoe.checked_div(100)?)?
        .checked_add(doy)?;
    era.checked_mul(146_097)?
        .checked_add(doe)?
        .checked_sub(719_468)
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "bounded calendar arithmetic on a day count from a u64 of seconds"
)]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> Url {
        Url::parse("https://cloud.example/remote.php/dav/files/ada/").unwrap()
    }

    #[test]
    fn hrefs_map_to_decoded_relative_ids_and_back() {
        let root = root();
        assert_eq!(
            id_for_href(&root, "/remote.php/dav/files/ada/Notes%20and%20plans.md"),
            Some(ItemId::new("Notes and plans.md"))
        );
        assert_eq!(
            id_for_href(&root, "/remote.php/dav/files/ada/"),
            Some(ItemId::new(""))
        );
        assert_eq!(id_for_href(&root, "Photos/"), Some(ItemId::new("Photos")));
        let url = url_for(&root, &ItemId::new("Notes and plans.md"), false).unwrap();
        assert_eq!(
            url.as_str(),
            "https://cloud.example/remote.php/dav/files/ada/Notes%20and%20plans.md"
        );
        assert!(
            url_for(&root, &ItemId::new("Photos"), true)
                .unwrap()
                .as_str()
                .ends_with("/Photos/")
        );
    }

    #[test]
    fn unsafe_hrefs_are_refused() {
        let root = root();
        for href in [
            "https://attacker.example/remote.php/dav/files/ada/x",
            "/remote.php/dav/files/ada/bad%2Fname",
            "/remote.php/dav/files/ada/%2e%2e",
            "/remote.php/dav/files/ada/a?token=1",
            "/remote.php/dav/files/adam/x",
            "/remote.php/dav/files/ada/%ff",
        ] {
            assert_eq!(id_for_href(&root, href), None, "{href}");
        }
        assert!(url_for(&root, &ItemId::new("../x"), false).is_none());
    }

    #[test]
    fn http_dates_round_trip() {
        let time = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        assert_eq!(
            time.duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            784_111_777
        );
        assert_eq!(format_http_date(time), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert!(parse_http_date("yesterday").is_none());
    }
}

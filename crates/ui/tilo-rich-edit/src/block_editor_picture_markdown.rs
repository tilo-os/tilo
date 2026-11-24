//! A block that is only a Markdown picture: its description, where its bytes are, the width the
//! person sized it to, and how it is set — flipped, cropped, free of the text, or wrapped by it.
//! The one place the `![alt|width|flip:hv|crop:l,t,r,b|place:free|wrap:left](source)` form is read
//! and written.

/// The narrowest and widest a person can size a picture to.
pub(crate) const IMAGE_WIDTH_RANGE: std::ops::RangeInclusive<u32> = 48..=4096;

/// The side of a picture the text runs down beside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PictureWrap {
    Left,
    Right,
}

/// How a picture is set beside the document: whether it is mirrored, the share of each edge
/// cropped off, whether it stands free of the text, and the side the text wraps down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PictureSet {
    pub(crate) flip_h: bool,
    pub(crate) flip_v: bool,
    /// Cropped off the left, top, right and bottom edges, in percent of the picture.
    pub(crate) crop: (u8, u8, u8, u8),
    /// Free of the text flow: the picture fills the column rather than fitting inside it.
    pub(crate) free: bool,
    pub(crate) wrap: Option<PictureWrap>,
}

impl PictureSet {
    /// Whether any edge is cropped off.
    pub(crate) fn cropped(self) -> bool {
        let (left, top, right, bottom) = self.crop;
        u16::from(left) + u16::from(top) + u16::from(right) + u16::from(bottom) > 0
    }

    /// Whether it is mirrored on either axis.
    pub(crate) fn flipped(self) -> bool {
        self.flip_h || self.flip_v
    }
}

/// A block that is only a Markdown picture: its description and where its bytes are. A width the
/// person sized it to (`![alt|480](…)`, as Obsidian writes it) and how it is set are not part of
/// the description; [`picture_of`] reads them.
pub(crate) fn standalone_markdown_image(source: &str) -> Option<(&str, &str)> {
    let (alt, destination, _, _) = picture_of(source)?;
    Some((alt, destination))
}

pub(crate) fn markdown_image_parts(source: &str) -> Option<(&str, &str, Option<u32>)> {
    let (alt, destination, width, _) = picture_of(source)?;
    Some((alt, destination, width))
}

/// The picture `source` writes, when it is only a picture: its description, where its bytes are,
/// the width it was sized to, and how it is set.
pub(crate) fn picture_of(source: &str) -> Option<(&str, &str, Option<u32>, PictureSet)> {
    let source = source.trim();
    let body = source.strip_prefix("![")?;
    let (alt, destination) = body.split_once("](")?;
    let destination = destination.strip_suffix(')')?.trim();
    if destination.is_empty() || destination.contains(['\n', '\r']) {
        return None;
    }
    let mut width = None;
    let mut set = PictureSet::default();
    for part in alt.split('|') {
        let part = part.trim();
        if let Ok(value) = part.parse::<u32>()
            && width.is_none()
            && IMAGE_WIDTH_RANGE.contains(&value)
        {
            width = Some(value);
            continue;
        }
        if let Some(flip) = part.strip_prefix("flip:") {
            set.flip_h = flip.contains('h');
            set.flip_v = flip.contains('v');
            continue;
        }
        if let Some(crop) = part.strip_prefix("crop:")
            && let Some(parsed) = crop_of(crop)
        {
            set.crop = parsed;
            continue;
        }
        match part {
            "place:free" => set.free = true,
            "place:inline" => set.free = false,
            "wrap:left" => set.wrap = Some(PictureWrap::Left),
            "wrap:right" => set.wrap = Some(PictureWrap::Right),
            "wrap:none" => set.wrap = None,
            _ => {}
        }
    }
    // The description is what stands before the first setting; a pipe in a description with no
    // setting after it stays part of the description.
    let mut offset = 0usize;
    let mut alt_end = alt.len();
    for part in alt.split('|') {
        if is_setting(part.trim()) {
            alt_end = offset;
            break;
        }
        offset = offset.saturating_add(part.len()).saturating_add(1);
    }
    let alt = alt
        .get(..alt_end)
        .unwrap_or(alt)
        .trim_end_matches('|')
        .trim();
    Some((alt, destination, width, set))
}

fn is_setting(part: &str) -> bool {
    part.parse::<u32>()
        .is_ok_and(|value| IMAGE_WIDTH_RANGE.contains(&value))
        || part.starts_with("flip:")
        || part.starts_with("crop:")
        || part.starts_with("place:")
        || part.starts_with("wrap:")
}

fn crop_of(crop: &str) -> Option<(u8, u8, u8, u8)> {
    let mut edges = [0u8; 4];
    let mut parts = crop.split(',');
    for edge in &mut edges {
        *edge = parts.next()?.trim().parse::<u8>().ok()?.min(95);
    }
    if parts.next().is_some() {
        return None;
    }
    if u16::from(edges[0]) + u16::from(edges[2]) > 95
        || u16::from(edges[1]) + u16::from(edges[3]) > 95
    {
        return None;
    }
    Some((edges[0], edges[1], edges[2], edges[3]))
}

pub(crate) fn with_markdown_image_width(source: &str, width: u32) -> Option<String> {
    let (_, _, _, set) = picture_of(source)?;
    with_picture(source, Some(width), set)
}

/// The picture `source` writes with its width and settings as `width` and `set` say: the one
/// writer of the form, so a person's size, flip, crop and placement travel together.
pub(crate) fn with_picture(source: &str, width: Option<u32>, set: PictureSet) -> Option<String> {
    let (alt, destination, _, _) = picture_of(source)?;
    let mut parts: Vec<String> = Vec::new();
    if !alt.is_empty() {
        parts.push(alt.to_owned());
    }
    if let Some(width) = width {
        parts.push(
            width
                .clamp(*IMAGE_WIDTH_RANGE.start(), *IMAGE_WIDTH_RANGE.end())
                .to_string(),
        );
    }
    match (set.flip_h, set.flip_v) {
        (true, true) => parts.push("flip:hv".to_owned()),
        (true, false) => parts.push("flip:h".to_owned()),
        (false, true) => parts.push("flip:v".to_owned()),
        (false, false) => {}
    }
    let (left, top, right, bottom) = set.crop;
    if set.cropped() {
        parts.push(format!("crop:{left},{top},{right},{bottom}"));
    }
    if set.free {
        parts.push("place:free".to_owned());
    }
    if let Some(wrap) = set.wrap {
        parts.push(match wrap {
            PictureWrap::Left => "wrap:left".to_owned(),
            PictureWrap::Right => "wrap:right".to_owned(),
        });
    }
    Some(format!("![{}]({destination})", parts.join("|")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_reads_its_size_and_setting_and_writes_them_back() {
        let source = "![Scan|320|flip:hv|crop:5,10,5,10|place:free|wrap:left](scan.png)";
        assert_eq!(
            picture_of(source),
            Some((
                "Scan",
                "scan.png",
                Some(320),
                PictureSet {
                    flip_h: true,
                    flip_v: true,
                    crop: (5, 10, 5, 10),
                    free: true,
                    wrap: Some(PictureWrap::Left),
                }
            ))
        );
        assert_eq!(
            with_picture(source, Some(320), picture_of(source).unwrap().3).as_deref(),
            Some(source)
        );
        assert_eq!(
            with_markdown_image_width("![Scan|flip:h](scan.png)", 300).as_deref(),
            Some("![Scan|300|flip:h](scan.png)")
        );
        assert_eq!(
            standalone_markdown_image("![Scan|flip:h](scan.png)"),
            Some(("Scan", "scan.png")),
            "the setting is not part of the description"
        );
        assert_eq!(
            picture_of("![A|B](a.png)").map(|(alt, _, width, _)| (alt, width)),
            Some(("A|B", None)),
            "a pipe in a description with no setting is text"
        );
        assert!(picture_of("![Scan|crop:60,0,60,0](a.png)").is_some());
        assert_eq!(
            picture_of("![Scan|crop:60,0,60,0](a.png)").map(|(_, _, _, set)| set.crop),
            Some((0, 0, 0, 0)),
            "a crop that would hide the picture is refused, leaving it plain"
        );
    }
}

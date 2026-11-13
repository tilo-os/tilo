//! A block that is only a Markdown picture: its description, where its bytes are, and the width
//! the person sized it to. The one place the `![alt|width](source)` form is read and written.

/// A block that is only a Markdown picture: its description and where its
/// bytes are. A width the person sized it to (`![alt|480](…)`, as Obsidian
/// writes it) is not part of the description; [`markdown_image_parts`]
/// reads it.
pub(super) fn standalone_markdown_image(source: &str) -> Option<(&str, &str)> {
    let (alt, destination, _) = markdown_image_parts(source)?;
    Some((alt, destination))
}

/// The narrowest and widest a person can size a picture to.
pub(super) const IMAGE_WIDTH_RANGE: std::ops::RangeInclusive<u32> = 48..=4096;

pub(super) fn markdown_image_parts(source: &str) -> Option<(&str, &str, Option<u32>)> {
    let source = source.trim();
    let body = source.strip_prefix("![")?;
    let (alt, destination) = body.split_once("](")?;
    let destination = destination.strip_suffix(')')?.trim();
    if destination.is_empty() || destination.contains(['\n', '\r']) {
        return None;
    }
    let (alt, width) = match alt.rsplit_once('|') {
        Some((text, width)) => match width.trim().parse::<u32>() {
            Ok(width) if IMAGE_WIDTH_RANGE.contains(&width) => (text, Some(width)),
            _ => (alt, None),
        },
        None => (alt, None),
    };
    Some((alt.trim(), destination, width))
}

/// `source` with its picture sized to `width` logical pixels.
pub(super) fn with_markdown_image_width(source: &str, width: u32) -> Option<String> {
    let (alt, destination, _) = markdown_image_parts(source)?;
    let width = width.clamp(*IMAGE_WIDTH_RANGE.start(), *IMAGE_WIDTH_RANGE.end());
    Some(format!("![{alt}|{width}]({destination})"))
}

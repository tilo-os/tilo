//! Flathub, from the `AppStream` metadata Flatpak caches for each remote.
//!
//! Flatpak keeps `appstream/<remote>/<arch>/active/appstream.xml.gz` in each
//! installation, with the icons it caches under `active/icons/<size>/`, and
//! refreshes it itself (`flatpak update --appstream`, which the hourly
//! `tilo-app-update` run asks for). Reading it needs no network and no
//! Flatpak process; it is a large file, so read it on a worker thread.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::entry::{CatalogueEntry, Installation, Source};

/// The remote Flathub is configured as, in either installation.
pub const FLATHUB_REMOTE: &str = "flathub";

/// The system installation's directory.
pub const SYSTEM_INSTALLATION: &str = "/var/lib/flatpak";

/// One cached `AppStream` file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppstreamSource {
    pub remote: String,
    pub installation: Installation,
    /// `appstream.xml.gz`, or `appstream.xml` where Flatpak left it plain.
    pub file: PathBuf,
    /// The `icons` directory beside it, holding `64x64/` and `128x128/`.
    pub icons: PathBuf,
}

/// The person's installation's directory.
#[must_use]
pub fn user_installation() -> Option<PathBuf> {
    tilo_paths::data_dir().map(|dir| dir.join("flatpak"))
}

/// The Flathub `AppStream` files cached on this device: the person's
/// installation first, so its copy wins when both have Flathub.
#[must_use]
pub fn cached_sources() -> Vec<AppstreamSource> {
    let mut sources = Vec::new();
    if let Some(user) = user_installation() {
        sources.extend(sources_in(&user, Installation::User));
    }
    sources.extend(sources_in(
        Path::new(SYSTEM_INSTALLATION),
        Installation::System,
    ));
    sources
}

/// The Flathub `AppStream` file of the Flatpak installation at `root`, for
/// this machine's architecture, when Flatpak has cached one.
#[must_use]
pub fn sources_in(root: &Path, installation: Installation) -> Vec<AppstreamSource> {
    let active = root
        .join("appstream")
        .join(FLATHUB_REMOTE)
        .join(std::env::consts::ARCH)
        .join("active");
    ["appstream.xml.gz", "appstream.xml"]
        .into_iter()
        .map(|name| active.join(name))
        .find(|file| file.is_file())
        .map(|file| AppstreamSource {
            remote: FLATHUB_REMOTE.to_owned(),
            installation,
            file,
            icons: active.join("icons"),
        })
        .into_iter()
        .collect()
}

/// A value that changes whenever Flatpak replaces a cached `AppStream` file,
/// so a reader knows to read it again. Only `stat`s.
#[must_use]
pub fn cache_stamp(sources: &[AppstreamSource]) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for source in sources {
        source.file.hash(&mut hasher);
        if let Ok(meta) = std::fs::metadata(&source.file) {
            meta.len().hash(&mut hasher);
            if let Ok(modified) = meta.modified() {
                modified.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Every Flathub app cached on this device, with names and tag lines in
/// `locale` (a BCP 47 tag such as `de-DE`) where the metadata has them. An
/// app both installations list appears once, from the person's.
#[must_use]
pub fn load_cached(locale: &str) -> Vec<CatalogueEntry> {
    load_all(&cached_sources(), locale)
}

/// [`load_cached`] over the given sources. A source that cannot be read is
/// skipped: the others still answer.
#[must_use]
pub fn load_all(sources: &[AppstreamSource], locale: &str) -> Vec<CatalogueEntry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for source in sources {
        if let Ok(found) = load(source, locale) {
            entries.extend(found.into_iter().filter(|entry| seen.insert(entry.key())));
        }
    }
    entries
}

/// The apps of one cached `AppStream` file.
///
/// # Errors
///
/// The file cannot be opened, decompressed or parsed.
pub fn load(source: &AppstreamSource, locale: &str) -> Result<Vec<CatalogueEntry>, String> {
    let file = std::fs::File::open(&source.file).map_err(|error| error.to_string())?;
    let compressed = source
        .file
        .extension()
        .is_some_and(|extension| extension == "gz");
    let reader: Box<dyn Read> = if compressed {
        Box::new(flate2::read::GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    parse(BufReader::new(reader), source, locale)
}

/// What is being read inside a component.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Id,
    Name,
    Summary,
    Icon,
    Category,
    Keyword,
    Bundle,
}

/// One `<component>` while it is read.
#[derive(Default)]
struct Component {
    application: bool,
    id: String,
    bundle: String,
    /// `(xml:lang, text)`; `None` is the untranslated text.
    names: Vec<(Option<String>, String)>,
    summaries: Vec<(Option<String>, String)>,
    keywords: Vec<(Option<String>, String)>,
    categories: Vec<String>,
    /// `(width, file name)` of each cached icon.
    cached_icons: Vec<(u32, String)>,
    remote_icon: Option<String>,
}

/// Whether text in `lang` is for a desktop speaking `locale`: the same tag,
/// or the same language.
fn speaks(lang: &str, locale: &str) -> bool {
    let primary = |tag: &str| {
        tag.split(['-', '_'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    lang.replace('_', "-").eq_ignore_ascii_case(locale) || primary(lang) == primary(locale)
}

/// The text for `locale`, else the untranslated one.
fn localized(texts: &[(Option<String>, String)], locale: &str) -> Option<String> {
    texts
        .iter()
        .find(|(lang, _)| lang.as_deref().is_some_and(|lang| speaks(lang, locale)))
        .or_else(|| texts.iter().find(|(lang, _)| lang.is_none()))
        .map(|(_, text)| text.clone())
}

impl Component {
    /// Its catalogue entry, when it is an application with an id and a name.
    fn into_entry(self, source: &AppstreamSource, locale: &str) -> Option<CatalogueEntry> {
        if !self.application {
            return None;
        }
        // `app/<id>/<arch>/<branch>` names the Flatpak itself; older
        // metadata's `<id>` may carry a `.desktop` suffix.
        let id = self
            .bundle
            .strip_prefix("app/")
            .and_then(|rest| rest.split('/').next())
            .filter(|id| !id.is_empty())
            .map_or_else(
                || {
                    self.id
                        .strip_suffix(".desktop")
                        .unwrap_or(&self.id)
                        .to_owned()
                },
                str::to_owned,
            );
        let name = localized(&self.names, locale)?;
        if id.is_empty() || name.trim().is_empty() {
            return None;
        }
        let untranslated = self
            .names
            .iter()
            .find(|(lang, _)| lang.is_none())
            .map(|(_, text)| text.clone());
        let icon_file = self
            .cached_icons
            .iter()
            .filter(|(width, _)| *width <= 128)
            .max_by_key(|(width, _)| *width)
            .or_else(|| self.cached_icons.iter().min_by_key(|(width, _)| *width))
            .map(|(width, file)| source.icons.join(format!("{width}x{width}")).join(file));
        Some(CatalogueEntry {
            id,
            source: Source::Flathub {
                remote: source.remote.clone(),
                installation: source.installation,
            },
            other_names: untranslated
                .filter(|other| *other != name)
                .into_iter()
                .collect(),
            name,
            tagline: localized(&self.summaries, locale).unwrap_or_default(),
            description: String::new(),
            keywords: self
                .keywords
                .into_iter()
                .filter(|(lang, _)| lang.as_deref().is_none_or(|lang| speaks(lang, locale)))
                .map(|(_, word)| word)
                .collect(),
            category: self.categories.into_iter().next(),
            icon_file,
            icon_url: self.remote_icon,
            resource_types: Vec::new(),
            actions: Vec::new(),
        })
    }
}

fn attribute(start: &BytesStart<'_>, key: &[u8]) -> Option<String> {
    start
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.as_ref() == key)
        .and_then(|attribute| {
            let raw = std::str::from_utf8(attribute.value.as_ref()).ok()?;
            quick_xml::escape::unescape(raw)
                .ok()
                .map(std::borrow::Cow::into_owned)
        })
}

/// The character an entity or character reference stands for; `AppStream`
/// declares no entities beyond the predefined ones.
fn reference_char(reference: &quick_xml::events::BytesRef<'_>) -> Option<char> {
    if let Ok(Some(c)) = reference.resolve_char_ref() {
        return Some(c);
    }
    let name = reference.decode().ok()?;
    quick_xml::escape::resolve_predefined_entity(&name)?
        .chars()
        .next()
}

/// The read of one collection: where it is, and what it has found.
struct Parser<'a> {
    source: &'a AppstreamSource,
    locale: &'a str,
    /// Elements open: 1 is the root `<components>`, 2 a component, 3 its
    /// fields, 4 the items of `<categories>` and `<keywords>`.
    depth: usize,
    component: Option<Component>,
    /// The field being read, its `xml:lang` (for an icon, its type) and,
    /// for an icon, its width.
    field: Option<(Field, Option<String>, u32)>,
    text: String,
    entries: Vec<CatalogueEntry>,
}

impl Parser<'_> {
    fn start(&mut self, start: &BytesStart<'_>) {
        self.depth = self.depth.saturating_add(1);
        let name = start.name();
        let field = match (self.depth, name.as_ref()) {
            (2, b"component") => {
                let kind = attribute(start, b"type").unwrap_or_default();
                self.component = Some(Component {
                    application: matches!(kind.as_str(), "desktop-application" | "desktop"),
                    ..Component::default()
                });
                None
            }
            (3, b"id") => Some((Field::Id, None, 0)),
            (3, b"name") => Some((Field::Name, attribute(start, b"xml:lang"), 0)),
            (3, b"summary") => Some((Field::Summary, attribute(start, b"xml:lang"), 0)),
            (3, b"bundle") if attribute(start, b"type").as_deref() == Some("flatpak") => {
                Some((Field::Bundle, None, 0))
            }
            (3, b"icon") => {
                let kind =
                    attribute(start, b"type").filter(|kind| kind == "cached" || kind == "remote");
                let width = attribute(start, b"width")
                    .and_then(|width| width.parse().ok())
                    .unwrap_or(64);
                kind.map(|kind| (Field::Icon, Some(kind), width))
            }
            (4, b"category") => Some((Field::Category, None, 0)),
            (4, b"keyword") => Some((Field::Keyword, attribute(start, b"xml:lang"), 0)),
            _ => None,
        };
        if self.component.is_some() && field.is_some() {
            self.field = field;
            self.text.clear();
        }
    }

    fn end(&mut self) {
        if let (Some((kind, lang, width)), Some(current)) =
            (self.field.take(), self.component.as_mut())
        {
            let value = self.text.trim().to_owned();
            if !value.is_empty() {
                match kind {
                    Field::Id => current.id = value,
                    Field::Name => current.names.push((lang, value)),
                    Field::Summary => current.summaries.push((lang, value)),
                    Field::Bundle => current.bundle = value,
                    Field::Category => current.categories.push(value),
                    Field::Keyword => current.keywords.push((lang, value)),
                    Field::Icon => {
                        if lang.as_deref() == Some("cached") {
                            current.cached_icons.push((width, value));
                        } else if value.starts_with("https://") {
                            current.remote_icon.get_or_insert(value);
                        }
                    }
                }
            }
            self.text.clear();
        }
        if self.depth == 2
            && let Some(done) = self.component.take()
            && let Some(entry) = done.into_entry(self.source, self.locale)
        {
            self.entries.push(entry);
        }
        self.depth = self.depth.saturating_sub(1);
    }
}

/// The applications of an `AppStream` collection read from `input`.
///
/// # Errors
///
/// The XML is malformed.
pub fn parse(
    input: impl BufRead,
    source: &AppstreamSource,
    locale: &str,
) -> Result<Vec<CatalogueEntry>, String> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut parser = Parser {
        source,
        locale,
        depth: 0,
        component: None,
        field: None,
        text: String::new(),
        entries: Vec::new(),
    };
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|error| error.to_string())?
        {
            Event::Start(start) => parser.start(&start),
            Event::Text(chunk) if parser.field.is_some() => {
                let chunk = chunk.xml10_content().map_err(|error| error.to_string())?;
                parser.text.push_str(&chunk);
            }
            Event::GeneralRef(reference) if parser.field.is_some() => {
                parser.text.extend(reference_char(&reference));
            }
            Event::End(_) => parser.end(),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(parser.entries)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const SPOTIFY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<components version="0.16" origin="flathub">
  <component type="desktop-application">
    <id>com.spotify.Client</id>
    <name>Spotify</name>
    <name xml:lang="de">Spotify</name>
    <summary>Online music streaming service</summary>
    <summary xml:lang="de">Musik-Streaming</summary>
    <description><p>Listen &amp; discover.</p></description>
    <icon type="cached" height="64" width="64">com.spotify.Client.png</icon>
    <icon type="cached" height="128" width="128">com.spotify.Client.png</icon>
    <icon type="remote" height="128" width="128">https://dl.flathub.org/media/spotify.png</icon>
    <categories><category>Audio</category><category>Music</category></categories>
    <keywords><keyword>music</keyword><keyword xml:lang="de">Musik</keyword></keywords>
    <bundle type="flatpak" runtime="org.freedesktop.Platform/x86_64/24.08">app/com.spotify.Client/x86_64/stable</bundle>
  </component>
  <component type="runtime">
    <id>org.freedesktop.Platform</id>
    <name>Freedesktop Platform</name>
  </component>
  <component type="desktop">
    <id>org.gnome.Rhythmbox3.desktop</id>
    <name>Rhythmbox</name>
    <summary>Play and organize your music</summary>
  </component>
</components>"#;

    pub(crate) fn source(root: &Path) -> AppstreamSource {
        AppstreamSource {
            remote: FLATHUB_REMOTE.into(),
            installation: Installation::System,
            file: root.join("appstream.xml"),
            icons: root.join("icons"),
        }
    }

    #[test]
    fn flathub_metadata_reads_into_entries_with_their_cached_icons() {
        let root = Path::new("/var/lib/flatpak/appstream/flathub/x86_64/active");
        let entries = parse(SPOTIFY.as_bytes(), &source(root), "en-US").expect("parses");
        assert_eq!(entries.len(), 2, "applications only: {entries:?}");
        let spotify = &entries[0];
        assert_eq!(spotify.id, "com.spotify.Client");
        assert_eq!(spotify.name, "Spotify");
        assert_eq!(spotify.tagline, "Online music streaming service");
        assert_eq!(spotify.category.as_deref(), Some("Audio"));
        assert_eq!(spotify.keywords, ["music"]);
        assert_eq!(
            spotify.icon_file.as_deref(),
            Some(root.join("icons/128x128/com.spotify.Client.png").as_path())
        );
        assert_eq!(
            spotify.icon_url.as_deref(),
            Some("https://dl.flathub.org/media/spotify.png")
        );
        assert!(spotify.source.needs_install());
        assert_eq!(
            entries[1].id, "org.gnome.Rhythmbox3",
            "the .desktop suffix goes"
        );

        let german = parse(SPOTIFY.as_bytes(), &source(root), "de-DE").expect("parses");
        assert_eq!(german[0].tagline, "Musik-Streaming");
        assert_eq!(german[0].keywords, ["music", "Musik"]);
    }

    #[test]
    fn a_compressed_cache_is_found_per_installation_and_read() {
        use std::io::Write as _;
        let root = tempfile::tempdir().unwrap();
        let active = root
            .path()
            .join("appstream/flathub")
            .join(std::env::consts::ARCH)
            .join("active");
        std::fs::create_dir_all(&active).unwrap();
        let mut gz = flate2::write::GzEncoder::new(
            std::fs::File::create(active.join("appstream.xml.gz")).unwrap(),
            flate2::Compression::fast(),
        );
        gz.write_all(SPOTIFY.as_bytes()).unwrap();
        gz.finish().unwrap();
        let sources = sources_in(root.path(), Installation::User);
        assert_eq!(sources.len(), 1);
        let stamp = cache_stamp(&sources);
        let entries = load_all(&sources, "en");
        assert!(entries.iter().any(|entry| entry.id == "com.spotify.Client"));
        assert_eq!(stamp, cache_stamp(&sources), "nothing changed");
    }
}

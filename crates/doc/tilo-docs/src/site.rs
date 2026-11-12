//! Writes a book as a static site under the book's base:
//!
//! ```text
//! index.html, <slug>/index.html   each page
//! index.md, <slug>.md             each page as Markdown, front matter and all
//! llms.txt, llms-full.txt         the book for agents
//! search.json                     the search index
//! assets/…                        style, script, icon and fonts, named by content
//! ```
//!
//! Every link into the book must name a page that exists, and a heading on it.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::book::Book;
use crate::layout::{self, Assets, Context};
use crate::markdown::{self, Rendered};
use crate::search;

const STYLE: &str = include_str!("../assets/docs.css");
const SCRIPT: &str = include_str!("../assets/docs.js");
const ICON: &str = include_str!("../assets/icon.svg");
/// Marks a directory this program wrote, which it may replace.
const MARKER: &str = ".tilo-docs";

/// The faces the site sets text in, from a font catalog when one is given:
/// `(file in the catalog, family, style)`.
const FONTS: [(&str, &str, &str); 2] = [
    ("TiloSans[wdth,wght].ttf", "Tilo Sans", "normal"),
    ("TiloSans-Italic[wdth,wght].ttf", "Tilo Sans", "italic"),
];
const FONT_LICENSE: &str = "LICENSES/Tilo-Sans-OFL.txt";

#[derive(Debug, Default)]
pub struct Options {
    /// A font catalog (`crates/sdk/ui-core/assets/fonts` prepared, or a web
    /// bundle's `fonts/`); without one the site uses the reader's system faces.
    pub fonts: Option<PathBuf>,
}

#[derive(Debug)]
pub struct Report {
    pub pages: usize,
    pub files: usize,
}

/// Builds `book` into `out`, replacing what an earlier build wrote there.
///
/// # Errors
/// When `out` holds files this program did not write, a link into the book
/// names no page or heading, or a file cannot be written.
pub fn build(book: &Book, out: &Path, options: &Options) -> Result<Report, Error> {
    prepare(out)?;
    let mut writer = Writer { out, files: 0 };
    let assets = write_assets(&mut writer, book, options)?;
    let rendered: Vec<Rendered> = book
        .pages
        .iter()
        .map(|(_, page)| markdown::render(&page.markdown))
        .collect();
    check_links(book, &rendered)?;
    let mut entries = Vec::with_capacity(book.pages.len());
    for (number, ((_, page), rendered)) in book.pages.iter().zip(&rendered).enumerate() {
        let context = Context {
            book,
            assets: &assets,
            page,
            rendered,
            previous: number
                .checked_sub(1)
                .and_then(|previous| book.pages.get(previous))
                .map(|(_, page)| page),
            next: book
                .pages
                .get(number.saturating_add(1))
                .map(|(_, page)| page),
        };
        let directory = if page.slug.is_empty() {
            PathBuf::new()
        } else {
            PathBuf::from(&page.slug)
        };
        writer.write(
            &directory.join("index.html"),
            layout::page(&context).as_bytes(),
        )?;
        let name = if page.slug.is_empty() {
            "index"
        } else {
            &page.slug
        };
        writer.write(Path::new(&format!("{name}.md")), page.document.as_bytes())?;
        let section = book
            .sections
            .get(book.pages.get(number).map_or(0, |(section, _)| *section));
        entries.push(search::Entry {
            url: &page.url,
            title: &page.title,
            section: section.map_or("", |section| section.title.as_str()),
            description: &page.description,
            headings: search::Entry::headings_of(&rendered.headings),
            text: &rendered.text,
        });
    }
    writer.write(Path::new("search.json"), search::index(&entries).as_bytes())?;
    let summary = book
        .pages
        .first()
        .map_or("", |(_, page)| page.description.as_str());
    writer.write(
        Path::new("llms.txt"),
        crate::llms::index(book, summary).as_bytes(),
    )?;
    writer.write(
        Path::new("llms-full.txt"),
        crate::llms::full(book).as_bytes(),
    )?;
    Ok(Report {
        pages: book.pages.len(),
        files: writer.files,
    })
}

fn prepare(out: &Path) -> Result<(), Error> {
    if out.exists() {
        let empty = fs::read_dir(out)
            .map_err(|error| Error::Io(out.to_path_buf(), error))?
            .next()
            .is_none();
        if !empty && !out.join(MARKER).exists() {
            return Err(Error::NotOurs(out.to_path_buf()));
        }
        fs::remove_dir_all(out).map_err(|error| Error::Io(out.to_path_buf(), error))?;
    }
    fs::create_dir_all(out).map_err(|error| Error::Io(out.to_path_buf(), error))?;
    // Written first, so a build that fails halfway is still known as ours.
    let marker = out.join(MARKER);
    fs::write(
        &marker,
        "Written by tilo-docs; the next build replaces this directory.\n",
    )
    .map_err(|error| Error::Io(marker, error))
}

struct Writer<'a> {
    out: &'a Path,
    files: usize,
}

impl Writer<'_> {
    fn write(&mut self, relative: &Path, bytes: &[u8]) -> Result<(), Error> {
        let path = self.out.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| Error::Io(parent.to_path_buf(), error))?;
        }
        fs::write(&path, bytes).map_err(|error| Error::Io(path, error))?;
        self.files = self.files.saturating_add(1);
        Ok(())
    }

    /// Writes `bytes` under `assets/` named by their content, and answers the
    /// address pages name it by.
    fn asset(
        &mut self,
        book: &Book,
        stem: &str,
        extension: &str,
        bytes: &[u8],
    ) -> Result<String, Error> {
        let hash = blake3::hash(bytes).to_hex();
        let name = format!("{stem}.{}.{extension}", hash.get(..12).unwrap_or_default());
        self.write(&Path::new("assets").join(&name), bytes)?;
        Ok(format!("{}assets/{name}", book.base))
    }
}

fn write_assets(writer: &mut Writer<'_>, book: &Book, options: &Options) -> Result<Assets, Error> {
    let mut faces = String::new();
    let mut preload_fonts = Vec::new();
    if let Some(catalog) = &options.fonts {
        for (file, family, style) in FONTS {
            let path = catalog.join(file);
            let bytes = fs::read(&path).map_err(|error| Error::Io(path, error))?;
            let stem = if style == "italic" {
                "tilo-sans-italic"
            } else {
                "tilo-sans"
            };
            let url = writer.asset(book, stem, "ttf", &bytes)?;
            let _ = writeln!(
                faces,
                "@font-face{{font-family:\"{family}\";font-style:{style};font-weight:400 700;font-stretch:75% 100%;\
                 font-display:swap;src:url(\"{url}\") format(\"truetype\")}}"
            );
            if style == "normal" {
                preload_fonts.push(url);
            }
        }
        let license = catalog.join(FONT_LICENSE);
        let text = fs::read(&license).map_err(|error| Error::Io(license, error))?;
        writer.write(Path::new("assets/fonts-license.txt"), &text)?;
    }
    let stylesheet = writer.asset(book, "docs", "css", format!("{faces}{STYLE}").as_bytes())?;
    let script = writer.asset(book, "docs", "js", SCRIPT.as_bytes())?;
    let icon = writer.asset(book, "icon", "svg", ICON.as_bytes())?;
    Ok(Assets {
        stylesheet,
        script,
        icon,
        preload_fonts,
    })
}

/// Every link into the book's base names a page and, with a fragment, one of
/// its headings.
fn check_links(book: &Book, rendered: &[Rendered]) -> Result<(), Error> {
    let mut anchors: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut files: HashSet<String> = HashSet::new();
    for ((_, page), rendered) in book.pages.iter().zip(rendered) {
        anchors.insert(
            page.url.as_str(),
            rendered
                .headings
                .iter()
                .map(|heading| heading.id.as_str())
                .collect(),
        );
        files.insert(page.markdown_url(&book.base));
    }
    for name in ["llms.txt", "llms-full.txt"] {
        files.insert(format!("{}{name}", book.base));
    }
    let mut broken = Vec::new();
    for ((_, page), rendered) in book.pages.iter().zip(rendered) {
        for link in &rendered.links {
            let target = link
                .strip_prefix(&book.origin)
                .filter(|_| !book.origin.is_empty())
                .unwrap_or(link);
            let (path, fragment) = target.split_once('#').unwrap_or((target, ""));
            let path = if path.is_empty() {
                page.url.as_str()
            } else {
                path
            };
            let elsewhere = book
                .api
                .as_ref()
                .is_some_and(|api| path.starts_with(&api.url));
            if !path.starts_with(&book.base) || elsewhere {
                continue;
            }
            match anchors.get(path) {
                Some(ids) if fragment.is_empty() || ids.contains(fragment) => {}
                Some(_) => broken.push(format!(
                    "{}: {link}: no heading #{fragment} there",
                    page.url
                )),
                None if files.contains(path) => {}
                None => broken.push(format!("{}: {link}: no such page", page.url)),
            }
        }
    }
    if broken.is_empty() {
        Ok(())
    } else {
        Err(Error::BrokenLinks(broken))
    }
}

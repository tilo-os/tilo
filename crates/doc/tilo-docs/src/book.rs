//! A book as `tools/docs/developer_docs.py export` writes it: `manifest.json`
//! (the navigation) and `pages/<id>.md`, each page's Markdown behind front
//! matter whose keys are the page's record fields.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::Error;

/// One page: the fields of its front matter, and its Markdown body.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub id: String,
    pub slug: String,
    /// The page's address on its site, from the site's root (`/docs/files/`).
    pub url: String,
    pub title: String,
    /// Its name in the navigation, shorter than its title.
    pub nav: String,
    pub description: String,
    pub section: String,
    pub order: u32,
    /// The repository file it is published from, or for a generated page the
    /// code it documents.
    pub source: String,
    pub source_url: String,
    /// The program that wrote it from `source`, for a generated page.
    #[serde(default)]
    pub generator: Option<String>,
    #[serde(skip)]
    pub markdown: String,
    /// The page as exported, front matter and all: what its `.md` serves.
    #[serde(skip)]
    pub document: String,
}

impl Page {
    /// The address of the page's Markdown, beside its HTML (`/docs/files.md`).
    #[must_use]
    pub fn markdown_url(&self, base: &str) -> String {
        let name = if self.slug.is_empty() {
            "index"
        } else {
            &self.slug
        };
        format!("{base}{name}.md")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Section {
    pub title: String,
}

/// A reference another program builds and the site serves under the book
/// (the Rust API reference), which links may lead into.
#[derive(Debug, Clone, Deserialize)]
pub struct Api {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Examples {
    #[serde(default)]
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Index {
    #[serde(default)]
    title: String,
    #[serde(default)]
    origin: String,
    base: String,
    repository: String,
    commit: Option<String>,
    #[serde(default)]
    examples: Examples,
    api: Option<Api>,
    sections: Vec<SectionIndex>,
}

#[derive(Debug, Clone, Deserialize)]
struct SectionIndex {
    title: String,
    pages: Vec<PageIndex>,
}

#[derive(Debug, Clone, Deserialize)]
struct PageIndex {
    id: String,
}

/// A whole book: its pages in navigation order, grouped into sections.
#[derive(Debug, Clone)]
pub struct Book {
    pub title: String,
    /// The site it is published on (`https://tilo.sh`), empty when unknown.
    pub origin: String,
    /// Where the book lives on its site (`/docs/`).
    pub base: String,
    pub repository: String,
    pub commit: Option<String>,
    pub examples: Examples,
    pub api: Option<Api>,
    pub sections: Vec<Section>,
    /// Every page in order; `section` indexes `sections`.
    pub pages: Vec<(usize, Page)>,
}

impl Book {
    /// Reads an exported book from `dir`.
    ///
    /// # Errors
    /// When the manifest or a page it lists is missing or malformed.
    pub fn load(dir: &Path) -> Result<Self, Error> {
        let manifest = dir.join("manifest.json");
        let index: Index = serde_json::from_str(&read(&manifest)?)
            .map_err(|error| Error::Malformed(manifest.clone(), error.to_string()))?;
        if !index.base.starts_with('/') || !index.base.ends_with('/') {
            return Err(Error::Malformed(
                manifest,
                format!("base {:?} must start and end with /", index.base),
            ));
        }
        let mut sections = Vec::new();
        let mut pages = Vec::new();
        for (number, section) in index.sections.into_iter().enumerate() {
            for entry in section.pages {
                let path = dir.join("pages").join(format!("{}.md", entry.id));
                let page = parse_page(&path, &read(&path)?)?;
                if page.id != entry.id {
                    return Err(Error::Malformed(
                        path,
                        format!("its id is {:?}, the manifest lists {:?}", page.id, entry.id),
                    ));
                }
                pages.push((number, page));
            }
            sections.push(Section {
                title: section.title,
            });
        }
        if pages.is_empty() {
            return Err(Error::Malformed(manifest, "the book has no pages".into()));
        }
        Ok(Self {
            title: if index.title.is_empty() {
                "Docs".into()
            } else {
                index.title
            },
            origin: index.origin,
            base: index.base,
            repository: index.repository,
            commit: index.commit,
            examples: index.examples,
            api: index.api,
            sections,
            pages,
        })
    }

    /// The page's address with the site's origin, for links that leave the
    /// site (canonical links, llms.txt).
    #[must_use]
    pub fn absolute(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }
}

fn read(path: &Path) -> Result<String, Error> {
    fs::read_to_string(path).map_err(|error| Error::Io(PathBuf::from(path), error))
}

/// Splits a page into its front matter (one `key: <JSON value>` per line, as
/// the export writes it) and its Markdown body.
///
/// # Errors
/// When the front matter is missing, a line is not `key: value`, or a field
/// is missing or of the wrong type.
pub fn parse_page(path: &Path, text: &str) -> Result<Page, Error> {
    let malformed = |why: &str| Error::Malformed(path.to_path_buf(), why.to_string());
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| malformed("no front matter"))?;
    let (front, body) = rest
        .split_once("\n---\n")
        .ok_or_else(|| malformed("front matter is not closed"))?;
    let mut fields = serde_json::Map::new();
    for line in front.lines() {
        let (key, value) = line
            .split_once(": ")
            .ok_or_else(|| malformed(&format!("{line:?} is not `key: value`")))?;
        let value =
            serde_json::from_str(value).map_err(|error| malformed(&format!("{key}: {error}")))?;
        fields.insert(key.to_string(), value);
    }
    let mut page: Page = serde_json::from_value(serde_json::Value::Object(fields))
        .map_err(|error| malformed(&error.to_string()))?;
    page.markdown = body.trim_start_matches('\n').to_string();
    page.document = text.to_string();
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "---\nid: \"files\"\nslug: \"files\"\nurl: \"/docs/files/\"\ntitle: \"Files\"\nnav: \"Files\"\ndescription: \"Put files in.\"\nsource: \"docs/tilo-sh/files.md\"\nsourceUrl: \"https://example/src\"\nsection: \"Guides\"\norder: 3\n---\n\n# Files\n\nBody.\n";

    #[test]
    fn a_page_is_its_front_matter_and_its_body() {
        let page = parse_page(Path::new("files.md"), PAGE).unwrap();
        assert_eq!(page.title, "Files");
        assert_eq!(page.order, 3);
        assert_eq!(page.generator, None);
        assert_eq!(page.markdown, "# Files\n\nBody.\n");
        assert_eq!(page.markdown_url("/docs/"), "/docs/files.md");
    }

    #[test]
    fn a_page_without_its_fields_is_refused() {
        let error = parse_page(Path::new("x.md"), "---\nid: \"x\"\n---\nBody").unwrap_err();
        assert!(error.to_string().contains("missing field"), "{error}");
    }
}

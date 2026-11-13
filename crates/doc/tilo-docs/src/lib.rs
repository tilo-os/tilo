//! Tilo's documentation books, from the pages `tools/docs/developer_docs.py
//! export` writes to a static site: HTML with search, dark mode and
//! language tabs that stay in step, each page's Markdown beside it, and
//! `llms.txt` for agents.
//!
//! A page is a record: its front matter (`book::Page`) and its Markdown. The
//! site is one way to show those records; the renderer keeps no state of its
//! own, so the same pages can be read as records elsewhere.

pub mod book;
pub mod highlight;
pub mod html;
pub mod layout;
pub mod llms;
pub mod markdown;
pub mod search;
pub mod site;
pub mod slug;
pub mod writer;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("{0}: {1}")]
    Malformed(PathBuf, String),
    #[error("{0} holds files tilo-docs did not write; choose an empty or new directory")]
    NotOurs(PathBuf),
    #[error("links that lead nowhere:\n  {}", .0.join("\n  "))]
    BrokenLinks(Vec<String>),
}

//! The languages a file is written in, as the engine names them: the label a file's language
//! is stored by, and which server answers for it.
//!
//! There is no UI here: the app owns what a language colours and comments with, and this
//! crate only needs the name a language is known by to find its server and to tell the server
//! which language a document is in.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Toml,
    Markdown,
    Json,
    Yaml,
    Python,
    JavaScript,
    TypeScript,
    Html,
    Css,
    Shell,
    C,
    Cpp,
    Go,
    Verilog,
    SystemVerilog,
    PlainText,
}

impl Language {
    pub const ALL: [Language; 17] = [
        Language::Rust,
        Language::Toml,
        Language::Markdown,
        Language::Json,
        Language::Yaml,
        Language::Python,
        Language::JavaScript,
        Language::TypeScript,
        Language::Html,
        Language::Css,
        Language::Shell,
        Language::C,
        Language::Cpp,
        Language::Go,
        Language::Verilog,
        Language::SystemVerilog,
        Language::PlainText,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Language::Rust => "Rust",
            Language::Toml => "TOML",
            Language::Markdown => "Markdown",
            Language::Json => "JSON",
            Language::Yaml => "YAML",
            Language::Python => "Python",
            Language::JavaScript => "JavaScript",
            Language::TypeScript => "TypeScript",
            Language::Html => "HTML",
            Language::Css => "CSS",
            Language::Shell => "Shell",
            Language::C => "C",
            Language::Cpp => "C++",
            Language::Go => "Go",
            Language::Verilog => "Verilog",
            Language::SystemVerilog => "SystemVerilog",
            Language::PlainText => "Plain Text",
        }
    }

    /// The language whose label is `label`, the case of its letters aside.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|language| language.label().eq_ignore_ascii_case(label))
    }

    /// The language a file is written in, by its name's extension: plain text when no extension
    /// says one.
    #[must_use]
    pub fn from_path(name: &str) -> Self {
        let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "rs" => Language::Rust,
            "toml" => Language::Toml,
            "md" | "markdown" => Language::Markdown,
            "json" => Language::Json,
            "yaml" | "yml" => Language::Yaml,
            "py" => Language::Python,
            "js" | "mjs" | "cjs" => Language::JavaScript,
            "ts" | "tsx" => Language::TypeScript,
            "html" | "htm" => Language::Html,
            "css" => Language::Css,
            "sh" | "bash" | "zsh" => Language::Shell,
            "c" | "h" => Language::C,
            "cpp" | "cc" | "cxx" | "hpp" => Language::Cpp,
            "go" => Language::Go,
            "v" | "vh" => Language::Verilog,
            "sv" | "svh" => Language::SystemVerilog,
            _ => Language::PlainText,
        }
    }

    /// The identity a language server is told a document is written in.
    #[must_use]
    pub fn server_id(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Toml => "toml",
            Language::Markdown => "markdown",
            Language::Json => "json",
            Language::Yaml => "yaml",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Html => "html",
            Language::Css => "css",
            Language::Shell => "shellscript",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Go => "go",
            Language::Verilog => "verilog",
            Language::SystemVerilog => "systemverilog",
            Language::PlainText => "plaintext",
        }
    }
}

/// How much a diagnostic matters, as a language server states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

/// The directory a file's project is rooted at: the folder the app was given, as the server
/// resolves a workspace.
#[expect(
    dead_code,
    reason = "the engine's own reading of a path; the app sends the project folder it opened"
)]
#[must_use]
pub fn root_of(path: &Path) -> &Path {
    path.parent().unwrap_or(path)
}

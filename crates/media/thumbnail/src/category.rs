//! Classifying a file by extension into a coarse category.
//!
//! Two jobs: decide **whether a rendered preview is available** (an image,
//! PDF, ebook or video can be rendered; a `.zip` cannot), and give a client a
//! stable category to draw a **type badge** for everything else. The
//! badge is drawn by the UI in the current theme — this crate only classifies,
//! it never bakes a colour or glyph in.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// A coarse file category, derived from the extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileCategory {
    /// A raster image the generator decodes and downscales.
    Image,
    /// A video whose representative keyframe the generator can extract.
    Video,
    /// An audio file (badge only — no visual frame).
    Audio,
    /// A PDF whose first page the generator can rasterise.
    Pdf,
    /// An EPUB whose embedded cover the generator can extract.
    Ebook,
    /// A word-processing / rich-text document (badge).
    Document,
    /// A spreadsheet (badge).
    Spreadsheet,
    /// A presentation / slide deck (badge).
    Presentation,
    /// A compressed archive (badge).
    Archive,
    /// Source code / markup (badge).
    Code,
    /// Plain text / notes (badge).
    Text,
    /// A font file (badge).
    Font,
    /// Anything unrecognised (badge).
    Unknown,
}

impl FileCategory {
    /// Whether the generator can produce a rendered preview for this category
    /// (as opposed to only a type badge). This is what the service
    /// checks before it bothers reading and decoding the source bytes.
    pub fn has_preview(self) -> bool {
        matches!(
            self,
            FileCategory::Image | FileCategory::Pdf | FileCategory::Ebook | FileCategory::Video
        )
    }

    /// A short, stable slug (used in tests and as a badge hint if a client
    /// wants one).
    pub fn slug(self) -> &'static str {
        match self {
            FileCategory::Image => "image",
            FileCategory::Video => "video",
            FileCategory::Audio => "audio",
            FileCategory::Pdf => "pdf",
            FileCategory::Ebook => "ebook",
            FileCategory::Document => "document",
            FileCategory::Spreadsheet => "spreadsheet",
            FileCategory::Presentation => "presentation",
            FileCategory::Archive => "archive",
            FileCategory::Code => "code",
            FileCategory::Text => "text",
            FileCategory::Font => "font",
            FileCategory::Unknown => "unknown",
        }
    }
}

/// Classify a lowercase-or-any-case extension (without the dot).
pub fn category_for_ext(ext: &str) -> FileCategory {
    let e = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    match e.as_str() {
        // Raster images the `image` crate decodes.
        "png" | "jpg" | "jpeg" | "jpe" | "gif" | "bmp" | "webp" | "tiff" | "tif" | "ico"
        | "avif" | "qoi" | "tga" | "pnm" | "ppm" | "pgm" | "pbm" | "dds" | "ff" | "hdr" => {
            FileCategory::Image
        }
        // Video containers.
        // (`.ts` is treated as TypeScript below — MPEG-TS is niche in a browser.)
        "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" | "wmv" | "flv" | "mpg" | "mpeg"
        | "m2ts" | "mts" | "ogv" | "3gp" => FileCategory::Video,
        // Audio.
        "mp3" | "flac" | "wav" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "aiff" | "aif"
        | "wma" | "mid" | "midi" => FileCategory::Audio,
        "pdf" => FileCategory::Pdf,
        "epub" => FileCategory::Ebook,
        // Word-processing / rich text.
        "doc" | "docx" | "odt" | "rtf" | "pages" | "abw" | "wpd" => FileCategory::Document,
        // Spreadsheets.
        "xls" | "xlsx" | "ods" | "csv" | "tsv" | "numbers" => FileCategory::Spreadsheet,
        // Presentations.
        "ppt" | "pptx" | "odp" | "key" => FileCategory::Presentation,
        // Archives.
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "lz" | "lzma"
        | "cab" | "ar" | "deb" | "rpm" => FileCategory::Archive,
        // Source code / markup.
        "rs" | "c" | "h" | "cpp" | "cc" | "hpp" | "cxx" | "py" | "js" | "ts" | "jsx" | "tsx"
        | "go" | "java" | "kt" | "rb" | "php" | "swift" | "sh" | "bash" | "fish" | "zsh"
        | "lua" | "pl" | "sql" | "html" | "htm" | "xml" | "css" | "scss" | "sass" | "json"
        | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "vue" | "svelte" | "dart"
        | "scala" | "clj" | "ex" | "exs" | "hs" | "ml" | "nim" | "zig" | "vim" | "el" => {
            FileCategory::Code
        }
        // Plain text / notes.
        "txt" | "md" | "markdown" | "log" | "text" | "org" | "rst" | "adoc" | "nfo" => {
            FileCategory::Text
        }
        // Fonts.
        "ttf" | "otf" | "woff" | "woff2" | "eot" | "pfb" => FileCategory::Font,
        _ => FileCategory::Unknown,
    }
}

/// Classify a path by its extension. A path with no extension is `Unknown`.
pub fn category_for_path(path: &Path) -> FileCategory {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => category_for_ext(ext),
        None => FileCategory::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_categories_decide_preview_or_badge() {
        // Phase `preview_types_are_classified_and_previewable`.
        {
            for (ext, cat) in [
                ("png", FileCategory::Image),
                ("JPG", FileCategory::Image),
                ("jpeg", FileCategory::Image),
                ("pdf", FileCategory::Pdf),
                ("epub", FileCategory::Ebook),
                ("mp4", FileCategory::Video),
                ("MKV", FileCategory::Video),
            ] {
                assert_eq!(category_for_ext(ext), cat, "{ext}");
                assert!(cat.has_preview(), "{ext} should be previewable");
            }
        }
        // Phase `badge_types_have_no_preview`.
        {
            for (ext, cat) in [
                ("zip", FileCategory::Archive),
                ("rs", FileCategory::Code),
                ("mp3", FileCategory::Audio),
                ("docx", FileCategory::Document),
                ("xlsx", FileCategory::Spreadsheet),
                ("pptx", FileCategory::Presentation),
                ("ttf", FileCategory::Font),
                ("md", FileCategory::Text),
                ("wat", FileCategory::Unknown),
            ] {
                assert_eq!(category_for_ext(ext), cat, "{ext}");
                assert!(!cat.has_preview(), "{ext} should be a badge, not a preview");
            }
        }
        // Phase `path_without_extension_is_unknown`.
        {
            assert_eq!(
                category_for_path(Path::new("/tmp/README")),
                FileCategory::Unknown
            );
            assert_eq!(
                category_for_path(Path::new("/a/b/photo.PNG")),
                FileCategory::Image
            );
        }
    }
}

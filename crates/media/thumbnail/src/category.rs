//! Classifying a file by extension into a coarse category: whether a rendered preview is
//! available, and a stable category for a type badge the UI draws in its own theme.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileCategory {
    Image,
    Video,
    /// An audio file (badge only — no visual frame).
    Audio,
    Pdf,
    Ebook,
    Document,
    Spreadsheet,
    Presentation,
    Archive,
    Code,
    Text,
    Font,
    Unknown,
}

impl FileCategory {
    /// Whether the generator can produce a rendered preview for this category, as opposed to
    /// only a type badge.
    pub fn has_preview(self) -> bool {
        matches!(
            self,
            FileCategory::Image | FileCategory::Pdf | FileCategory::Ebook | FileCategory::Video
        )
    }

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

pub fn category_for_ext(ext: &str) -> FileCategory {
    let e = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    match e.as_str() {
        "png" | "jpg" | "jpeg" | "jpe" | "gif" | "bmp" | "webp" | "tiff" | "tif" | "ico"
        | "avif" | "qoi" | "tga" | "pnm" | "ppm" | "pgm" | "pbm" | "dds" | "ff" | "hdr" => {
            FileCategory::Image
        }
        // Video containers.
        // (`.ts` is treated as TypeScript below — MPEG-TS is niche in a browser.)
        "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" | "wmv" | "flv" | "mpg" | "mpeg"
        | "m2ts" | "mts" | "ogv" | "3gp" => FileCategory::Video,
        "mp3" | "flac" | "wav" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "aiff" | "aif"
        | "wma" | "mid" | "midi" => FileCategory::Audio,
        "pdf" => FileCategory::Pdf,
        "epub" => FileCategory::Ebook,
        "doc" | "docx" | "odt" | "rtf" | "pages" | "abw" | "wpd" => FileCategory::Document,
        "xls" | "xlsx" | "ods" | "csv" | "tsv" | "numbers" => FileCategory::Spreadsheet,
        "ppt" | "pptx" | "odp" | "key" => FileCategory::Presentation,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "lz" | "lzma"
        | "cab" | "ar" | "deb" | "rpm" => FileCategory::Archive,
        "rs" | "c" | "h" | "cpp" | "cc" | "hpp" | "cxx" | "py" | "js" | "ts" | "jsx" | "tsx"
        | "go" | "java" | "kt" | "rb" | "php" | "swift" | "sh" | "bash" | "fish" | "zsh"
        | "lua" | "pl" | "sql" | "html" | "htm" | "xml" | "css" | "scss" | "sass" | "json"
        | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "vue" | "svelte" | "dart"
        | "scala" | "clj" | "ex" | "exs" | "hs" | "ml" | "nim" | "zig" | "vim" | "el" => {
            FileCategory::Code
        }
        "txt" | "md" | "markdown" | "log" | "text" | "org" | "rst" | "adoc" | "nfo" => {
            FileCategory::Text
        }
        "ttf" | "otf" | "woff" | "woff2" | "eot" | "pfb" => FileCategory::Font,
        _ => FileCategory::Unknown,
    }
}

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

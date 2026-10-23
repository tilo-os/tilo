//! The one taxonomy of file and link types: which category every type
//! belongs to.
//!
//! Settings groups default apps by it, and Files, search and the Store use
//! the same table, so a `.toml` file is "Code" everywhere and no surface
//! keeps a list of its own. The taxonomy is data ([`TAXONOMY`]): each
//! category names its extensions, MIME types, MIME prefixes and URI
//! schemes. A type the table does not name falls back on its MIME type
//! (`image/heic` is an image), and anything still unknown is
//! [`TypeCategory::Other`].

/// The category a type of file or link belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypeCategory {
    Documents,
    Spreadsheets,
    Presentations,
    Images,
    Video,
    Audio,
    Code,
    Archives,
    Web,
    Mail,
    CalendarContacts,
    Fonts,
    Ebooks,
    Other,
}

/// One category of [`TAXONOMY`]: its identity, how it is shown, and the
/// types it holds.
#[derive(Debug)]
pub struct CategoryEntry {
    pub category: TypeCategory,
    /// Stable id, as settings store a category's choice: `calendar-contacts`.
    pub slug: &'static str,
    /// English name, sentence case. Surfaces with a catalogue translate it
    /// by [`TypeCategory::slug`].
    pub label: &'static str,
    /// The design system's icon for it, by its Lucide name.
    pub icon: &'static str,
    /// File name extensions, lowercase, without the dot.
    pub extensions: &'static [&'static str],
    /// MIME types, lowercase, without parameters.
    pub mime_types: &'static [&'static str],
    /// MIME type prefixes every type under which belongs here unless a
    /// category names it ([`Self::mime_types`]): `image/`.
    pub mime_prefixes: &'static [&'static str],
    /// URI schemes, lowercase, without the colon.
    pub schemes: &'static [&'static str],
    /// The word that finds apps for the category in an app catalogue
    /// (the Store's search): `font`. Empty for a category no word finds.
    pub app_search: &'static str,
}

/// Every category, in the order a list of them reads when nothing else
/// orders it. [`TypeCategory::Other`] is last and names nothing: it is
/// where everything unnamed falls.
pub const TAXONOMY: &[CategoryEntry] = &[
    CategoryEntry {
        category: TypeCategory::Documents,
        slug: "documents",
        label: "Documents",
        icon: "file-text",
        extensions: &[
            "doc", "docx", "docm", "dot", "dotx", "odt", "ott", "fodt", "rtf", "txt", "text", "md",
            "markdown", "mdown", "pages", "wpd", "wps", "abw", "pdf", "xps", "oxps", "djvu", "tex",
            "typ", "org", "rst", "adoc", "asciidoc",
        ],
        mime_types: &[
            "application/pdf",
            "application/msword",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
            "application/vnd.ms-word.document.macroenabled.12",
            "application/vnd.oasis.opendocument.text",
            "application/vnd.oasis.opendocument.text-template",
            "application/vnd.apple.pages",
            "application/rtf",
            "application/x-abiword",
            "application/vnd.ms-xpsdocument",
            "application/oxps",
            "application/x-tex",
            "image/vnd.djvu",
            "text/plain",
            "text/rtf",
            "text/markdown",
            "text/x-markdown",
            "text/x-tex",
            "text/x-rst",
            "text/x-org",
            "text/asciidoc",
        ],
        mime_prefixes: &[],
        schemes: &[],
        app_search: "document",
    },
    CategoryEntry {
        category: TypeCategory::Spreadsheets,
        slug: "spreadsheets",
        label: "Spreadsheets",
        icon: "table",
        extensions: &[
            "xls", "xlsx", "xlsm", "xlsb", "xlt", "xltx", "ods", "ots", "fods", "csv", "tsv",
            "numbers",
        ],
        mime_types: &[
            "application/vnd.ms-excel",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.template",
            "application/vnd.ms-excel.sheet.macroenabled.12",
            "application/vnd.ms-excel.sheet.binary.macroenabled.12",
            "application/vnd.oasis.opendocument.spreadsheet",
            "application/vnd.oasis.opendocument.spreadsheet-template",
            "application/vnd.apple.numbers",
            "text/csv",
            "text/tab-separated-values",
        ],
        mime_prefixes: &[],
        schemes: &[],
        app_search: "spreadsheet",
    },
    CategoryEntry {
        category: TypeCategory::Presentations,
        slug: "presentations",
        label: "Presentations",
        icon: "bar-chart",
        extensions: &[
            "ppt", "pptx", "pptm", "pps", "ppsx", "pot", "potx", "odp", "otp", "fodp", "key",
        ],
        mime_types: &[
            "application/vnd.ms-powerpoint",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "application/vnd.openxmlformats-officedocument.presentationml.slideshow",
            "application/vnd.openxmlformats-officedocument.presentationml.template",
            "application/vnd.ms-powerpoint.presentation.macroenabled.12",
            "application/vnd.oasis.opendocument.presentation",
            "application/vnd.oasis.opendocument.presentation-template",
            "application/vnd.apple.keynote",
        ],
        mime_prefixes: &[],
        schemes: &[],
        app_search: "presentation",
    },
    CategoryEntry {
        category: TypeCategory::Images,
        slug: "images",
        label: "Images",
        icon: "image",
        extensions: &[
            "png", "jpg", "jpeg", "jpe", "jfif", "gif", "bmp", "webp", "tif", "tiff", "ico",
            "avif", "heic", "heif", "jxl", "svg", "svgz", "qoi", "tga", "pnm", "ppm", "pgm", "pbm",
            "dds", "hdr", "exr", "psd", "xcf", "kra", "ora", "raw", "cr2", "cr3", "nef", "arw",
            "dng", "orf", "rw2", "artwork",
        ],
        mime_types: &["application/vnd.tilo.artwork+zip"],
        mime_prefixes: &["image/"],
        schemes: &[],
        app_search: "image",
    },
    CategoryEntry {
        category: TypeCategory::Video,
        slug: "video",
        label: "Video",
        icon: "film",
        extensions: &[
            "mp4", "m4v", "mov", "webm", "mkv", "avi", "wmv", "flv", "mpg", "mpeg", "m2ts", "mts",
            "ogv", "3gp", "3g2", "vob",
        ],
        mime_types: &["application/x-matroska"],
        mime_prefixes: &["video/"],
        schemes: &[],
        app_search: "video",
    },
    CategoryEntry {
        category: TypeCategory::Audio,
        slug: "audio",
        label: "Audio",
        icon: "music",
        extensions: &[
            "mp3", "flac", "wav", "wave", "ogg", "oga", "opus", "m4a", "aac", "aif", "aiff", "wma",
            "mid", "midi", "ape", "alac", "m3u", "m3u8", "pls",
        ],
        mime_types: &["application/ogg", "application/x-mpegurl"],
        mime_prefixes: &["audio/"],
        schemes: &[],
        app_search: "music",
    },
    CategoryEntry {
        category: TypeCategory::Code,
        slug: "code",
        label: "Code",
        icon: "code",
        extensions: &[
            // Source files.
            "rs",
            "c",
            "h",
            "cpp",
            "cc",
            "cxx",
            "hpp",
            "hh",
            "hxx",
            "cs",
            "fs",
            "go",
            "java",
            "kt",
            "kts",
            "scala",
            "clj",
            "swift",
            "m",
            "mm",
            "dart",
            "py",
            "pyw",
            "pyi",
            "rb",
            "php",
            "pl",
            "pm",
            "lua",
            "r",
            "jl",
            "hs",
            "ml",
            "mli",
            "ex",
            "exs",
            "erl",
            "nim",
            "zig",
            "v",
            "vh",
            "sv",
            "svh",
            "vhd",
            "vhdl",
            "asm",
            "s",
            "el",
            "lisp",
            "scm",
            "vim",
            "sql",
            "proto",
            "graphql",
            "wat",
            // Web front ends.
            "js",
            "mjs",
            "cjs",
            "jsx",
            "ts",
            "tsx",
            "css",
            "scss",
            "sass",
            "less",
            "vue",
            "svelte",
            // Scripts.
            "sh",
            "bash",
            "zsh",
            "fish",
            "ps1",
            "bat",
            "cmd",
            "awk",
            // Data and configuration.
            "json",
            "jsonc",
            "json5",
            "ndjson",
            "toml",
            "yaml",
            "yml",
            "ini",
            "cfg",
            "conf",
            "env",
            "properties",
            "xml",
            "xsd",
            "xsl",
            "plist",
            "lock",
            "gradle",
            "cmake",
            "mk",
            "nix",
            "dockerfile",
            // Changes and logs.
            "diff",
            "patch",
            "log",
        ],
        mime_types: &[
            "application/json",
            "application/ld+json",
            "application/x-ndjson",
            "application/toml",
            "application/x-toml",
            "application/yaml",
            "application/x-yaml",
            "application/xml",
            "application/javascript",
            "application/x-javascript",
            "application/typescript",
            "application/x-sh",
            "application/x-shellscript",
            "application/x-csh",
            "application/sql",
            "application/x-sql",
            "application/x-httpd-php",
            "application/x-perl",
            "application/x-python",
            "application/x-ruby",
            "application/vnd.dart",
            "application/graphql",
            "application/x-patch",
        ],
        // Every other text type (`text/x-rust`, `text/css`) is source.
        mime_prefixes: &["text/"],
        schemes: &[],
        app_search: "code",
    },
    CategoryEntry {
        category: TypeCategory::Archives,
        slug: "archives",
        label: "Archives",
        icon: "archive",
        extensions: &[
            "zip", "tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz", "zst", "tzst", "lz",
            "lzma", "lz4", "7z", "rar", "cab", "ar", "cpio", "iso", "img", "dmg", "jar", "war",
            "deb", "rpm", "apk",
        ],
        mime_types: &[
            "application/zip",
            "application/x-zip-compressed",
            "application/x-tar",
            "application/gzip",
            "application/x-gzip",
            "application/x-compressed-tar",
            "application/x-bzip",
            "application/x-bzip2",
            "application/x-xz",
            "application/x-lzip",
            "application/x-lzma",
            "application/x-lz4",
            "application/zstd",
            "application/x-zstd",
            "application/x-7z-compressed",
            "application/x-rar-compressed",
            "application/vnd.rar",
            "application/vnd.ms-cab-compressed",
            "application/x-archive",
            "application/x-cpio",
            "application/x-iso9660-image",
            "application/x-apple-diskimage",
            "application/java-archive",
            "application/vnd.debian.binary-package",
            "application/x-rpm",
            "application/vnd.android.package-archive",
        ],
        mime_prefixes: &[],
        schemes: &[],
        app_search: "archive",
    },
    CategoryEntry {
        category: TypeCategory::Web,
        slug: "web",
        label: "Web",
        icon: "globe",
        extensions: &[
            "html", "htm", "xhtml", "xht", "shtml", "mht", "mhtml", "url", "webloc", "rss", "atom",
            "opml",
        ],
        mime_types: &[
            "text/html",
            "application/xhtml+xml",
            "multipart/related",
            "message/rfc822-html",
            "application/rss+xml",
            "application/atom+xml",
            "text/x-opml",
            "application/x-mswinurl",
        ],
        mime_prefixes: &[],
        schemes: &["http", "https", "web+search"],
        app_search: "browser",
    },
    CategoryEntry {
        category: TypeCategory::Mail,
        slug: "mail",
        label: "Mail",
        icon: "mail",
        extensions: &["eml", "emlx", "msg", "mbox"],
        mime_types: &["application/mbox", "application/vnd.ms-outlook"],
        mime_prefixes: &["message/"],
        schemes: &["mailto"],
        app_search: "mail",
    },
    CategoryEntry {
        category: TypeCategory::CalendarContacts,
        slug: "calendar-contacts",
        label: "Calendar & contacts",
        icon: "calendar",
        extensions: &["ics", "ical", "ifb", "vcs", "vcf", "vcard"],
        mime_types: &[
            "text/calendar",
            "text/x-vcalendar",
            "text/vcard",
            "text/x-vcard",
            "text/directory",
        ],
        mime_prefixes: &[],
        schemes: &["webcal", "webcals"],
        app_search: "calendar",
    },
    CategoryEntry {
        category: TypeCategory::Fonts,
        slug: "fonts",
        label: "Fonts",
        icon: "type",
        extensions: &[
            "ttf", "otf", "ttc", "otc", "woff", "woff2", "eot", "pfa", "pfb",
        ],
        mime_types: &["application/vnd.ms-fontobject"],
        mime_prefixes: &["font/", "application/font-", "application/x-font-"],
        schemes: &[],
        app_search: "font",
    },
    CategoryEntry {
        category: TypeCategory::Ebooks,
        slug: "ebooks",
        label: "Ebooks",
        icon: "bookmark",
        extensions: &[
            "epub", "mobi", "azw", "azw3", "kf8", "fb2", "cbz", "cbr", "cb7",
        ],
        mime_types: &[
            "application/epub+zip",
            "application/x-mobipocket-ebook",
            "application/vnd.amazon.ebook",
            "application/x-fictionbook+xml",
            "application/vnd.comicbook+zip",
            "application/vnd.comicbook-rar",
            "application/x-cbr",
            "application/x-cbz",
        ],
        mime_prefixes: &[],
        schemes: &[],
        app_search: "ebook",
    },
    CategoryEntry {
        category: TypeCategory::Other,
        slug: "other",
        label: "Other",
        icon: "file",
        extensions: &[],
        mime_types: &[],
        mime_prefixes: &[],
        schemes: &[],
        app_search: "",
    },
];

impl TypeCategory {
    /// Every category, in [`TAXONOMY`] order.
    pub const ALL: [Self; 14] = [
        Self::Documents,
        Self::Spreadsheets,
        Self::Presentations,
        Self::Images,
        Self::Video,
        Self::Audio,
        Self::Code,
        Self::Archives,
        Self::Web,
        Self::Mail,
        Self::CalendarContacts,
        Self::Fonts,
        Self::Ebooks,
        Self::Other,
    ];

    /// The category's entry in [`TAXONOMY`].
    #[must_use]
    pub fn entry(self) -> &'static CategoryEntry {
        // `TAXONOMY` lists `ALL` in order; the test below holds it to that.
        TAXONOMY
            .iter()
            .find(|entry| entry.category == self)
            .unwrap_or(&OTHER)
    }

    /// Stable id: `code`, `calendar-contacts`.
    #[must_use]
    pub fn slug(self) -> &'static str {
        self.entry().slug
    }

    /// The category a [`Self::slug`] names.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        TAXONOMY
            .iter()
            .find(|entry| entry.slug == slug)
            .map(|entry| entry.category)
    }

    /// English name, sentence case.
    #[must_use]
    pub fn label(self) -> &'static str {
        self.entry().label
    }

    /// The design system's icon for it, by its Lucide name.
    #[must_use]
    pub fn icon(self) -> &'static str {
        self.entry().icon
    }

    /// The word that finds apps for the category in an app catalogue,
    /// if one does.
    #[must_use]
    pub fn app_search(self) -> Option<&'static str> {
        Some(self.entry().app_search).filter(|word| !word.is_empty())
    }

    /// The category of files ending in `.extension` (without the dot; any
    /// case): the one that names it, else the one its MIME type falls in.
    #[must_use]
    pub fn of_extension(extension: &str) -> Self {
        let extension = extension
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        TAXONOMY
            .iter()
            .find(|entry| entry.extensions.contains(&extension.as_str()))
            .map(|entry| entry.category)
            .or_else(|| crate::from_ext(&extension).map(Self::of_mime))
            .unwrap_or(Self::Other)
    }

    /// The category of the MIME type `mime` (any case, parameters
    /// ignored): the one that names it, else the one whose prefix it has.
    #[must_use]
    pub fn of_mime(mime: &str) -> Self {
        let mime = mime
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        TAXONOMY
            .iter()
            .find(|entry| entry.mime_types.contains(&mime.as_str()))
            .or_else(|| {
                TAXONOMY.iter().find(|entry| {
                    entry
                        .mime_prefixes
                        .iter()
                        .any(|prefix| mime.starts_with(prefix))
                })
            })
            .map_or(Self::Other, |entry| entry.category)
    }

    /// The category of links with the URI scheme `scheme` (any case, with
    /// or without the colon).
    #[must_use]
    pub fn of_scheme(scheme: &str) -> Self {
        let scheme = scheme.trim().trim_end_matches(':').to_ascii_lowercase();
        TAXONOMY
            .iter()
            .find(|entry| entry.schemes.contains(&scheme.as_str()))
            .map_or(Self::Other, |entry| entry.category)
    }
}

const OTHER: CategoryEntry = CategoryEntry {
    category: TypeCategory::Other,
    slug: "other",
    label: "Other",
    icon: "file",
    extensions: &[],
    mime_types: &[],
    mime_prefixes: &[],
    schemes: &[],
    app_search: "",
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Every known MIME type and extension maps to exactly one category:
    /// Code covers source files, scripts, config formats and logs; unknown
    /// types fall into Other.
    #[test]
    fn every_type_maps_to_one_category() {
        // No extension, MIME type, prefix or scheme is named by two
        // categories, nor twice by one.
        let mut seen: BTreeMap<String, TypeCategory> = BTreeMap::new();
        for entry in TAXONOMY {
            let names = entry
                .extensions
                .iter()
                .map(|ext| format!("ext:{ext}"))
                .chain(entry.mime_types.iter().map(|mime| format!("mime:{mime}")))
                .chain(entry.mime_prefixes.iter().map(|p| format!("prefix:{p}")))
                .chain(
                    entry
                        .schemes
                        .iter()
                        .map(|scheme| format!("scheme:{scheme}")),
                );
            for name in names {
                let earlier = seen.insert(name.clone(), entry.category);
                assert!(
                    earlier.is_none(),
                    "{name} is named by {earlier:?} and {:?}",
                    entry.category
                );
            }
        }
        // No prefix of one category takes a type another category names.
        for entry in TAXONOMY {
            for prefix in entry.mime_prefixes {
                for other in TAXONOMY.iter().filter(|o| o.category != entry.category) {
                    for longer in other.mime_prefixes {
                        assert!(
                            !longer.starts_with(prefix),
                            "{prefix} of {:?} overlaps {longer} of {:?}",
                            entry.category,
                            other.category
                        );
                    }
                }
            }
        }
        // Every named type maps to the category that names it, whatever
        // its case, and every type the MIME table knows maps to one.
        for entry in TAXONOMY {
            for ext in entry.extensions {
                assert_eq!(TypeCategory::of_extension(ext), entry.category, "{ext}");
                assert_eq!(
                    TypeCategory::of_extension(&format!(".{}", ext.to_ascii_uppercase())),
                    entry.category,
                    "{ext} in capitals"
                );
            }
            for mime in entry.mime_types {
                assert_eq!(TypeCategory::of_mime(mime), entry.category, "{mime}");
            }
            for scheme in entry.schemes {
                assert_eq!(TypeCategory::of_scheme(scheme), entry.category, "{scheme}");
            }
        }
        for &(_, _, start, len) in &crate::table::EXTENSIONS {
            let ext = crate::string(start, len);
            let category = TypeCategory::of_extension(ext);
            assert!(TypeCategory::ALL.contains(&category), "{ext}");
        }
        // Code covers source, scripts, configuration formats and logs.
        for ext in ["rs", "py", "sh", "toml", "yaml", "yml", "json", "log"] {
            assert_eq!(TypeCategory::of_extension(ext), TypeCategory::Code, "{ext}");
        }
        // A type no category names falls back on its MIME type.
        assert_eq!(
            TypeCategory::of_mime("image/x-unheard-of"),
            TypeCategory::Images
        );
        assert_eq!(
            TypeCategory::of_mime("text/x-rust; charset=utf-8"),
            TypeCategory::Code
        );
        // Unknown types fall into Other.
        assert_eq!(
            TypeCategory::of_extension("no-such-type"),
            TypeCategory::Other
        );
        assert_eq!(TypeCategory::of_extension(""), TypeCategory::Other);
        assert_eq!(
            TypeCategory::of_mime("application/x-unheard-of"),
            TypeCategory::Other
        );
        assert_eq!(TypeCategory::of_scheme("tilo-clocks"), TypeCategory::Other);
    }

    #[test]
    fn the_taxonomy_lists_every_category_once_in_order() {
        let listed: Vec<TypeCategory> = TAXONOMY.iter().map(|entry| entry.category).collect();
        assert_eq!(listed, TypeCategory::ALL);
        for category in TypeCategory::ALL {
            assert_eq!(TypeCategory::from_slug(category.slug()), Some(category));
            assert!(!category.label().is_empty());
            assert!(!category.icon().is_empty());
        }
        assert_eq!(TypeCategory::Other.entry().extensions.len(), 0);
        for category in TypeCategory::ALL {
            assert_eq!(
                category.app_search().is_none(),
                category == TypeCategory::Other,
                "{category:?} has a word that finds its apps"
            );
        }
    }
}

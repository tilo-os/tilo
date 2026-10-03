//! Copy ratchet: the word and structure rules of `docs/design-language.md`
//! that can be seen in source.
//!
//! - No all-caps labels: section headers are sentence case in `Subheading`
//!   or `Heading`.
//! - No `A · B · C` metadata chains: use a secondary line, a `PropertyList`
//!   or columns.
//! - No `.to_uppercase()` on a displayed label.
//!
//! Counted per file in every app's production code
//! (`tests/baselines/copy/apps-<name>.txt`, see `support::ratchet`); the
//! counts may only fall.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use regex::Captures;

use crate::support::ratchet::{Pattern, Ratchet, no_exemptions};

/// Upper-case words that are names, not shouting: initialisms, file formats,
/// protocols, units and HTTP methods. A literal that starts with one of
/// these is not an all-caps label.
const ACRONYMS: &[&str] = &[
    // Formats and documents.
    "PDF", "HTML", "CSS", "JSON", "XML", "YAML", "TOML", "CSV", "TSV", "SQL", "EPUB", "ICS", "VCF",
    "SVG", "PNG", "JPG", "JPEG", "GIF", "WEBP", "HEIC", "TIFF", "BMP", "AVIF", "RAW", "EXIF",
    "IPTC", "MP3", "MP4", "AAC", "FLAC", "WAV", "OGG", "OPUS", "MKV", "AVI", "MOV", "WEBM", "MIDI",
    "ZIP", "TAR", "GZIP", "ISO", "IMG", "DMG", "APK", "DEB", "RPM", "OCI", "UTF", "ASCII", "MIME",
    "UUID", // Networks, protocols and security.
    "API", "URL", "URI", "HTTP", "HTTPS", "REST", "SSH", "FTP", "SFTP", "TLS", "SSL", "DNS", "VPN",
    "LAN", "WAN", "NAS", "TCP", "UDP", "ICMP", "DHCP", "NAT", "MTU", "MAC", "IPV4", "IPV6", "IMAP",
    "SMTP", "POP3", "LDAP", "RSS", "NFC", "GPS", "SMS", "MMS", "LTE", "SIM", "BLE", "TOTP", "HOTP",
    "OTP", "PIN", "AES", "RSA", "PGP", "GPG", "SHA", "MD5", "CRC", "DRM", "WASM",
    // HTTP methods, shown as protocol tokens in request builders.
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE", "CONNECT",
    // Hardware, storage and systems.
    "CPU", "GPU", "RAM", "HDMI", "DVI", "VGA", "OCR", "ROM", "SSD", "HDD", "USB", "HID", "PCI",
    "PCIE", "SATA", "NVME", "BIOS", "UEFI", "EFI", "TPM", "GPT", "MBR", "FAT", "FAT32", "EXT4",
    "NTFS", "EXFAT", "BTRFS", "XFS", "LUKS", "RAID", "LVM", "ACPI", "KVM", "QEMU", "SDK", "CLI",
    "IDE", "LSP", "LLM", "MCP", // Imaging, colour and media.
    "HDR", "SDR", "RGB", "RGBA", "HSL", "HSV", "CMYK", "HEX", "DPI", "PPI", "FPS", "BPM", "LUFS",
    // Time zones and standards.
    "UTC", "GMT", "CET", "CEST", "PST", "PDT", "EST", "EDT", "ETA", "FAQ", "RTL", "LTR",
];

/// The literal is a string (not a `b"…"` byte string) and its first word is
/// not an acronym.
fn not_an_acronym(c: &Captures<'_>) -> bool {
    &c[1] != "b" && !ACRONYMS.contains(&&c[2])
}

pub fn copy_ratchet() -> Ratchet {
    Ratchet {
        name: "copy",
        roots: &["apps/*/src"],
        patterns: vec![
            // A string literal that opens with an upper-case word of three or
            // more letters, on a line that hands text to something displayed
            // (a label, title, caption, heading, header, placeholder, tooltip
            // or button, a `Text`, or the draw/measure/elide text calls).
            // Group 1 is the character before the opening quote, so a byte
            // string can be told apart.
            Pattern::new(
                "all_caps_label",
                r#"(?:(?i:\w*(?:label|title|caption|heading|header|placeholder|tooltip|button|draw_text|measure|elide)\w*)|\bText\b)[^\n]*?([^\n])"([A-Z][A-Z0-9]{2,})\b[^"\n]*""#,
                "all-caps label — write it in sentence case and give it a Subheading or \
                 Heading role",
            )
            .keep(not_an_acronym),
            Pattern::new(
                "middle_dot_join",
                r#""[^"\n]*?(?: · |\\u\{(?:00)?[bB]7\})[^"\n]*""#,
                "`A · B` metadata chain — use a secondary line, a PropertyList or columns",
            ),
            Pattern::new(
                "to_uppercase",
                r"\.to_uppercase\(\)",
                "upper-casing displayed text — show it in sentence case; compare with \
                 eq_ignore_ascii_case or to_lowercase",
            ),
        ],
        exempt: no_exemptions,
        advice: "Sentence case everywhere, no all-caps labels and no `A · B · C` chains \
                 (docs/design-language.md, Words and structure).",
    }
}

/// Every key counts the shapes it names and nothing else.
#[test]
fn copy_patterns_match_exactly_the_shapes_they_name() {
    let ratchet = copy_ratchet();
    let cases: [(&str, &[&str], &[&str]); 3] = [
        (
            "all_caps_label",
            &[
                "Self::draw_section_label(scene, t, tr, theme, l.label, \"COLLECTIONS\");",
                "let (_, cap_h) = tr.measure(\"DESTINATIONS\", 10.5, None);",
                "let caption = \"RESPONSE HEADERS\";",
                ".with_heading(\"SCHEMA\")",
                "label(scene, tr, \"GROUPS (comma separated)\", r);",
                "let label = format!(\"REC  {}\", fmt_dur(elapsed));",
                "Text::new(\"NOTES\")",
                "label: \"EVENT · SAVES AUTOMATICALLY\",",
            ],
            &[
                "label: \"HDR\",",
                ".with_heading(\"API keys\")",
                "Text::new(\"PDFs\")",
                "label(scene, tr, \"Notes\", r);",
                "std::env::var(\"HOME\")",
                "title: \"TILO_DEMO\".into()",
                "Err(anyhow!(\"BAD HEADER\"))",
                "Instruction::addressed(\"SCRIBE: give this a title\", \"Scribe\")",
                "button(\"GET\")",
                "header.get(12..16)? != b\"IHDR\"",
                "status: args.text(\"status\").unwrap_or(\"NEEDS-ACTION\")",
            ],
        ),
        (
            "middle_dot_join",
            &[
                "format!(\"{artist} · {album}\")",
                "parts.join(\" · \")",
                "\"Inbox \\u{b7} 3 unread\"",
            ],
            &["\"Inbox\"", "'·'", "format!(\"{a}·{b}\")"],
        ),
        (
            "to_uppercase",
            &["name.to_uppercase()", "Text::new(label.to_uppercase())"],
            &["name.to_lowercase()", "c.to_ascii_uppercase()"],
        ),
    ];
    for (key, hits, misses) in cases {
        let pattern = ratchet.pattern(key);
        for line in hits {
            assert!(pattern.hits(line) > 0, "{key} must count {line:?}");
        }
        for line in misses {
            assert_eq!(pattern.hits(line), 0, "{key} must not count {line:?}");
        }
    }
}

/// All-caps labels, middle-dot chains and upper-cased labels may not grow.
#[test]
fn copy_rules_do_not_grow() {
    copy_ratchet().check();
}

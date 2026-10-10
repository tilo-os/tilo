//! Bounded real-file indexing for Photos.
//!
//! A directory import is an explicit local filesystem operation. It discovers
//! only supported image files beneath the selected root, reads actual EXIF
//! metadata where it exists, stages verified bytes in the content store, and
//! returns canonical `LibraryPhoto` values for the normal action boundary to
//! commit. It never invents dates, locations, labels, people, or descriptions.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_content::ContentStore;

use crate::model::{LibraryPhoto, LocationProvenance, OpenImage, PhotoLocation};
use crate::strings::tr;

/// Conservative limits for one explicit directory import. They prevent a
/// mounted photo archive or a malicious directory tree from monopolising the
/// client while still comfortably covering a few hundred normal photographs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportLimits {
    pub max_files: usize,
    pub max_depth: usize,
    pub max_encoded_bytes: u64,
}

impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_files: 2_000,
            max_depth: 12,
            max_encoded_bytes: 64 * 1024 * 1024,
        }
    }
}

/// User-visible options for a real import. EXIF GPS is retained only because
/// selecting this import is explicit; it is never reverse-geocoded, uploaded,
/// or inferred from network/location services.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportOptions {
    pub limits: ImportLimits,
    pub retain_exif_location: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            limits: ImportLimits::default(),
            retain_exif_location: true,
        }
    }
}

/// Incremental state suitable for a shared progress widget. `current` is a
/// local source path and must never be put in a resource, search index, or
/// collaborative document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportProgress {
    pub discovered: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub current: Option<PathBuf>,
}

/// Actual metadata read from a photo container. `orientation` records the
/// source container faithfully; the shared pixel decoder independently
/// normalises it so every thumbnail, analysis and export sees upright pixels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExifMetadata {
    pub captured_at: Option<String>,
    pub orientation: Option<u16>,
    pub location: Option<PhotoLocation>,
}

/// One successful staged import. The source path is local diagnostic data,
/// while `photo` is safe for the normal portable document action.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexedPhoto {
    pub source: PathBuf,
    pub photo: LibraryPhoto,
    pub exif: ExifMetadata,
}

/// A per-file failure that is intentionally non-fatal: an inaccessible or
/// malformed image must not make the remaining selected photos disappear.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportFailure {
    pub source: PathBuf,
    pub message: String,
    pub retryable: bool,
}

impl ImportFailure {
    fn new(source: PathBuf, message: String, retryable: bool) -> Self {
        Self {
            source,
            message,
            retryable,
        }
    }
}

/// Terminal result of a bounded import. `cancelled` is explicit so a UI can
/// distinguish a partial user cancellation from a successful short import.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportReport {
    pub photos: Vec<IndexedPhoto>,
    pub failures: Vec<ImportFailure>,
    pub discovered: usize,
    pub cancelled: bool,
    pub limit_reached: bool,
}

/// Index one explicitly selected directory. Progress runs on the caller's
/// worker thread; it is never emitted by the CRDT. Cancellation is checked
/// before descent, after discovery, and before each bounded file read.
///
/// # Errors
///
/// Returns a user-facing message when `root` is not an absolute path to an
/// accessible, real (non-symlink) directory. Per-item problems are reported
/// in [`ImportReport::failures`] instead.
#[expect(
    clippy::too_many_lines,
    reason = "the bounded walk keeps every refusal beside the check that makes it"
)]
pub fn index_directory<F>(
    root: &Path,
    store: &ContentStore,
    options: ImportOptions,
    cancelled: &AtomicBool,
    mut progress: F,
) -> Result<ImportReport, String>
where
    F: FnMut(ImportProgress),
{
    if !root.is_absolute() {
        return Err(tr("photos-import-folder-path"));
    }
    let unavailable = || tr("photos-import-folder-unavailable");
    let (parent, root_name) = tilo_files::file_at(root).map_err(|_| unavailable())?;
    let root_meta =
        tilo_files::blocking::metadata(&parent, &root_name).map_err(|_| unavailable())?;
    if root_meta.kind != tilo_files::Kind::Dir || root_meta.symlink {
        return Err(tr("photos-import-folder-link"));
    }
    let tree = tilo_files::Dir::open(tilo_files::Location::Native(root.to_path_buf()))
        .map_err(|_| unavailable())?;
    let mut report = ImportReport::default();
    let mut stack = vec![(tilo_files::VPath::root(), 0_usize)];
    while let Some((folder, depth)) = stack.pop() {
        if cancelled.load(Ordering::Acquire) {
            report.cancelled = true;
            break;
        }
        let directory = root.join(folder.as_str());
        let Ok(entries) = tilo_files::blocking::list(&tree, &folder) else {
            let failure =
                ImportFailure::new(directory, tr("photos-import-folder-unreadable"), true);
            report.failures.push(failure);
            continue;
        };
        for entry in entries {
            if cancelled.load(Ordering::Acquire) {
                report.cancelled = true;
                break;
            }
            let path = directory.join(&entry.name);
            let Ok(child) = tilo_files::VPath::new(&entry.name).map(|name| folder.join(&name))
            else {
                let message = tr("photos-import-item-unreadable");
                report
                    .failures
                    .push(ImportFailure::new(directory.clone(), message, true));
                continue;
            };
            let Ok(meta) = tilo_files::blocking::metadata(&tree, &child) else {
                let failure =
                    ImportFailure::new(path, tr("photos-import-item-uninspectable"), true);
                report.failures.push(failure);
                continue;
            };
            // Refusing links closes traversal loops and avoids silently
            // importing an unrelated tree through a selected directory.
            if meta.symlink {
                let message = tr("photos-import-link");
                report
                    .failures
                    .push(ImportFailure::new(path, message, false));
                continue;
            }
            if meta.kind == tilo_files::Kind::Dir {
                if depth < options.limits.max_depth {
                    stack.push((child, depth + 1));
                } else {
                    let message = tr("photos-import-too-deep");
                    report
                        .failures
                        .push(ImportFailure::new(path, message, false));
                }
                continue;
            }
            if meta.kind != tilo_files::Kind::File || !supported_image(&path) {
                continue;
            }
            if report.discovered >= options.limits.max_files {
                report.limit_reached = true;
                break;
            }
            report.discovered += 1;
            progress(ImportProgress {
                discovered: report.discovered,
                indexed: report.photos.len(),
                skipped: report.failures.len(),
                current: Some(path.clone()),
            });
            match index_file(&path, store, options, cancelled) {
                Ok(indexed) => report.photos.push(indexed),
                Err(error) if error == tr("photos-import-cancelled") => {
                    report.cancelled = true;
                    break;
                }
                Err(message) => report.failures.push(ImportFailure {
                    source: path,
                    message,
                    retryable: true,
                }),
            }
        }
        if report.cancelled || report.limit_reached {
            break;
        }
    }
    progress(ImportProgress {
        discovered: report.discovered,
        indexed: report.photos.len(),
        skipped: report.failures.len(),
        current: None,
    });
    Ok(report)
}

/// Index one real image file with the same byte and metadata bounds used by
/// directory import. The returned record can be dispatched as `ImportPhoto`.
///
/// # Errors
///
/// Returns a user-facing message when the import was cancelled (exactly
/// `"Import cancelled"`), or when the file cannot be read, exceeds the byte
/// bounds, cannot be decoded, has no usable name, or cannot be stored.
pub fn index_file(
    path: &Path,
    store: &ContentStore,
    options: ImportOptions,
    cancelled: &AtomicBool,
) -> Result<IndexedPhoto, String> {
    if cancelled.load(Ordering::Acquire) {
        return Err(tr("photos-import-cancelled"));
    }
    let (dir, name) = tilo_files::file_at(path).map_err(|_| tr("photos-import-unreadable"))?;
    let meta =
        tilo_files::blocking::metadata(&dir, &name).map_err(|_| tr("photos-import-unreadable"))?;
    if meta.kind != tilo_files::Kind::File
        || meta.len == 0
        || meta.len > options.limits.max_encoded_bytes
    {
        return Err(tr("photos-import-too-large"));
    }
    let exif = read_exif(path).unwrap_or_default();
    if cancelled.load(Ordering::Acquire) {
        return Err(tr("photos-import-cancelled"));
    }
    let bytes = read_bounded(path, options.limits.max_encoded_bytes)?;
    if cancelled.load(Ordering::Acquire) {
        return Err(tr("photos-import-cancelled"));
    }
    // Decode once before staging. Header dimensions alone are not a promise
    // that the complete payload is a safe image.
    let decoded = crate::pixels::decode(&bytes).ok_or_else(|| tr("photos-import-unsafe"))?;
    let content = store.put(&bytes).map_err(|error| {
        tracing::warn!(%error, "photos: could not store an imported original");
        tr("photos-import-not-kept")
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| tr("photos-import-no-name"))?;
    let open = OpenImage::from_path(&path.to_string_lossy(), decoded.w, decoded.h)
        .with_content(content)
        .portable();
    let mut photo = LibraryPhoto::from_image(open);
    name.clone_into(&mut photo.image.name);
    photo.captured_at = exif
        .captured_at
        .as_deref()
        .and_then(crate::actions::canonical_capture_at_value);
    if options.retain_exif_location {
        photo.location.clone_from(&exif.location);
    }
    Ok(IndexedPhoto {
        source: path.to_path_buf(),
        photo,
        exif,
    })
}

/// The most of a file EXIF is read from; photographs past it have no EXIF.
const EXIF_READ_LIMIT: u64 = 256 * 1024 * 1024;

/// Read actual EXIF from a bounded-size image. Parsing failure is intentionally
/// non-fatal: many valid PNG/WebP files simply have no EXIF container.
#[must_use]
pub fn read_exif(path: &Path) -> Option<ExifMetadata> {
    let bytes = crate::files::read_native(path, EXIF_READ_LIMIT)?;
    let mut reader = Cursor::new(bytes);
    let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let captured_at = [
        exif::Tag::DateTimeOriginal,
        exif::Tag::DateTimeDigitized,
        exif::Tag::DateTime,
    ]
    .iter()
    .find_map(|tag| ascii_field(exif.get_field(*tag, exif::In::PRIMARY)?));
    let orientation = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(short_field)
        .filter(|value| (1..=8).contains(value));
    let location = match (
        gps_coordinate(&exif, exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef),
        gps_coordinate(&exif, exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef),
    ) {
        (Some(latitude), Some(longitude)) => Some(PhotoLocation {
            latitude,
            longitude,
            label: None,
            provenance: LocationProvenance {
                source: "exif".to_owned(),
                scope: "shared".to_owned(),
            },
        }),
        _ => None,
    };
    Some(ExifMetadata {
        captured_at,
        orientation,
        location,
    })
}

fn gps_coordinate(exif: &exif::Exif, value_tag: exif::Tag, ref_tag: exif::Tag) -> Option<f64> {
    let field = exif.get_field(value_tag, exif::In::PRIMARY)?;
    let exif::Value::Rational(values) = &field.value else {
        return None;
    };
    let [degrees, minutes, seconds, ..] = values.as_slice() else {
        return None;
    };
    if degrees.denom == 0 || minutes.denom == 0 || seconds.denom == 0 {
        return None;
    }
    let magnitude = f64::from(degrees.num) / f64::from(degrees.denom)
        + f64::from(minutes.num) / f64::from(minutes.denom) / 60.0
        + f64::from(seconds.num) / f64::from(seconds.denom) / 3600.0;
    let reference = ascii_field(exif.get_field(ref_tag, exif::In::PRIMARY)?)?;
    let signed = match reference.as_str() {
        "N" | "E" => magnitude,
        "S" | "W" => -magnitude,
        _ => return None,
    };
    signed.is_finite().then_some(signed)
}

fn ascii_field(field: &exif::Field) -> Option<String> {
    let exif::Value::Ascii(values) = &field.value else {
        return None;
    };
    let value = values.first()?;
    let text = std::str::from_utf8(value)
        .ok()?
        .trim_matches(char::from(0))
        .trim();
    (!text.is_empty()).then_some(text.to_owned())
}

fn short_field(field: &exif::Field) -> Option<u16> {
    let exif::Value::Short(values) = &field.value else {
        return None;
    };
    values.first().copied()
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    crate::files::read_native(path, limit).ok_or_else(|| tr("photos-import-unreadable-or-large"))
}

fn supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "webp"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn jpeg_with_orientation(orientation: u16) -> Vec<u8> {
        use image::ImageEncoder as _;

        let rgb = vec![
            255, 0, 0, 0, 255, 0, 0, 0, 255, // first row
            255, 255, 0, 0, 255, 255, 255, 0, 255, // second row
        ];
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
            .write_image(&rgb, 3, 2, image::ExtendedColorType::Rgb8)
            .expect("encode test JPEG");
        let mut payload = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0".to_vec();
        payload.extend_from_slice(&[
            0x12,
            0x01,
            0x03,
            0x00,
            0x01,
            0x00,
            0x00,
            0x00,
            orientation as u8,
            (orientation >> 8) as u8,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
        ]);
        let segment_length = u16::try_from(payload.len() + 2).expect("small APP1 segment");
        let mut tagged = Vec::with_capacity(jpeg.len() + payload.len() + 4);
        tagged.extend_from_slice(&jpeg[..2]);
        tagged.extend_from_slice(&[0xff, 0xe1]);
        tagged.extend_from_slice(&segment_length.to_be_bytes());
        tagged.extend_from_slice(&payload);
        tagged.extend_from_slice(&jpeg[2..]);
        tagged
    }

    fn png(path: &Path) {
        image::save_buffer_with_format(
            path,
            &[8, 20, 30, 255, 40, 50, 60, 255],
            2,
            1,
            image::ExtendedColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .unwrap();
    }

    #[test]
    fn directory_import_is_bounded_cancellable_and_refuses_unsafe_roots() {
        eprintln!("phase directory_import_is_bounded_real_and_reports_progress");
        {
            let directory = tempfile::tempdir().unwrap();
            png(&directory.path().join("one.png"));
            png(&directory.path().join("two.png"));
            fs::write(directory.path().join("ignore.txt"), "not an image").unwrap();
            let cancelled = AtomicBool::new(false);
            let mut progress = Vec::new();
            let report = index_directory(
                directory.path(),
                &ContentStore::memory(),
                ImportOptions {
                    limits: ImportLimits {
                        max_files: 1,
                        ..ImportLimits::default()
                    },
                    ..ImportOptions::default()
                },
                &cancelled,
                |value| progress.push(value),
            )
            .unwrap();
            assert_eq!(report.discovered, 1);
            assert_eq!(report.photos.len(), 1);
            assert!(report.limit_reached);
            assert!(report.photos[0].photo.image.content.is_some());
            assert!(progress.last().is_some_and(|value| value.current.is_none()));
        }

        eprintln!("phase cancelled_import_does_not_open_or_stage_a_file");
        {
            let directory = tempfile::tempdir().unwrap();
            png(&directory.path().join("one.png"));
            let cancelled = AtomicBool::new(true);
            let report = index_directory(
                directory.path(),
                &ContentStore::memory(),
                ImportOptions::default(),
                &cancelled,
                |_| {},
            )
            .unwrap();
            assert!(report.cancelled);
            assert!(report.photos.is_empty());
            assert_eq!(report.discovered, 0);
        }

        eprintln!("phase relative_and_symlink_roots_are_refused");
        {
            let cancelled = AtomicBool::new(false);
            assert_eq!(
                index_directory(
                    Path::new("relative/photos"),
                    &ContentStore::memory(),
                    ImportOptions::default(),
                    &cancelled,
                    |_| {},
                )
                .unwrap_err(),
                tr("photos-import-folder-path")
            );

            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;
                let parent = tempfile::tempdir().unwrap();
                let real = parent.path().join("real");
                fs::create_dir(&real).unwrap();
                let linked = parent.path().join("linked");
                symlink(&real, &linked).unwrap();
                assert_eq!(
                    index_directory(
                        &linked,
                        &ContentStore::memory(),
                        ImportOptions::default(),
                        &cancelled,
                        |_| {},
                    )
                    .unwrap_err(),
                    tr("photos-import-folder-link")
                );
            }
        }
    }

    #[test]
    fn imports_keep_source_bytes_with_real_exif_upright_dimensions() {
        eprintln!("phase exif_reader_has_no_synthetic_fallback_for_a_plain_png");
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("plain.png");
            png(&path);
            assert!(read_exif(&path).is_none());
        }

        eprintln!("phase import_keeps_addressed_source_bytes_but_records_upright_dimensions");
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("portrait.jpg");
            let source = jpeg_with_orientation(6);
            fs::write(&path, &source).unwrap();

            let store = ContentStore::memory();
            let indexed = index_file(
                &path,
                &store,
                ImportOptions::default(),
                &AtomicBool::new(false),
            )
            .expect("index oriented JPEG");

            assert_eq!(indexed.exif.orientation, Some(6));
            assert_eq!(
                (indexed.photo.image.width, indexed.photo.image.height),
                (2, 3)
            );
            let hash = indexed
                .photo
                .image
                .content
                .as_deref()
                .expect("portable content address");
            assert_eq!(store.read(hash).as_deref(), Some(source.as_slice()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn partial_failures_are_retained_without_aborting_valid_photos() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let good = directory.path().join("good.png");
        png(&good);
        fs::write(directory.path().join("broken.png"), b"not a photograph").unwrap();
        symlink(&good, directory.path().join("linked.png")).unwrap();
        let report = index_directory(
            directory.path(),
            &ContentStore::memory(),
            ImportOptions::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();

        assert_eq!(report.photos.len(), 1);
        assert_eq!(report.photos[0].source.file_name().unwrap(), "good.png");
        assert!(report.failures.iter().any(|failure| {
            failure.source.ends_with("broken.png")
                && failure.message.contains("cannot be decoded")
                && failure.retryable
        }));
        assert!(report.failures.iter().any(|failure| {
            failure.source.ends_with("linked.png")
                && failure.message == crate::strings::tr("photos-import-link")
                && !failure.retryable
        }));
    }
}

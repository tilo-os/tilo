//! Whole-cohort capacity admission before the first VM upload.
//! Private temporary archives retain the existing framed END verification.
//! Estimates include rounded payload, per-entry metadata, and peak upload;
//! concurrent writers can still consume capacity after this observation.
use std::fs::File;
use std::io::{Read, Seek as _, SeekFrom};
use std::path::Path;

use crate::error::CliError;
use serde::Serialize;

const MAX_ARCHIVE: u64 = 4 << 30;
const MAX_ENTRIES: usize = 262_144;
const ENTRY_OVERHEAD: u64 = 16 << 10;

pub(in crate::app) struct Archive {
    pub file: File,
    bytes: u64,
    unpack_bytes: u64,
}

fn invalid(message: &str) -> CliError {
    CliError::InvalidInput(format!("userspace capacity preflight: {message}"))
}

impl Archive {
    /// Time reception, END verification and local capacity scanning together.
    /// The phase is not a pure network measurement. Refused streams record
    /// elapsed work but never publish a successful archive byte count.
    pub(in crate::app) fn receive_tracked(
        reader: &mut impl Read,
        directory: &Path,
        receipt: &mut crate::receipt::Receipt,
        component: &str,
    ) -> Result<Self, CliError> {
        let archive = receipt.phase(&format!("receive-{component}"), || {
            Self::receive(reader, directory)
        })?;
        receipt.set(
            &format!("cohort_archive-{component}"),
            serde_json::json!({
                "bytes": archive.bytes,
                "unpack_estimate_bytes": archive.unpack_bytes,
            }),
        );
        Ok(archive)
    }

    pub(in crate::app) fn receive(
        reader: &mut impl Read,
        directory: &Path,
    ) -> Result<Self, CliError> {
        let mut file = tempfile::tempfile_in(directory)
            .map_err(CliError::io("create private cohort archive"))?;
        let bytes = std::io::copy(&mut reader.take(MAX_ARCHIVE + 1), &mut file)
            .map_err(CliError::io("receive complete cohort archive"))?;
        if bytes == 0 || bytes > MAX_ARCHIVE {
            return Err(invalid("archive is empty or exceeds 4 GiB"));
        }
        file.rewind()
            .map_err(CliError::io("rewind cohort archive"))?;
        let mut unpack_bytes = 1_u64 << 20;
        let mut archive = tar::Archive::new(&mut file);
        for (index, entry) in archive
            .entries()
            .map_err(CliError::io("inspect cohort archive"))?
            .enumerate()
        {
            if index >= MAX_ENTRIES {
                return Err(invalid("archive entry count exceeds its bound"));
            }
            let entry = entry.map_err(CliError::io("inspect cohort archive entry"))?;
            let kind = entry.header().entry_type();
            if !kind.is_file() && !kind.is_dir() && !kind.is_symlink() && !kind.is_hard_link() {
                return Err(invalid("archive has unsupported storage semantics"));
            }
            let rounded = entry
                .size()
                .checked_add(4095)
                .map(|size| size & !4095)
                .ok_or_else(|| invalid("entry storage estimate overflow"))?;
            unpack_bytes = unpack_bytes
                .checked_add(rounded)
                .and_then(|size| size.checked_add(ENTRY_OVERHEAD))
                .ok_or_else(|| invalid("archive storage estimate overflow"))?;
        }
        file.seek(SeekFrom::Start(0))
            .map_err(CliError::io("rewind verified cohort archive"))?;
        Ok(Self {
            file,
            bytes,
            unpack_bytes,
        })
    }
}

#[derive(Debug, Serialize)]
pub(in crate::app) struct Budget {
    #[serde(rename = "reserve_bytes")]
    reserve: u64,
    #[serde(rename = "unpack_estimate_bytes")]
    unpack_estimate: u64,
    #[serde(rename = "peak_upload_bytes")]
    peak_upload: u64,
    #[serde(rename = "required_free_bytes")]
    required_free: u64,
}

impl Budget {
    pub(in crate::app) fn new<'a>(
        archives: impl Iterator<Item = &'a Archive>,
        reserve: u64,
    ) -> Result<Self, CliError> {
        let mut unpack_estimate = 0_u64;
        let mut peak_upload = 0_u64;
        for archive in archives {
            unpack_estimate = unpack_estimate
                .checked_add(archive.unpack_bytes)
                .ok_or_else(|| invalid("cohort storage estimate overflow"))?;
            peak_upload = peak_upload.max(archive.bytes);
        }
        let required_free = reserve
            .checked_add(unpack_estimate)
            .and_then(|size| size.checked_add(peak_upload))
            .ok_or_else(|| invalid("cohort capacity requirement overflow"))?;
        Ok(Self {
            reserve,
            unpack_estimate,
            peak_upload,
            required_free,
        })
    }

    pub(in crate::app) fn admit(&self, free_bytes: Option<u64>) -> Result<(), CliError> {
        let free = free_bytes.ok_or_else(|| {
            invalid("VM available capacity cannot be observed; no layers transferred")
        })?;
        if free < self.required_free {
            let shortfall = self
                .required_free
                .checked_sub(free)
                .ok_or_else(|| invalid("capacity shortfall overflow"))?;
            return Err(invalid(&format!(
                "whole cohort needs {} bytes free ({} reserve, {} unpack estimate, {} peak upload); VM has {free}, shortfall {}; no layers transferred. Run tilo-vm gc --dry-run, then tilo-vm gc; see tilo-vm guide storage",
                self.required_free, self.reserve, self.unpack_estimate, self.peak_upload, shortfall
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builderlayer::{Lines, Packed, Stream};
    use std::io::Write as _;

    fn framed(payload: &[u8]) -> Vec<u8> {
        let mut tree = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tree.append_data(&mut header, "usr/bin/app", payload)
            .unwrap();
        let archive = tree.into_inner().unwrap();
        let mut output = Vec::new();
        writeln!(
            output,
            "{}{}",
            crate::builderlayer::BEGIN,
            serde_json::to_string(&Packed::default()).unwrap()
        )
        .unwrap();
        let mut lines = Lines::new(&mut output);
        lines.write_all(&archive).unwrap();
        let (hash, size) = lines.finish().unwrap();
        writeln!(output, "{}{hash} {size}", crate::builderlayer::END).unwrap();
        output
    }

    fn receive(bytes: &[u8]) -> Result<Archive, CliError> {
        let mut stream = Stream::new(bytes);
        stream.header(&mut Vec::new()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        Archive::receive(&mut stream, directory.path())
    }

    #[test]
    fn whole_cohort_space_includes_all_unpacks_peak_upload_and_unchanged_reserve() {
        let first = receive(&framed(b"first")).unwrap();
        let second = receive(&framed(&vec![7; 9000])).unwrap();
        let floor = 5 << 30;
        let budget = Budget::new([&first, &second].into_iter(), floor).unwrap();
        assert_eq!(budget.reserve, floor);
        assert_eq!(
            budget.unpack_estimate,
            (2 << 20) + 4096 + 12_288 + 2 * ENTRY_OVERHEAD
        );
        assert_eq!(budget.peak_upload, second.bytes);
        assert_eq!(
            budget.required_free,
            floor + budget.unpack_estimate + second.bytes
        );
        assert!(budget.admit(Some(budget.required_free)).is_ok());
        assert!(budget.admit(Some(budget.required_free - 1)).is_err());
        assert!(
            budget.admit(Some(floor)).is_err(),
            "per-member floor alone is insufficient"
        );
        assert!(
            budget.admit(None).is_err(),
            "unobservable capacity cannot pass"
        );
    }

    #[test]
    fn complete_verified_archives_are_rewound_and_corrupted_end_is_refused() {
        let bytes = framed(b"owned source bytes");
        let mut archive = receive(&bytes).unwrap();
        let mut tree = tar::Archive::new(&mut archive.file);
        let mut entries = tree.entries().unwrap();
        let mut entry = entries.next().unwrap().unwrap();
        let mut payload = Vec::new();
        entry.read_to_end(&mut payload).unwrap();
        assert_eq!(payload, b"owned source bytes");
        let damaged = String::from_utf8(bytes).unwrap().replace(
            crate::builderlayer::END,
            &format!("{}bad", crate::builderlayer::END),
        );
        assert!(receive(damaged.as_bytes()).is_err());
    }

    #[test]
    fn capacity_sum_overflow_refuses_instead_of_wrapping_to_admission() {
        let archive = Archive {
            file: tempfile::tempfile().unwrap(),
            bytes: 1,
            unpack_bytes: u64::MAX,
        };
        assert!(Budget::new([&archive].into_iter(), 1).is_err());
    }

    #[test]
    fn tracked_archive_receipt_includes_real_receive_work_and_verified_bytes() {
        struct Delayed<'a> {
            reader: Stream<&'a [u8]>,
            delayed: bool,
        }
        impl Read for Delayed<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                if !self.delayed {
                    self.delayed = true;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                self.reader.read(bytes)
            }
        }
        let bytes = framed(b"verified receipt payload");
        let mut stream = Stream::new(bytes.as_slice());
        stream.header(&mut Vec::new()).unwrap();
        let mut reader = Delayed {
            reader: stream,
            delayed: false,
        };
        let directory = tempfile::tempdir().unwrap();
        let mut receipt = crate::receipt::Receipt::new(directory.path(), vec![]);
        let mut archive =
            Archive::receive_tracked(&mut reader, directory.path(), &mut receipt, "apps+runtime")
                .unwrap();
        let mut payload = Vec::new();
        tar::Archive::new(&mut archive.file)
            .entries()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .read_to_end(&mut payload)
            .unwrap();
        assert_eq!(payload, b"verified receipt payload");
        let path = receipt.finish(Ok(&serde_json::json!({}))).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(value["phases"][0]["name"], "receive-apps+runtime");
        assert!(value["phases"][0]["seconds"].as_f64().unwrap() >= 0.019);
        assert_eq!(value["cohort_archive-apps+runtime"]["bytes"], archive.bytes);
        assert_eq!(
            value["cohort_archive-apps+runtime"]["unpack_estimate_bytes"],
            archive.unpack_bytes
        );
    }

    #[test]
    fn tracked_archive_failure_records_elapsed_without_claiming_verified_bytes() {
        let bytes = String::from_utf8(framed(b"must reject")).unwrap().replace(
            crate::builderlayer::END,
            &format!("{}bad", crate::builderlayer::END),
        );
        let mut stream = Stream::new(bytes.as_bytes());
        stream.header(&mut Vec::new()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let mut receipt = crate::receipt::Receipt::new(directory.path(), vec![]);
        assert!(
            Archive::receive_tracked(&mut stream, directory.path(), &mut receipt, "shell",)
                .is_err()
        );
        let path = receipt.finish(Err(&serde_json::json!({}))).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(value["status"], "failure");
        assert_eq!(value["phases"][0]["name"], "receive-shell");
        assert!(value.get("cohort_archive-shell").is_none());
    }
}

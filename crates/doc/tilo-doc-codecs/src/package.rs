//! The ZIP package every office format is: OOXML (`.docx`, `.xlsx`) and ODF (`.odt`, `.ods`).
//! Untrusted bytes enter only through [`ZipPackage::read`], so the zip-bomb, entry-count and
//! DTD/XXE guards apply to every format. Parts keep their original order and bytes.

use crate::format::CodecError;
use crate::safety::{ZipLimits, read_zip_parts};
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

pub struct ZipPackage {
    /// Parts in original zip order: `(name, bytes)`.
    pub parts: Vec<(String, Vec<u8>)>,
}

impl ZipPackage {
    pub fn read(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(Self {
            parts: read_zip_parts(bytes, &ZipLimits::default())?,
        })
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_slice())
    }

    pub fn set(&mut self, name: &str, bytes: Vec<u8>) {
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == name) {
            p.1 = bytes;
        } else {
            self.parts.push((name.to_string(), bytes));
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.parts.retain(|(n, _)| n != name);
    }

    /// Serialise an OOXML package: every part deflated, in order.
    pub fn write_opc(&self) -> Result<Vec<u8>, CodecError> {
        self.write(None)
    }

    /// Serialise an ODF package: ODF requires `mimetype` to be the first entry
    /// and stored uncompressed; the rest are deflated, in order.
    pub fn write_odf(&self) -> Result<Vec<u8>, CodecError> {
        self.write(Some("mimetype"))
    }

    fn write(&self, stored_first: Option<&str>) -> Result<Vec<u8>, CodecError> {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let werr = |e: zip::result::ZipError| CodecError::Export(format!("zip write: {e}"));
        let ierr = |e: std::io::Error| CodecError::Export(format!("zip io: {e}"));
        if let Some(first) = stored_first
            && let Some(bytes) = self.get(first)
        {
            zw.start_file(first, stored).map_err(werr)?;
            zw.write_all(bytes).map_err(ierr)?;
        }
        for (name, bytes) in &self.parts {
            if Some(name.as_str()) == stored_first {
                continue;
            }
            zw.start_file(name.clone(), deflated).map_err(werr)?;
            zw.write_all(bytes).map_err(ierr)?;
        }
        Ok(zw.finish().map_err(werr)?.into_inner())
    }
}

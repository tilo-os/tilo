//! The ZIP package every office format is: OOXML (`.docx`, `.xlsx`) and ODF
//! (`.odt`, `.ods`). Untrusted bytes enter only through [`ZipPackage::read`],
//! so the same zip-bomb, entry-count and DTD/XXE guards ([`read_zip_parts`])
//! apply to every format. Parts keep their original order and bytes, so parts
//! a codec doesn't model are carried through an export verbatim.

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
    /// Read an untrusted package under the default [`ZipLimits`].
    ///
    /// # Errors
    /// Returns [`CodecError`] if `bytes` are not a readable zip archive, if the
    /// archive exceeds [`ZipLimits`], or if an XML part declares a DTD.
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

    /// Replace an existing part (preserving its position) or append a new one.
    pub fn set(&mut self, name: &str, bytes: Vec<u8>) {
        if let Some(p) = self.parts.iter_mut().find(|(n, _)| n == name) {
            p.1 = bytes;
        } else {
            self.parts.push((name.to_string(), bytes));
        }
    }

    /// Remove a part, if present.
    pub fn remove(&mut self, name: &str) {
        self.parts.retain(|(n, _)| n != name);
    }

    /// Serialise an OOXML package: every part deflated, in order.
    ///
    /// # Errors
    /// Returns [`CodecError`] if the zip writer rejects a part name or fails to
    /// write its bytes.
    pub fn write_opc(&self) -> Result<Vec<u8>, CodecError> {
        self.write(None)
    }

    /// Serialise an ODF package: ODF requires `mimetype` to be the first entry
    /// and stored uncompressed; the rest are deflated, in order.
    ///
    /// # Errors
    /// Returns [`CodecError`] if the zip writer rejects a part name or fails to
    /// write its bytes.
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

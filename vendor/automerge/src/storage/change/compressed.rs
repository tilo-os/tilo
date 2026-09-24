use std::{borrow::Cow, io::Read};

use crate::storage::{Change, CheckSum, ChunkType, MAGIC_BYTES};

use super::OpReadState;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Compressed<'a> {
    checksum: CheckSum,
    bytes: Cow<'a, [u8]>,
}

impl<'a> Compressed<'a> {
    pub(crate) fn new(checksum: CheckSum, bytes: Cow<'a, [u8]>) -> Self {
        Self { checksum, bytes }
    }

    pub(crate) fn compress<'b, O: OpReadState>(change: &'b Change<'b, O>) -> Compressed<'static> {
        let mut result = Vec::with_capacity(change.bytes().len());
        result.extend(MAGIC_BYTES);
        result.extend(change.checksum().bytes());
        result.push(u8::from(ChunkType::Compressed));
        let mut deflater = flate2::bufread::DeflateEncoder::new(
            change.body_bytes(),
            flate2::Compression::default(),
        );
        let mut deflated = Vec::new();
        let deflated_len = deflater.read_to_end(&mut deflated).unwrap();
        leb128::write::unsigned(&mut result, deflated_len as u64).unwrap();
        result.extend(&deflated[..]);
        Compressed {
            checksum: change.checksum(),
            bytes: Cow::Owned(result),
        }
    }

    pub(crate) fn bytes(&self) -> Cow<'a, [u8]> {
        self.bytes.clone()
    }

    pub(crate) fn checksum(&self) -> CheckSum {
        self.checksum
    }

    pub(crate) fn into_owned(self) -> Compressed<'static> {
        Compressed {
            checksum: self.checksum,
            bytes: Cow::Owned(self.bytes.into_owned()),
        }
    }
}

// Native allocation traversal is read-only and does not populate caches.
use crate::resource_limits::retained::{NativeVisit, Visitor};
impl NativeVisit for Compressed<'_> {
    fn native_visit(
        &self,
        visitor: &mut Visitor<'_>,
    ) -> Result<(), crate::resource_limits::NativeResourceError> {
        let Self { checksum: _, bytes } = self;
        bytes.native_visit(visitor)
    }
}

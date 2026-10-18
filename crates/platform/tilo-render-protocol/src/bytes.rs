//! Little-endian reading and writing of the wire formats, with every length
//! and offset checked. The reader never panics: running past the end, an
//! overflowing count or a misaligned section is a [`WireError`].

use crate::WireError;

/// Reads a hostile byte string front to back.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        let (head, tail) = self
            .bytes
            .split_at_checked(len)
            .ok_or(WireError::Truncated)?;
        self.bytes = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let bytes = self.take(N)?;
        <[u8; N]>::try_from(bytes).map_err(|_| WireError::Truncated)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, WireError> {
        Ok(u8::from_le_bytes(self.array()?))
    }

    pub(crate) fn u16(&mut self) -> Result<u16, WireError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub(crate) fn i16(&mut self) -> Result<i16, WireError> {
        Ok(i16::from_le_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, WireError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn i32(&mut self) -> Result<i32, WireError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, WireError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub(crate) fn f32(&mut self) -> Result<f32, WireError> {
        Ok(f32::from_le_bytes(self.array()?))
    }

    pub(crate) fn f64(&mut self) -> Result<f64, WireError> {
        Ok(f64::from_le_bytes(self.array()?))
    }

    /// A `u32` count that has to fit the bytes left at `item_bytes` each, so
    /// a hostile count never reserves memory the record does not carry.
    pub(crate) fn count(&mut self, item_bytes: usize) -> Result<usize, WireError> {
        let count = usize::try_from(self.u32()?).map_err(|_| WireError::Overflow)?;
        let needed = count.checked_mul(item_bytes).ok_or(WireError::Overflow)?;
        if needed > self.remaining() {
            return Err(WireError::Truncated);
        }
        Ok(count)
    }

    /// Skip the padding that aligns the next field to four bytes, after
    /// `consumed` bytes of a section.
    pub(crate) fn align4(&mut self, consumed: usize) -> Result<(), WireError> {
        let pad = pad4(consumed);
        let padding = self.take(pad)?;
        if padding.iter().any(|byte| *byte != 0) {
            return Err(WireError::Malformed("nonzero padding"));
        }
        Ok(())
    }
}

/// Bytes that pad `len` to a multiple of four.
pub(crate) fn pad4(len: usize) -> usize {
    len.wrapping_neg() & 3
}

/// Appends little-endian fields.
pub(crate) struct Writer<'a> {
    out: &'a mut Vec<u8>,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(out: &'a mut Vec<u8>) -> Self {
        Self { out }
    }

    pub(crate) fn len(&self) -> usize {
        self.out.len()
    }

    /// The buffer itself, to append a nested record in place.
    pub(crate) fn vec(&mut self) -> &mut Vec<u8> {
        self.out
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }

    pub(crate) fn u8(&mut self, value: u8) {
        self.out.push(value);
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    pub(crate) fn i16(&mut self, value: i16) {
        self.bytes(&value.to_le_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    pub(crate) fn i32(&mut self, value: i32) {
        self.bytes(&value.to_le_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    pub(crate) fn f32(&mut self, value: f32) {
        self.bytes(&value.to_le_bytes());
    }

    pub(crate) fn f64(&mut self, value: f64) {
        self.bytes(&value.to_le_bytes());
    }

    /// A count as the wire's `u32`.
    pub(crate) fn count(&mut self, count: usize) -> Result<(), WireError> {
        self.u32(u32::try_from(count).map_err(|_| WireError::Overflow)?);
        Ok(())
    }

    pub(crate) fn align4(&mut self, consumed: usize) {
        for _ in 0..pad4(consumed) {
            self.u8(0);
        }
    }

    /// Overwrite the `u32` at `at`, written earlier as a placeholder.
    pub(crate) fn patch_u32(&mut self, at: usize, value: u32) -> Result<(), WireError> {
        let end = at.checked_add(4).ok_or(WireError::Overflow)?;
        let slot = self
            .out
            .get_mut(at..end)
            .ok_or(WireError::Malformed("patch outside the record"))?;
        slot.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }
}

//! Fixed-capacity storage normalization. Entry requires the private checked
//! storage witness; no native document/operation reconstruction occurs here.
use super::inspect::{inflate, uint_size, visit_inflated, Reader, MAX_COLUMNS};
use super::{NativeAllowance, NativeResourceError as Error};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Default)]
struct Column {
    spec: u32,
    encoded: usize,
    normalized: usize,
}

struct Group<'a> {
    columns: [Column; MAX_COLUMNS],
    count: usize,
    data: &'a [u8],
}
impl<'a> Group<'a> {
    fn metadata(reader: &mut Reader<'a>) -> Result<Self, Error> {
        let count = reader.size()?;
        if count > MAX_COLUMNS {
            return Err(Error::Invalid);
        }
        let mut columns = [Column::default(); MAX_COLUMNS];
        for column in &mut columns[..count] {
            column.spec = u32::try_from(reader.uint()?).map_err(|_| Error::Invalid)?;
            column.encoded = reader.size()?;
        }
        Ok(Self {
            columns,
            count,
            data: &[],
        })
    }
    fn read_data(
        &mut self,
        reader: &mut Reader<'a>,
        maximum: usize,
        allowance: &NativeAllowance,
    ) -> Result<(), Error> {
        let len = self.columns[..self.count].iter().try_fold(0usize, |n, c| {
            n.checked_add(c.encoded).ok_or(Error::Invalid)
        })?;
        self.data = reader.take(len)?;
        let mut data = Reader(self.data);
        for column in &mut self.columns[..self.count] {
            let bytes = data.take(column.encoded)?;
            column.normalized = if column.spec & 8 == 0 {
                bytes.len()
            } else {
                let mut count = 0usize;
                visit_inflated(bytes, allowance, |part| {
                    count = count
                        .checked_add(part.len())
                        .filter(|n| *n <= maximum)
                        .ok_or(Error::NormalizedBytes)?;
                    Ok(())
                })?;
                count
            };
        }
        Ok(())
    }
    fn size(&self) -> Result<usize, Error> {
        self.columns[..self.count]
            .iter()
            .try_fold(uint_size(self.count), |n, c| {
                n.checked_add(uint_size((c.spec & !8) as usize))
                    .and_then(|n| n.checked_add(uint_size(c.normalized)))
                    .and_then(|n| n.checked_add(c.normalized))
                    .ok_or(Error::Invalid)
            })
    }
    fn write_metadata(&self, output: &mut Output<'_>) -> Result<(), Error> {
        output.uint(self.count)?;
        for column in &self.columns[..self.count] {
            output.uint((column.spec & !8) as usize)?;
            output.uint(column.normalized)?;
        }
        Ok(())
    }
    fn write_data(
        &self,
        output: &mut Output<'_>,
        allowance: &NativeAllowance,
    ) -> Result<(), Error> {
        let mut data = Reader(self.data);
        for column in &self.columns[..self.count] {
            let bytes = data.take(column.encoded)?;
            if column.spec & 8 == 0 {
                output.put(bytes)?;
            } else {
                let before = output.bytes.len();
                visit_inflated(bytes, allowance, |part| output.put(part))?;
                if output.bytes.len() - before != column.normalized {
                    return Err(Error::Invalid);
                }
            }
        }
        Ok(())
    }
}

struct Document<'a> {
    prefix: &'a [u8],
    changes: Group<'a>,
    ops: Group<'a>,
    suffix: &'a [u8],
}
impl<'a> Document<'a> {
    fn layout(body: &'a [u8], maximum: usize, allowance: &NativeAllowance) -> Result<Self, Error> {
        let mut reader = Reader(body);
        let actors = reader.size()?;
        for _ in 0..actors {
            reader.blob()?;
        }
        let heads = reader.size()?;
        reader.take(heads.checked_mul(32).ok_or(Error::Invalid)?)?;
        let prefix = &body[..body.len() - reader.0.len()];
        let mut changes = Group::metadata(&mut reader)?;
        let mut ops = Group::metadata(&mut reader)?;
        changes.read_data(&mut reader, maximum, allowance)?;
        ops.read_data(&mut reader, maximum, allowance)?;
        Ok(Self {
            prefix,
            changes,
            ops,
            suffix: reader.0,
        })
    }
    fn size(&self) -> Result<usize, Error> {
        let changes = self.changes.size()?;
        let ops = self.ops.size()?;
        self.prefix
            .len()
            .checked_add(changes)
            .and_then(|n| n.checked_add(ops))
            .and_then(|n| n.checked_add(self.suffix.len()))
            .ok_or(Error::Invalid)
    }
    fn write(&self, output: &mut Output<'_>, allowance: &NativeAllowance) -> Result<(), Error> {
        output.put(self.prefix)?;
        self.changes.write_metadata(output)?;
        self.ops.write_metadata(output)?;
        self.changes.write_data(output, allowance)?;
        self.ops.write_data(output, allowance)?;
        output.put(self.suffix)
    }
}

struct Output<'a> {
    bytes: &'a mut Vec<u8>,
    maximum: usize,
}
impl Output<'_> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let end = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(Error::Invalid)?;
        // Never ask Vec to grow. Its entire backing was charged before creation.
        if end > self.maximum || end > self.bytes.capacity() {
            return Err(Error::Invalid);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    fn uint(&mut self, mut value: usize) -> Result<(), Error> {
        loop {
            let byte = (value & 127) as u8;
            value >>= 7;
            self.put(&[byte | if value == 0 { 0 } else { 128 }])?;
            if value == 0 {
                return Ok(());
            }
        }
    }
}

pub(super) fn normalize(
    input: &[u8],
    bytes: &mut Vec<u8>,
    maximum: usize,
    allowance: &NativeAllowance,
) -> Result<(), Error> {
    let _scratch = allowance.reserve(std::mem::size_of::<Document<'_>>())?;
    let mut input = Reader(input);
    let mut output = Output { bytes, maximum };
    while !input.0.is_empty() {
        if input.take(4)? != crate::storage::MAGIC_BYTES {
            return Err(Error::Invalid);
        }
        input.take(4)?;
        let kind = input.take(1)?[0];
        let len = input.size()?;
        let body = input.take(len)?;
        let decoded;
        let (kind, body) = if kind == 2 {
            decoded = inflate(body, maximum, allowance)?;
            (1, &decoded[..])
        } else {
            (kind, body)
        };
        let document = if kind == 0 {
            Some(Document::layout(body, maximum, allowance)?)
        } else {
            None
        };
        let length = match &document {
            Some(doc) => doc.size()?,
            None => body.len(),
        };
        let start = output.bytes.len();
        output.put(&crate::storage::MAGIC_BYTES)?;
        output.put(&[0; 4])?;
        output.put(&[kind])?;
        output.uint(length)?;
        match document {
            Some(doc) => doc.write(&mut output, allowance)?,
            None if kind == 1 || kind == 3 => output.put(body)?,
            None => return Err(Error::Invalid),
        }
        let checksum = Sha256::digest(&output.bytes[start + 8..]);
        output.bytes[start + 4..start + 8].copy_from_slice(&checksum[..4]);
    }
    if output.bytes.len() != maximum {
        return Err(Error::Invalid);
    }
    Ok(())
}

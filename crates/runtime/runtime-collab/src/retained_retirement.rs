//! Cleanup observations only. Possession of a receipt grants no launch or file rights.
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
pub const ENDPOINT_TAG: u8 = 73;
const BOUND: usize = 4096;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subject {
    pub source_operation: String,
    pub context_sha256: String,
    pub root_pid: u32,
}
impl Subject {
    pub fn validate(&self) -> io::Result<()> {
        validate_labels(&self.source_operation, &self.context_sha256)?;
        if self.root_pid == 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    subject: Subject,
}
pub fn write(writer: &mut impl Write, subject: &Subject) -> io::Result<()> {
    subject.validate()?;
    let bytes = serde_json::to_vec(&Receipt {
        schema: 1,
        subject: subject.clone(),
    })
    .map_err(io::Error::other)?;
    if bytes.len() > BOUND {
        return Err(io::ErrorKind::InvalidData.into());
    }
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
}
pub fn read(reader: &mut impl Read, expected: &Subject) -> io::Result<()> {
    expected.validate()?;
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > BOUND {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    let receipt: Receipt = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    receipt.subject.validate()?;
    if receipt.schema != 1 || receipt.subject != *expected {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut trailing = [0];
    if reader.read(&mut trailing)? != 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(())
}

pub fn validate_labels(operation: &str, digest: &str) -> io::Result<()> {
    let id = uuid::Uuid::parse_str(operation).map_err(io::Error::other)?;
    if id.is_nil()
        || id.to_string() != operation
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    fn subject() -> Subject {
        Subject {
            source_operation: uuid::Uuid::new_v4().to_string(),
            context_sha256: "a".repeat(64),
            root_pid: std::process::id(),
        }
    }
    #[test]
    fn terminal_socket_receipt_is_exact_and_closed() {
        let expected = subject();
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        write(&mut writer, &expected).unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();
        read(&mut reader, &expected).unwrap();
        for changed in [
            Subject {
                context_sha256: "b".repeat(64),
                ..expected.clone()
            },
            Subject {
                root_pid: expected.root_pid + 1,
                ..expected.clone()
            },
            subject(),
        ] {
            let (mut reader, mut writer) = UnixStream::pair().unwrap();
            write(&mut writer, &changed).unwrap();
            writer.shutdown(std::net::Shutdown::Write).unwrap();
            assert!(read(&mut reader, &expected).is_err());
        }
    }
    #[test]
    fn malformed_or_extra_terminal_data_never_counts_as_cleanup() {
        let expected = subject();
        for bytes in [
            serde_json::to_vec(
                &serde_json::json!({"schema":1,"subject":expected,"authority":true}),
            )
            .unwrap(),
            serde_json::to_vec(&serde_json::json!({"schema":2,"subject":expected})).unwrap(),
            vec![b'x'; BOUND + 1],
        ] {
            let (mut reader, mut writer) = UnixStream::pair().unwrap();
            writer
                .write_all(&(bytes.len() as u32).to_le_bytes())
                .unwrap();
            writer.write_all(&bytes).unwrap();
            writer.shutdown(std::net::Shutdown::Write).unwrap();
            assert!(read(&mut reader, &expected).is_err());
        }
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        write(&mut writer, &expected).unwrap();
        writer.write_all(&[1]).unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(read(&mut reader, &expected).is_err());
    }
}

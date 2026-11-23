//! Fixed native issuer, no caller path or receipt is accepted as read authority.
use crate::setup::prepared_setup::completion;
use std::{
    ffi::OsString,
    io::{self, Write},
    sync::atomic::AtomicBool,
};
fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Completed installer request is invalid",
    )
}
pub fn selected(
    args: impl IntoIterator<Item = OsString>,
    output: &mut impl Write,
) -> io::Result<()> {
    if args.into_iter().next().is_some() {
        return Err(refused());
    }
    let path = completion::selected_generation()?;
    let bytes = path.to_str().ok_or_else(refused)?.as_bytes();
    if bytes.len() > 256 || bytes.contains(&b'\n') {
        return Err(refused());
    }
    output.write_all(bytes)?;
    output.write_all(b"\n")?;
    output.flush()
}
pub fn payload(
    args: impl IntoIterator<Item = OsString>,
    output: &mut impl Write,
) -> io::Result<()> {
    let mut args = args.into_iter();
    let kind = args.next().ok_or_else(refused)?;
    let kind = kind
        .to_str()
        .filter(|k| matches!(*k, "home" | "applications" | "browser"))
        .ok_or_else(refused)?;
    if args.next().is_some() {
        return Err(refused());
    }
    completion::stream_payload(kind, output, &AtomicBool::new(false))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helper_refuses_caller_path_receipt_and_unknown_payload_before_any_source_access() {
        for args in [
            vec!["/tmp/generation"],
            vec!["--receipt", "{}"],
            vec!["../home"],
            vec!["home", "/dev/sda"],
            vec![],
        ] {
            let mut output = Vec::new();
            assert_eq!(
                payload(args.into_iter().map(OsString::from), &mut output)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
            assert!(output.is_empty());
        }
        for value in ["/tmp/setup", "{}", "--package"] {
            let mut output = Vec::new();
            assert_eq!(
                selected([OsString::from(value)], &mut output)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
            assert!(output.is_empty());
        }
    }
}

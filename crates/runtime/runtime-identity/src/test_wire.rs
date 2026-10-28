use crate::{NotAvailable, wire};
use std::io::{Read, Write};

pub fn read_frame<T: serde::de::DeserializeOwned>(
    stream: &mut (impl Read + Write),
) -> Result<T, NotAvailable> {
    wire::FRAMING
        .read_value(stream)
        .map_err(|error| NotAvailable::ConnectionFailed(error.into()))?
        .ok_or_else(|| NotAvailable::ConnectionFailed(std::io::ErrorKind::UnexpectedEof.into()))
}

pub fn write_frame<T: serde::Serialize>(
    stream: &mut (impl Read + Write),
    value: &T,
) -> Result<(), NotAvailable> {
    wire::FRAMING
        .write_value(stream, value)
        .map_err(|error| NotAvailable::ConnectionFailed(error.into()))
}

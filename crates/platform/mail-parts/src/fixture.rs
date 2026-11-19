//! A message with attachments of chosen sizes, for tests that carry large mail.

/// A `multipart/mixed` message with a plain-text body and one base64 attachment per entry of
/// `attachments` (its file name, and how many bytes it holds), CRLF line endings, under
/// `message_id`. Each attachment's bytes are [`attachment_bytes`] of its name and size.
#[must_use]
pub fn message_with_attachments(
    message_id: &str,
    subject: &str,
    attachments: &[(&str, usize)],
) -> Vec<u8> {
    let boundary = "tilo-fixture-boundary";
    let mut message = format!(
        "Message-ID: <{message_id}>\r\nFrom: Grace Hopper <grace@example.net>\r\nTo: Ada <ada@example.com>\r\n\
Subject: {subject}\r\nDate: Tue, 3 Nov 2026 10:00:00 +0000\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n\
--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThe files are attached.\r\n"
    )
    .into_bytes();
    for (name, size) in attachments {
        message.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Type: application/octet-stream; name=\"{name}\"\r\n\
Content-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n"
            )
            .as_bytes(),
        );
        base64_lines(&attachment_bytes(name, *size), &mut message);
    }
    message.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    message
}

/// The bytes of a fixture attachment named `name` holding `size` bytes: different for every
/// name, and not the same byte over and over, so a part put back in the wrong place shows.
#[must_use]
pub fn attachment_bytes(name: &str, size: usize) -> Vec<u8> {
    let seed = name.bytes().fold(0x9e37_79b9_u32, |seed, byte| {
        seed.rotate_left(5) ^ u32::from(byte)
    });
    (0..size)
        .map(|index| {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let mixed = index
                .wrapping_mul(0x0101_0101)
                .wrapping_add(seed)
                .rotate_left(index % 13);
            mixed.to_le_bytes().first().copied().unwrap_or_default()
        })
        .collect()
}

/// `bytes` in base64, 76 characters to a CRLF-ended line, appended to `out`.
fn base64_lines(bytes: &[u8], out: &mut Vec<u8>) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let symbol = |value: u32| {
        ALPHABET
            .get(usize::try_from(value & 63).unwrap_or_default())
            .copied()
            .unwrap_or(b'=')
    };
    // 57 bytes make one 76-character line.
    for line in bytes.chunks(57) {
        for group in line.chunks(3) {
            let byte = |index: usize| u32::from(group.get(index).copied().unwrap_or_default());
            let value = (byte(0) << 16) | (byte(1) << 8) | byte(2);
            out.push(symbol(value >> 18));
            out.push(symbol(value >> 12));
            out.push(if group.len() > 1 {
                symbol(value >> 6)
            } else {
                b'='
            });
            out.push(if group.len() > 2 { symbol(value) } else { b'=' });
        }
        out.extend_from_slice(b"\r\n");
    }
}

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unreachable,
    clippy::arithmetic_side_effects,
    reason = "in a test these operations form assertions over bounded fixtures"
)]

use tilo_connected_protocol::{
    CodecError, DecodeStatus, Envelope, EnvelopeKind, FrameDecoder, MAX_FRAME_BODY_BYTES,
    NativeString, encode_frame,
};

const VALID_REQUEST: &str = concat!(
    r#"{"protocol":"tilo.connected","version":1,"kind":"request","#,
    r#""correlation":"0000000000000001","type":"fixture.echo","payload":{}}"#,
);

fn literal_frame(body: &[u8]) -> Vec<u8> {
    let length = u32::try_from(body.len()).expect("fixture body fits u32");
    let mut frame = length.to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    frame
}

fn decode_literal(body: &[u8]) -> Result<Envelope, CodecError> {
    let frame = literal_frame(body);
    let mut decoder = FrameDecoder::new();
    match decoder.push(&frame)? {
        DecodeStatus::Complete { envelope, consumed } => {
            assert_eq!(consumed, frame.len());
            Ok(envelope)
        }
        DecodeStatus::Pending { .. } => panic!("complete literal frame remained pending"),
    }
}

#[test]
fn decoded_envelopes_are_validated_in_a_fixed_order_but_keep_payloads_opaque() {
    // Phase: the fixed literal frame decodes and preserves its opaque payload.
    {
        let frame = literal_frame(VALID_REQUEST.as_bytes());
        assert_eq!(
            &frame[..4],
            &[0x00, 0x00, 0x00, 0x7e],
            "the wire prefix is the exact big-endian body length"
        );

        let envelope = decode_literal(VALID_REQUEST.as_bytes()).expect("valid fixed fixture");
        assert_eq!(envelope.kind(), EnvelopeKind::Request);
        assert_eq!(envelope.correlation(), 1);
        assert_eq!(envelope.message_type(), "fixture.echo");
        assert_eq!(envelope.payload_json(), "{}");

        let encoded = encode_frame(
            &Envelope::request(1, "fixture.echo", "{}").expect("fixed request constructor"),
        )
        .expect("fixed request encoder");
        assert_eq!(encoded, frame);
    }

    // Phase: payload members remain raw for later domain validation.
    {
        let body = br#"{"protocol":"tilo.connected","version":1,"kind":"response","correlation":"0000000000000001","type":"fixture.echo","payload":{"duplicate":1,"duplicate":2}}"#;
        let envelope =
            decode_literal(body).expect("opaque payload permits duplicate domain fields");
        assert_eq!(envelope.payload_json(), r#"{"duplicate":1,"duplicate":2}"#);
    }

    // Phase: validation precedence and categories are deterministic.
    {
        let invalid_utf8 = [0xff];
        assert!(matches!(
            decode_literal(&invalid_utf8),
            Err(CodecError::InvalidUtf8 { offset: 0 })
        ));

        let cases: &[(&[u8], CodecError)] = &[
            // The envelope root must be an object, not a positional array.
            (
                br#"["tilo.connected",1,"response","0000000000000001","fixture.echo",{}]"#,
                CodecError::InvalidEnvelope,
            ),
            (b"{", CodecError::InvalidJson { offset: 1 }),
            (b"{} {}", CodecError::InvalidJson { offset: 3 }),
            (
                br#"{"protocol":"tilo.connected","version":2,"kind":"request","correlation":"0000000000000001","type":"fixture.echo","payload":{}}"#,
                CodecError::UnsupportedVersion,
            ),
            (
                br#"{"protocol":"tilo.connected","version":-1,"kind":"request","correlation":"0000000000000001","type":"fixture.echo","payload":{}}"#,
                CodecError::UnsupportedVersion,
            ),
            (
                br#"{"protocol":"tilo.connected","version":18446744073709551615,"kind":"request","correlation":"0000000000000001","type":"fixture.echo","payload":{}}"#,
                CodecError::UnsupportedVersion,
            ),
            (
                br#"{"protocol":"tilo.connected","version":999999999999999999999999999999999999999999999999,"kind":"request","correlation":"0000000000000001","type":"fixture.echo","payload":{}}"#,
                CodecError::UnsupportedVersion,
            ),
            (
                br#"{"protocol":"tilo.connected","version":1.0,"kind":"request","correlation":"0000000000000001","type":"fixture.echo","payload":{}}"#,
                CodecError::InvalidEnvelope,
            ),
            (
                br#"{"protocol":"tilo.connected","version":1,"kind":"request","correlation":"0000000000000000","type":"fixture.echo","payload":{}}"#,
                CodecError::InvalidEnvelope,
            ),
            (
                br#"{"protocol":"tilo.connected","version":1,"kind":"request","correlation":"000000000000000A","type":"fixture.echo","payload":{}}"#,
                CodecError::InvalidEnvelope,
            ),
            (
                br#"{"protocol":"tilo.connected","version":1,"kind":"request","correlation":"0000000000000001","type":"Bad Type","payload":{}}"#,
                CodecError::InvalidEnvelope,
            ),
            (
                br#"{"protocol":"tilo.connected","version":1,"kind":"request","correlation":"0000000000000001","type":"fixture.echo","payload":[],"extra":0}"#,
                CodecError::InvalidEnvelope,
            ),
            (
                br#"{"protocol":"tilo.connected","version":1,"kind":"request","correlation":"0000000000000001","type":"fixture.echo","payload":[]}"#,
                CodecError::InvalidEnvelope,
            ),
            (
                br#"{"protocol":"tilo.connected","version":1,"kind":"request","correlation":"0000000000000001","type":"fixture.echo"}"#,
                CodecError::InvalidEnvelope,
            ),
            (
                br#"{"protocol":"tilo.connected","version":1,"kind":"request","correlation":"0000000000000001","type":"fixture.echo","payload":{},"unknown":0}"#,
                CodecError::InvalidEnvelope,
            ),
            (
                br#"{"protocol":"tilo.connected","protocol":"tilo.connected","version":1,"kind":"request","correlation":"0000000000000001","type":"fixture.echo","payload":{}}"#,
                CodecError::InvalidEnvelope,
            ),
        ];
        for (body, expected) in cases {
            assert_eq!(
                decode_literal(body),
                Err(expected.clone()),
                "case {}",
                String::from_utf8_lossy(body)
            );
        }
    }
}

#[test]
fn frame_decoder_is_incremental_and_rejects_bad_lengths_before_buffering() {
    // Phase: every header split and bytewise body feed is incremental.
    {
        let frame = literal_frame(VALID_REQUEST.as_bytes());
        for split in 0..=4 {
            let mut decoder = FrameDecoder::new();
            let first = decoder.push(&frame[..split]).expect("prefix fragment");
            assert_eq!(first, DecodeStatus::Pending { consumed: split });

            let mut complete = None;
            for byte in &frame[split..] {
                let status = decoder.push(std::slice::from_ref(byte)).expect("body byte");
                if let DecodeStatus::Complete { envelope, consumed } = status {
                    assert_eq!(consumed, 1);
                    complete = Some(envelope);
                }
            }
            assert_eq!(complete.expect("frame completes").correlation(), 1);
        }
    }

    // Phase: the decoder stops after one frame and reports unconsumed input.
    {
        let mut bytes = literal_frame(VALID_REQUEST.as_bytes());
        bytes.extend_from_slice(b"unsolicited");
        let mut decoder = FrameDecoder::new();

        let DecodeStatus::Complete { consumed, .. } = decoder.push(&bytes).expect("first frame")
        else {
            panic!("complete frame remained pending");
        };
        assert_eq!(consumed, literal_frame(VALID_REQUEST.as_bytes()).len());
    }

    // Phase: end of input distinguishes clean, header and body boundaries.
    {
        let clean = FrameDecoder::new();
        assert_eq!(clean.end_of_input(), Ok(()));

        for received in 1..=3 {
            let mut decoder = FrameDecoder::new();
            decoder.push(&[0; 3][..received]).expect("partial header");
            assert_eq!(
                decoder.end_of_input(),
                Err(CodecError::TruncatedHeader { received })
            );
        }

        let frame = literal_frame(VALID_REQUEST.as_bytes());
        for body_received in 0..VALID_REQUEST.len() {
            let mut decoder = FrameDecoder::new();
            decoder
                .push(&frame[..4 + body_received])
                .expect("partial body");
            assert_eq!(
                decoder.end_of_input(),
                Err(CodecError::TruncatedBody {
                    expected: VALID_REQUEST.len(),
                    received: body_received,
                })
            );
        }
    }

    // Phase: frame length is rejected before body buffering.
    {
        for (length, expected) in [
            (0_u32, CodecError::EmptyFrame),
            (
                u32::try_from(MAX_FRAME_BODY_BYTES + 1).expect("limit fits u32"),
                CodecError::FrameTooLarge,
            ),
        ] {
            let mut decoder = FrameDecoder::new();
            assert_eq!(decoder.push(&length.to_be_bytes()), Err(expected));
            assert_eq!(decoder.buffered_body_bytes(), 0);
        }

        let mut decoder = FrameDecoder::new();
        let exact_limit = u32::try_from(MAX_FRAME_BODY_BYTES).expect("limit fits u32");
        assert_eq!(
            decoder.push(&exact_limit.to_be_bytes()),
            Ok(DecodeStatus::Pending { consumed: 4 })
        );
        assert_eq!(decoder.buffered_body_bytes(), 0);
    }
}

#[test]
fn envelope_constructor_bounds_depth_type_tokens_and_raw_size() {
    // Phase: the depth limit counts containers but ignores braces in strings.
    {
        let open_at_limit = "[".repeat(62);
        let close_at_limit = "]".repeat(62);
        let payload_at_limit = [
            r#"{"text":"brace { and escaped quote \" and bracket [","v":"#,
            &open_at_limit,
            "0",
            &close_at_limit,
            "}",
        ]
        .concat();
        let envelope = Envelope::request(1, "fixture.echo", &payload_at_limit)
            .expect("64 total containers are valid");
        encode_frame(&envelope).expect("depth 64 encodes");

        let open_too_deep = "[".repeat(63);
        let close_too_deep = "]".repeat(63);
        let payload_too_deep = [r#"{"v":"#, &open_too_deep, "0", &close_too_deep, "}"].concat();
        assert!(matches!(
            Envelope::request(1, "fixture.echo", &payload_too_deep),
            Err(CodecError::DepthExceeded { .. })
        ));
    }

    // Phase: message type token boundaries are enforced.
    {
        let valid = format!("a{}", "0".repeat(63));
        assert!(Envelope::request(1, &valid, "{}").is_ok());
        assert_eq!(
            Envelope::request(1, &format!("a{}", "0".repeat(64)), "{}"),
            Err(CodecError::InvalidEnvelope)
        );
    }

    // Phase: an oversized raw payload is rejected before syntax or depth scanning.
    {
        let oversized_and_malformed = "[".repeat(MAX_FRAME_BODY_BYTES + 1);
        assert_eq!(
            Envelope::request(1, "fixture.echo", &oversized_and_malformed),
            Err(CodecError::FrameTooLarge)
        );
    }
}

#[test]
fn capped_encoder_enforces_the_encoded_body_byte_limit() {
    // Phase: the capped encoder accepts the exact body limit and rejects one more.
    {
        let empty = r#"{"data":""}"#;
        let fixed_body_overhead = concat!(
            r#"{"protocol":"tilo.connected","version":1,"kind":"request","#,
            r#""correlation":"0000000000000001","type":"fixture.echo","payload":{"data":""}}"#,
        )
        .len();
        let at_limit_data = "a".repeat(MAX_FRAME_BODY_BYTES - fixed_body_overhead);
        let at_limit_payload = format!(r#"{{"data":"{at_limit_data}"}}"#);
        let envelope =
            Envelope::request(1, "fixture.echo", &at_limit_payload).expect("valid payload");
        let frame = encode_frame(&envelope).expect("exact body limit fits");
        assert_eq!(frame.len(), MAX_FRAME_BODY_BYTES + 4);
        assert_eq!(&frame[..4], &[0x00, 0x04, 0x00, 0x00]);

        let over_limit_payload = format!(r#"{{"data":"{at_limit_data}a"}}"#);
        let envelope =
            Envelope::request(1, "fixture.echo", &over_limit_payload).expect("valid payload");
        assert_eq!(encode_frame(&envelope), Err(CodecError::FrameTooLarge));
        assert_eq!(empty.len(), 11);
    }

    // Phase: the capped encoder counts escaped payload bytes at the body limit.
    {
        let fixed_body_overhead = concat!(
            r#"{"protocol":"tilo.connected","version":1,"kind":"request","#,
            r#""correlation":"0000000000000001","type":"fixture.echo","payload":{"data":""}}"#,
        )
        .len();
        let available = MAX_FRAME_BODY_BYTES - fixed_body_overhead;
        let mut escaped_data = r#"\""#.repeat(available / 2);
        if available % 2 == 1 {
            escaped_data.push('a');
        }
        let payload = format!(r#"{{"data":"{escaped_data}"}}"#);
        let envelope = Envelope::request(1, "fixture.echo", &payload).expect("escaped payload");
        let frame = encode_frame(&envelope).expect("escaped body at exact limit");
        assert_eq!(frame.len(), MAX_FRAME_BODY_BYTES + 4);
    }

    // Phase: row count does not replace the encoded byte limit.
    {
        let no_rows = Envelope::request(1, "fixture.echo", r#"{"rows":[]}"#).expect("zero rows");
        assert!(encode_frame(&no_rows).is_ok());

        let row = format!(r#"{{"data":"{}"}}"#, "a".repeat(2_048));
        let rows = std::iter::repeat_n(row, 128).collect::<Vec<_>>().join(",");
        let payload = format!(r#"{{"rows":[{rows}]}}"#);
        assert_eq!(
            Envelope::request(1, "fixture.echo", &payload),
            Err(CodecError::FrameTooLarge)
        );
    }
}

#[test]
fn native_string_codec_is_lossless_strict_and_bounded() {
    // Phase: the codec preserves non-Unicode bytes and unpaired units.
    {
        let unix = NativeString::decode_json(br#"{"encoding":"unix_bytes","hex":"00ff80"}"#)
            .expect("Unix bytes");
        assert_eq!(unix, NativeString::UnixBytes(vec![0x00, 0xff, 0x80]));
        assert_eq!(
            unix.encode_json().expect("Unix encoding"),
            br#"{"encoding":"unix_bytes","hex":"00ff80"}"#
        );

        let windows =
            NativeString::decode_json(br#"{"encoding":"windows_utf16","hex":"d8000041dc00"}"#)
                .expect("Windows code units");
        assert_eq!(
            windows,
            NativeString::WindowsUtf16(vec![0xd800, 0x0041, 0xdc00])
        );
        assert_eq!(
            windows.encode_json().expect("Windows encoding"),
            br#"{"encoding":"windows_utf16","hex":"d8000041dc00"}"#
        );

        assert_eq!(
            NativeString::decode_json(br#"{"encoding":"unix_bytes","hex":""}"#),
            Ok(NativeString::UnixBytes(Vec::new()))
        );
        assert_eq!(
            NativeString::decode_json(br#"{"encoding":"windows_utf16","hex":""}"#),
            Ok(NativeString::WindowsUtf16(Vec::new()))
        );
    }

    // Phase: the codec is strict and bounded before conversion.
    {
        for malformed in [
            br#"{"encoding":"unix_bytes","hex":"0"}"#.as_slice(),
            br#"{"encoding":"unix_bytes","hex":"AA"}"#.as_slice(),
            br#"{"encoding":"windows_utf16","hex":"000"}"#.as_slice(),
            br#"{"encoding":"other","hex":"00"}"#.as_slice(),
            br#"{"encoding":"unix_bytes","hex":"00","extra":0}"#.as_slice(),
            br#"{"encoding":"unix_bytes","encoding":"unix_bytes","hex":"00"}"#.as_slice(),
            // Unix and Windows roots must be objects, not positional arrays.
            br#"["unix_bytes","00"]"#.as_slice(),
            br#"["windows_utf16","0041"]"#.as_slice(),
        ] {
            assert!(
                matches!(
                    NativeString::decode_json(malformed),
                    Err(CodecError::InvalidNativeString { .. })
                ),
                "case {}",
                String::from_utf8_lossy(malformed)
            );
        }

        let oversized = vec![b'a'; MAX_FRAME_BODY_BYTES + 1];
        assert_eq!(
            NativeString::decode_json(&oversized),
            Err(CodecError::FrameTooLarge)
        );

        let expansion_too_large = NativeString::UnixBytes(vec![0; MAX_FRAME_BODY_BYTES / 2]);
        assert_eq!(
            expansion_too_large.encode_json(),
            Err(CodecError::FrameTooLarge)
        );
    }
}

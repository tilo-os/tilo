//! The little of the DNS wire format the stub needs: the question a query
//! asks, the addresses an answer carries, and the refusal it sends instead
//! of an answer.
//!
//! Answers pass through byte-for-byte; the stub only reads them. A message
//! this parser does not understand is never admitted from — the worst a
//! parsing gap can do is make an allowed name unreachable.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The header is 12 bytes.
const HEADER: usize = 12;
/// A name is at most 255 bytes on the wire; a pointer chain longer than
/// this is a loop.
const MAX_POINTERS: usize = 32;

pub const TYPE_A: u16 = 1;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_SVCB: u16 = 64;
pub const TYPE_HTTPS: u16 = 65;

/// Response codes the stub sends itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rcode {
    /// The name was not granted: it does not exist, as far as the sandbox
    /// can tell. Final, so clients do not retry another server.
    NxDomain,
    /// The resolver behind the stub did not answer.
    ServFail,
    /// The query was not one the stub answers (several questions, not a
    /// query at all).
    Refused,
}

impl Rcode {
    fn code(self) -> u8 {
        match self {
            Self::ServFail => 2,
            Self::NxDomain => 3,
            Self::Refused => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub id: u16,
    /// Lower-case, dot-separated, no trailing dot; empty for the root.
    pub name: String,
    pub qtype: u16,
    /// Where the question section ends in the query.
    end: usize,
}

/// One address an answer carries, and the TTL it came with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answered {
    pub addr: IpAddr,
    pub ttl: u32,
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], at: usize) -> Self {
        Self { bytes, at }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let out = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(out)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_be_bytes(b.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_be_bytes(b.try_into().ok()?))
    }

    /// Read a possibly compressed name; the reader moves past it in place.
    fn name(&mut self) -> Option<String> {
        let mut out = String::new();
        let mut at = self.at;
        let mut resume: Option<usize> = None;
        let mut jumps = 0_usize;
        loop {
            let len = *self.bytes.get(at)?;
            match len & 0xc0 {
                0x00 => {
                    at = at.checked_add(1)?;
                    if len == 0 {
                        break;
                    }
                    let end = at.checked_add(usize::from(len))?;
                    let label = self.bytes.get(at..end)?;
                    if !out.is_empty() {
                        out.push('.');
                    }
                    if out.len().saturating_add(label.len()) > 255 {
                        return None;
                    }
                    for &b in label {
                        out.push(char::from(b.to_ascii_lowercase()));
                    }
                    at = end;
                }
                0xc0 => {
                    let low = *self.bytes.get(at.checked_add(1)?)?;
                    let target = usize::from(u16::from_be_bytes([len & 0x3f, low]));
                    if resume.is_none() {
                        resume = Some(at.checked_add(2)?);
                    }
                    jumps = jumps.saturating_add(1);
                    if jumps > MAX_POINTERS {
                        return None;
                    }
                    at = target;
                }
                _ => return None,
            }
        }
        self.at = resume.unwrap_or(at);
        Some(out)
    }
}

/// The header fields the stub reads.
struct Header {
    id: u16,
    flags: u16,
    qdcount: u16,
    ancount: u16,
}

fn header(message: &[u8]) -> Option<Header> {
    let mut r = Reader::new(message, 0);
    let id = r.u16()?;
    let flags = r.u16()?;
    let qdcount = r.u16()?;
    let ancount = r.u16()?;
    Some(Header {
        id,
        flags,
        qdcount,
        ancount,
    })
}

const FLAG_QR: u16 = 0x8000;
const FLAG_RD: u16 = 0x0100;
const FLAG_RA: u16 = 0x0080;
const FLAG_TC: u16 = 0x0200;
const OPCODE_MASK: u16 = 0x7800;

/// The question of a standard query with exactly one question, or `None`
/// for anything else (a response, another opcode, several questions,
/// malformed bytes).
pub fn parse_query(message: &[u8]) -> Option<Question> {
    let h = header(message)?;
    if h.flags & FLAG_QR != 0 || h.flags & OPCODE_MASK != 0 || h.qdcount != 1 {
        return None;
    }
    let mut r = Reader::new(message, HEADER);
    let name = r.name()?;
    let qtype = r.u16()?;
    let _qclass = r.u16()?;
    Some(Question {
        id: h.id,
        name,
        qtype,
        end: r.at,
    })
}

/// The id of a query, if it has a header at all — enough to refuse it.
pub fn query_id(message: &[u8]) -> Option<u16> {
    header(message).map(|h| h.id)
}

/// A response with `rcode` and no records, echoing the question when there
/// is one.
pub fn refusal(query: &[u8], question: Option<&Question>, rcode: Rcode) -> Vec<u8> {
    let rd = header(query).map_or(0, |h| h.flags & FLAG_RD);
    let id = question
        .map(|q| q.id)
        .or_else(|| query_id(query))
        .unwrap_or_default();
    let flags = FLAG_QR | rd | FLAG_RA | u16::from(rcode.code());
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&flags.to_be_bytes());
    let echoed = question.and_then(|q| query.get(HEADER..q.end));
    out.extend_from_slice(&u16::from(echoed.is_some()).to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    if let Some(section) = echoed {
        out.extend_from_slice(section);
    }
    out
}

/// Whether a response was truncated (the client should ask over TCP).
pub fn truncated(response: &[u8]) -> bool {
    header(response).is_some_and(|h| h.flags & FLAG_TC != 0)
}

/// Whether `response` answers `question`: same id, a response, and the same
/// question.
pub fn answers(response: &[u8], question: &Question) -> bool {
    let Some(h) = header(response) else {
        return false;
    };
    if h.id != question.id || h.flags & FLAG_QR == 0 || h.qdcount != 1 {
        return false;
    }
    let mut r = Reader::new(response, HEADER);
    r.name().is_some_and(|n| n == question.name) && r.u16() == Some(question.qtype)
}

/// Every address the answer section carries: A and AAAA records, and the
/// address hints of SVCB/HTTPS records (clients connect to those too). The
/// owner names are not checked: every record in the answer section answers
/// the one question, through whatever CNAME chain the resolver followed.
pub fn answered_addresses(response: &[u8]) -> Vec<Answered> {
    let mut out = Vec::new();
    let Some(h) = header(response) else {
        return out;
    };
    let mut r = Reader::new(response, HEADER);
    for _ in 0..h.qdcount {
        if r.name().is_none() || r.take(4).is_none() {
            return out;
        }
    }
    for _ in 0..h.ancount {
        let Some(record) = read_record(&mut r) else {
            break;
        };
        collect(&record, &mut out);
    }
    out
}

struct Record<'a> {
    rtype: u16,
    ttl: u32,
    rdata: &'a [u8],
}

fn read_record<'a>(r: &mut Reader<'a>) -> Option<Record<'a>> {
    r.name()?;
    let rtype = r.u16()?;
    let _class = r.u16()?;
    let ttl = r.u32()?;
    let len = r.u16()?;
    let rdata = r.take(usize::from(len))?;
    Some(Record { rtype, ttl, rdata })
}

fn collect(record: &Record<'_>, out: &mut Vec<Answered>) {
    match record.rtype {
        TYPE_A => {
            if let Ok(octets) = <[u8; 4]>::try_from(record.rdata) {
                out.push(Answered {
                    addr: IpAddr::V4(Ipv4Addr::from(octets)),
                    ttl: record.ttl,
                });
            }
        }
        TYPE_AAAA => {
            if let Ok(octets) = <[u8; 16]>::try_from(record.rdata) {
                out.push(Answered {
                    addr: IpAddr::V6(Ipv6Addr::from(octets)),
                    ttl: record.ttl,
                });
            }
        }
        TYPE_SVCB | TYPE_HTTPS => svcb_hints(record, out),
        _ => {}
    }
}

/// `ipv4hint` (key 4) and `ipv6hint` (key 6) of an SVCB/HTTPS record.
fn svcb_hints(record: &Record<'_>, out: &mut Vec<Answered>) {
    let mut r = Reader::new(record.rdata, 0);
    if r.u16().is_none() {
        return;
    }
    // The target name is never compressed in SVCB rdata.
    loop {
        let Some(len) = r.u8() else { return };
        if len == 0 {
            break;
        }
        if r.take(usize::from(len)).is_none() {
            return;
        }
    }
    while let (Some(key), Some(len)) = (r.u16(), r.u16()) {
        let Some(value) = r.take(usize::from(len)) else {
            return;
        };
        let width = match key {
            4 => 4,
            6 => 16,
            _ => continue,
        };
        for chunk in value.chunks_exact(width) {
            let addr = if width == 4 {
                <[u8; 4]>::try_from(chunk)
                    .ok()
                    .map(|o| IpAddr::V4(o.into()))
            } else {
                <[u8; 16]>::try_from(chunk)
                    .ok()
                    .map(|o| IpAddr::V6(o.into()))
            };
            if let Some(addr) = addr {
                out.push(Answered {
                    addr,
                    ttl: record.ttl,
                });
            }
        }
    }
}

/// A query for `name` of `qtype` — what `guard status` and the tests ask.
pub fn query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&FLAG_RD.to_be_bytes());
    out.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name
        .trim_end_matches('.')
        .split('.')
        .filter(|l| !l.is_empty())
    {
        let bytes = label.as_bytes();
        let Ok(len) = u8::try_from(bytes.len()) else {
            continue;
        };
        out.push(len);
        out.extend_from_slice(bytes);
    }
    out.push(0);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&1_u16.to_be_bytes());
    out
}

pub fn rcode(response: &[u8]) -> Option<u8> {
    header(response).and_then(|h| u8::try_from(h.flags & 0x000f).ok())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A response to `query` with the given answer records, each
    /// `(type, ttl, rdata)`, the owner name compressed to the question.
    pub(crate) fn response(query: &[u8], records: &[(u16, u32, Vec<u8>)]) -> Vec<u8> {
        let q = parse_query(query).unwrap();
        let mut out = query[..q.end].to_vec();
        out[2] |= 0x80; // QR
        out[3] |= 0x80; // RA
        out[6..8].copy_from_slice(&u16::try_from(records.len()).unwrap().to_be_bytes());
        for (rtype, ttl, rdata) in records {
            out.extend_from_slice(&[0xc0, 12]);
            out.extend_from_slice(&rtype.to_be_bytes());
            out.extend_from_slice(&1_u16.to_be_bytes());
            out.extend_from_slice(&ttl.to_be_bytes());
            out.extend_from_slice(&u16::try_from(rdata.len()).unwrap().to_be_bytes());
            out.extend_from_slice(rdata);
        }
        out
    }

    #[test]
    fn a_query_parses_to_its_lowercased_question() {
        let q = query(0x1234, "API.GitHub.com.", TYPE_AAAA);
        let parsed = parse_query(&q).unwrap();
        assert_eq!(parsed.id, 0x1234);
        assert_eq!(parsed.name, "api.github.com");
        assert_eq!(parsed.qtype, TYPE_AAAA);
        assert_eq!(parsed.end, q.len());

        // A response, two questions, and junk are not queries.
        let mut as_response = q.clone();
        as_response[2] |= 0x80;
        assert!(parse_query(&as_response).is_none());
        let mut two = q.clone();
        two[5] = 2;
        assert!(parse_query(&two).is_none());
        assert!(parse_query(&q[..q.len() - 3]).is_none());
        assert!(parse_query(&[]).is_none());
    }

    #[test]
    fn a_refusal_echoes_the_question_with_the_rcode() {
        let q = query(7, "denied.example", TYPE_A);
        let parsed = parse_query(&q).unwrap();
        let nx = refusal(&q, Some(&parsed), Rcode::NxDomain);
        assert_eq!(rcode(&nx), Some(3));
        assert!(answers(&nx, &parsed), "the refusal answers the question");
        assert!(answered_addresses(&nx).is_empty());
        let bare = refusal(&q[..12], None, Rcode::Refused);
        assert_eq!(bare.len(), 12);
        assert_eq!(rcode(&bare), Some(5));
        assert_eq!(query_id(&bare), Some(7));
    }

    #[test]
    fn answers_yield_their_addresses_and_ttls_through_cnames() {
        let q = query(9, "www.example.com", TYPE_A);
        // A CNAME (type 5) to a name that is compressed into the question,
        // then two A records and an AAAA.
        let resp = response(
            &q,
            &[
                (5, 300, vec![3, b'c', b'd', b'n', 0xc0, 16]),
                (TYPE_A, 60, vec![192, 0, 2, 1]),
                (TYPE_A, 7_200, vec![192, 0, 2, 2]),
                (
                    TYPE_AAAA,
                    30,
                    "2001:db8::1".parse::<Ipv6Addr>().unwrap().octets().to_vec(),
                ),
                (TYPE_A, 60, vec![1, 2, 3]),
            ],
        );
        let parsed = parse_query(&q).unwrap();
        assert!(answers(&resp, &parsed));
        assert!(!answers(
            &resp,
            &parse_query(&query(10, "www.example.com", TYPE_A)).unwrap()
        ));
        assert!(!answers(
            &resp,
            &parse_query(&query(9, "other.example", TYPE_A)).unwrap()
        ));
        let got = answered_addresses(&resp);
        assert_eq!(
            got,
            vec![
                Answered {
                    addr: "192.0.2.1".parse().unwrap(),
                    ttl: 60
                },
                Answered {
                    addr: "192.0.2.2".parse().unwrap(),
                    ttl: 7_200
                },
                Answered {
                    addr: "2001:db8::1".parse().unwrap(),
                    ttl: 30
                },
            ]
        );
    }

    #[test]
    fn https_records_yield_their_address_hints() {
        let q = query(3, "example.com", TYPE_HTTPS);
        let mut rdata = vec![0, 1, 0]; // priority 1, target "."
        rdata.extend_from_slice(&[0, 1, 0, 3, 2, b'h', b'2']); // alpn
        rdata.extend_from_slice(&[0, 4, 0, 8, 192, 0, 2, 7, 192, 0, 2, 8]); // ipv4hint
        let v6 = "2001:db8::7".parse::<Ipv6Addr>().unwrap().octets();
        rdata.extend_from_slice(&[0, 6, 0, 16]);
        rdata.extend_from_slice(&v6);
        let resp = response(&q, &[(TYPE_HTTPS, 120, rdata)]);
        let addrs: Vec<IpAddr> = answered_addresses(&resp).iter().map(|a| a.addr).collect();
        assert_eq!(
            addrs,
            vec![
                "192.0.2.7".parse::<IpAddr>().unwrap(),
                "192.0.2.8".parse().unwrap(),
                "2001:db8::7".parse().unwrap()
            ]
        );
    }

    #[test]
    fn pointer_loops_and_truncation_are_not_followed() {
        let q = query(1, "a.example", TYPE_A);
        let mut resp = response(&q, &[(TYPE_A, 60, vec![192, 0, 2, 1])]);
        let at = q.len();
        resp[at] = 0xc0;
        resp[at + 1] = u8::try_from(at).unwrap();
        assert!(answered_addresses(&resp).is_empty());
        assert!(!truncated(&resp));
        resp[2] |= 0x02;
        assert!(truncated(&resp));
    }
}

//! The sandbox's packet filter: one nftables table in its network namespace,
//! programmed over netlink. The table is created in one transaction, so
//! nothing leaves the namespace before the filter is in place.

use std::io;
use std::net::IpAddr;
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::Duration;

pub const TABLE: &str = "tilo_egress";
pub const CHAIN: &str = "output";
pub const SET4: &str = "allowed4";
pub const SET6: &str = "allowed6";
const SET4_ID: u32 = 1;
const SET6_ID: u32 = 2;

const LOOPBACK_IFINDEX: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    V4,
    V6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
    Icmpv6,
}

impl Proto {
    fn number(self) -> u8 {
        match self {
            Self::Tcp => 6,
            Self::Udp => 17,
            Self::Icmpv6 => 58,
        }
    }

    fn nft_name(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
            Self::Icmpv6 => "ipv6-icmp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    ToLoopback,
    /// Part of (or related to) a connection already allowed.
    Established,
    Family(Family),
    Proto(Proto),
    DestPort(u16),
    DestAllowed(Family),
    /// `ICMPv6` type in `from..=to` (follows `Family(V6)`, `Proto(Icmpv6)`).
    Icmpv6Types(u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accept,
    /// Refuse a TCP connection at once with a reset, so `connect` fails with
    /// "connection refused" instead of hanging until it times out.
    RejectReset,
    RejectProhibited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub matches: Vec<Match>,
    pub verdict: Verdict,
}

fn rule(matches: &[Match], verdict: Verdict) -> Rule {
    Rule {
        matches: matches.to_vec(),
        verdict,
    }
}

/// The output chain, in order. Its policy is drop; the last two rules
/// reject instead so a refused connection fails at once.
pub fn rules() -> Vec<Rule> {
    use Family::{V4, V6};
    use Match::{DestAllowed, DestPort, Established, Proto as P, ToLoopback};
    use Proto::{Icmpv6, Tcp, Udp};
    vec![
        rule(&[ToLoopback], Verdict::Accept),
        rule(&[Established], Verdict::Accept),
        rule(&[P(Udp), DestPort(53)], Verdict::RejectProhibited),
        rule(&[P(Tcp), DestPort(53)], Verdict::RejectReset),
        // Neighbour discovery: without it IPv6 never finds its gateway.
        rule(
            &[Match::Family(V6), P(Icmpv6), Match::Icmpv6Types(133, 136)],
            Verdict::Accept,
        ),
        rule(
            &[Match::Family(V4), P(Tcp), DestAllowed(V4)],
            Verdict::Accept,
        ),
        rule(
            &[Match::Family(V4), P(Udp), DestAllowed(V4)],
            Verdict::Accept,
        ),
        rule(
            &[Match::Family(V6), P(Tcp), DestAllowed(V6)],
            Verdict::Accept,
        ),
        rule(
            &[Match::Family(V6), P(Udp), DestAllowed(V6)],
            Verdict::Accept,
        ),
        rule(&[P(Tcp)], Verdict::RejectReset),
        rule(&[], Verdict::RejectProhibited),
    ]
}

pub fn render(rules: &[Rule]) -> String {
    let mut out = format!(
        "table inet {TABLE} {{\n\
         \tset {SET4} {{ type ipv4_addr; flags timeout; }}\n\
         \tset {SET6} {{ type ipv6_addr; flags timeout; }}\n\
         \tchain {CHAIN} {{\n\
         \t\ttype filter hook output priority 0; policy drop;\n"
    );
    for r in rules {
        let mut words: Vec<String> = Vec::new();
        let mut proto = None;
        for m in &r.matches {
            match *m {
                Match::ToLoopback => words.push("oif \"lo\"".into()),
                Match::Established => words.push("ct state established,related".into()),
                Match::Family(f) => words.push(
                    match f {
                        Family::V4 => "meta nfproto ipv4",
                        Family::V6 => "meta nfproto ipv6",
                    }
                    .into(),
                ),
                Match::Proto(p) => {
                    proto = Some(p);
                    words.push(format!("meta l4proto {}", p.nft_name()));
                }
                Match::DestPort(port) => words.push(format!(
                    "{} dport {port}",
                    proto.map_or("th", Proto::nft_name)
                )),
                Match::DestAllowed(f) => words.push(match f {
                    Family::V4 => format!("ip daddr @{SET4}"),
                    Family::V6 => format!("ip6 daddr @{SET6}"),
                }),
                Match::Icmpv6Types(from, to) => words.push(format!("icmpv6 type {from}-{to}")),
            }
        }
        words.push(
            match r.verdict {
                Verdict::Accept => "accept",
                Verdict::RejectReset => "reject with tcp reset",
                Verdict::RejectProhibited => "reject with icmpx admin-prohibited",
            }
            .into(),
        );
        out.push_str("\t\t");
        out.push_str(&words.join(" "));
        out.push('\n');
    }
    out.push_str("\t}\n}\n");
    out
}

const NETLINK_NETFILTER: libc::c_int = 12;
const NFNL_SUBSYS_NFTABLES: u16 = 10;
const NFNL_MSG_BATCH_BEGIN: u16 = 0x10;
const NFNL_MSG_BATCH_END: u16 = 0x11;
const NFPROTO_INET: u8 = 1;
const NFPROTO_IPV4: u8 = 2;
const NFPROTO_IPV6: u8 = 10;
const NFPROTO_BRIDGE: u8 = 7;

const NFT_MSG_NEWTABLE: u16 = 0;
const NFT_MSG_DELTABLE: u16 = 2;
const NFT_MSG_NEWCHAIN: u16 = 3;
const NFT_MSG_NEWRULE: u16 = 6;
const NFT_MSG_NEWSET: u16 = 9;
const NFT_MSG_NEWSETELEM: u16 = 12;
const NFT_MSG_DELSETELEM: u16 = 14;

const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_ACK: u16 = 0x4;
const NLM_F_CREATE: u16 = 0x400;
const NLM_F_APPEND: u16 = 0x800;
const NLMSG_ERROR: u16 = 2;
const NLA_F_NESTED: u16 = 0x8000;

const NFTA_LIST_ELEM: u16 = 1;
const NFTA_TABLE_NAME: u16 = 1;
const NFTA_CHAIN_TABLE: u16 = 1;
const NFTA_CHAIN_NAME: u16 = 3;
const NFTA_CHAIN_HOOK: u16 = 4;
const NFTA_CHAIN_POLICY: u16 = 5;
const NFTA_CHAIN_TYPE: u16 = 7;
const NFTA_HOOK_HOOKNUM: u16 = 1;
const NFTA_HOOK_PRIORITY: u16 = 2;
const NF_INET_LOCAL_OUT: u32 = 3;
const NF_BR_FORWARD: u32 = 2;
const NF_DROP: u32 = 0;
const NF_ACCEPT: u32 = 1;
const NFTA_RULE_TABLE: u16 = 1;
const NFTA_RULE_CHAIN: u16 = 2;
const NFTA_RULE_EXPRESSIONS: u16 = 4;
const NFTA_EXPR_NAME: u16 = 1;
const NFTA_EXPR_DATA: u16 = 2;
const NFTA_DATA_VALUE: u16 = 1;
const NFTA_DATA_VERDICT: u16 = 2;
const NFTA_VERDICT_CODE: u16 = 1;
const NFTA_SET_TABLE: u16 = 1;
const NFTA_SET_NAME: u16 = 2;
const NFTA_SET_FLAGS: u16 = 3;
const NFTA_SET_KEY_TYPE: u16 = 4;
const NFTA_SET_KEY_LEN: u16 = 5;
const NFTA_SET_ID: u16 = 10;
const NFT_SET_TIMEOUT: u32 = 0x10;
/// `nft`'s own type ids, stored so `nft list` can print the elements.
const TYPE_IPADDR: u32 = 7;
const TYPE_IP6ADDR: u32 = 8;
const NFTA_SET_ELEM_LIST_TABLE: u16 = 1;
const NFTA_SET_ELEM_LIST_SET: u16 = 2;
const NFTA_SET_ELEM_LIST_ELEMENTS: u16 = 3;
const NFTA_SET_ELEM_KEY: u16 = 1;
const NFTA_SET_ELEM_TIMEOUT: u16 = 4;

const NFT_REG_VERDICT: u32 = 0;
const NFT_REG_1: u32 = 1;
const NFTA_META_DREG: u16 = 1;
const NFTA_META_KEY: u16 = 2;
const NFT_META_PROTOCOL: u32 = 1;
const NFT_META_OIF: u32 = 5;
const NFT_META_IIFNAME: u32 = 6;
const NFT_META_NFPROTO: u32 = 15;
const NFT_META_L4PROTO: u32 = 16;
const NFTA_CMP_SREG: u16 = 1;
const NFTA_CMP_OP: u16 = 2;
const NFTA_CMP_DATA: u16 = 3;
const NFT_CMP_EQ: u32 = 0;
const NFT_CMP_NEQ: u32 = 1;
const NFT_CMP_LTE: u32 = 3;
const NFT_CMP_GTE: u32 = 5;
const NFTA_PAYLOAD_DREG: u16 = 1;
const NFTA_PAYLOAD_BASE: u16 = 2;
const NFTA_PAYLOAD_OFFSET: u16 = 3;
const NFTA_PAYLOAD_LEN: u16 = 4;
const NFT_PAYLOAD_NETWORK_HEADER: u32 = 1;
const NFT_PAYLOAD_TRANSPORT_HEADER: u32 = 2;
const NFTA_LOOKUP_SET: u16 = 1;
const NFTA_LOOKUP_SREG: u16 = 2;
const NFTA_LOOKUP_SET_ID: u16 = 4;
const NFTA_CT_DREG: u16 = 1;
const NFTA_CT_KEY: u16 = 2;
const NFT_CT_STATE: u32 = 0;
const NFTA_BITWISE_SREG: u16 = 1;
const NFTA_BITWISE_DREG: u16 = 2;
const NFTA_BITWISE_LEN: u16 = 3;
const NFTA_BITWISE_MASK: u16 = 4;
const NFTA_BITWISE_XOR: u16 = 5;
const NFTA_IMMEDIATE_DREG: u16 = 1;
const NFTA_IMMEDIATE_DATA: u16 = 2;
const NFTA_REJECT_TYPE: u16 = 1;
const NFTA_REJECT_ICMP_CODE: u16 = 2;
const NFT_REJECT_TCP_RST: u32 = 1;
const NFT_REJECT_ICMPX_UNREACH: u32 = 2;
const NFT_REJECT_ICMPX_ADMIN_PROHIBITED: u8 = 3;
const CT_STATE_ESTABLISHED_RELATED: u32 = 0x02 | 0x04;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Expr {
    Meta(u32),
    Cmp(u32, Vec<u8>),
    Payload { base: u32, offset: u32, len: u32 },
    Lookup { set: &'static str, id: u32 },
    Ct(u32),
    Bitwise { mask: Vec<u8>, xor: Vec<u8> },
    Verdict(u32),
    Reject { kind: u32, code: u8 },
}

fn exprs(m: Match) -> Vec<Expr> {
    match m {
        Match::ToLoopback => vec![
            Expr::Meta(NFT_META_OIF),
            Expr::Cmp(NFT_CMP_EQ, LOOPBACK_IFINDEX.to_ne_bytes().to_vec()),
        ],
        Match::Established => vec![
            Expr::Ct(NFT_CT_STATE),
            Expr::Bitwise {
                mask: CT_STATE_ESTABLISHED_RELATED.to_ne_bytes().to_vec(),
                xor: vec![0; 4],
            },
            Expr::Cmp(NFT_CMP_NEQ, vec![0; 4]),
        ],
        Match::Family(f) => vec![
            Expr::Meta(NFT_META_NFPROTO),
            Expr::Cmp(
                NFT_CMP_EQ,
                vec![match f {
                    Family::V4 => NFPROTO_IPV4,
                    Family::V6 => NFPROTO_IPV6,
                }],
            ),
        ],
        Match::Proto(p) => vec![
            Expr::Meta(NFT_META_L4PROTO),
            Expr::Cmp(NFT_CMP_EQ, vec![p.number()]),
        ],
        Match::DestPort(port) => vec![
            Expr::Payload {
                base: NFT_PAYLOAD_TRANSPORT_HEADER,
                offset: 2,
                len: 2,
            },
            Expr::Cmp(NFT_CMP_EQ, port.to_be_bytes().to_vec()),
        ],
        Match::DestAllowed(Family::V4) => vec![
            Expr::Payload {
                base: NFT_PAYLOAD_NETWORK_HEADER,
                offset: 16,
                len: 4,
            },
            Expr::Lookup {
                set: SET4,
                id: SET4_ID,
            },
        ],
        Match::DestAllowed(Family::V6) => vec![
            Expr::Payload {
                base: NFT_PAYLOAD_NETWORK_HEADER,
                offset: 24,
                len: 16,
            },
            Expr::Lookup {
                set: SET6,
                id: SET6_ID,
            },
        ],
        Match::Icmpv6Types(from, to) => vec![
            Expr::Payload {
                base: NFT_PAYLOAD_TRANSPORT_HEADER,
                offset: 0,
                len: 1,
            },
            Expr::Cmp(NFT_CMP_GTE, vec![from]),
            Expr::Cmp(NFT_CMP_LTE, vec![to]),
        ],
    }
}

fn verdict_exprs(v: Verdict) -> Expr {
    match v {
        Verdict::Accept => Expr::Verdict(NF_ACCEPT),
        Verdict::RejectReset => Expr::Reject {
            kind: NFT_REJECT_TCP_RST,
            code: 0,
        },
        Verdict::RejectProhibited => Expr::Reject {
            kind: NFT_REJECT_ICMPX_UNREACH,
            code: NFT_REJECT_ICMPX_ADMIN_PROHIBITED,
        },
    }
}

struct Attrs<'a>(&'a mut Vec<u8>);

impl Attrs<'_> {
    fn put(&mut self, kind: u16, payload: &[u8]) {
        let len = u16::try_from(payload.len().saturating_add(4)).unwrap_or(u16::MAX);
        self.0.extend_from_slice(&len.to_ne_bytes());
        self.0.extend_from_slice(&kind.to_ne_bytes());
        self.0.extend_from_slice(payload);
        pad(self.0);
    }

    fn str(&mut self, kind: u16, s: &str) {
        let mut z = s.as_bytes().to_vec();
        z.push(0);
        self.put(kind, &z);
    }

    fn be32(&mut self, kind: u16, v: u32) {
        self.put(kind, &v.to_be_bytes());
    }

    fn be64(&mut self, kind: u16, v: u64) {
        self.put(kind, &v.to_be_bytes());
    }

    fn nest(&mut self, kind: u16, inner: impl FnOnce(&mut Attrs<'_>)) {
        let start = self.0.len();
        self.0.extend_from_slice(&[0, 0]);
        self.0
            .extend_from_slice(&(kind | NLA_F_NESTED).to_ne_bytes());
        inner(&mut Attrs(self.0));
        let len = u16::try_from(self.0.len().saturating_sub(start)).unwrap_or(u16::MAX);
        if let Some(slot) = self.0.get_mut(start..start.saturating_add(2)) {
            slot.copy_from_slice(&len.to_ne_bytes());
        }
    }

    fn data(&mut self, kind: u16, value: &[u8]) {
        self.nest(kind, |a| a.put(NFTA_DATA_VALUE, value));
    }
}

fn pad(buf: &mut Vec<u8>) {
    while buf.len() & 3 != 0 {
        buf.push(0);
    }
}

fn encode_expr(a: &mut Attrs<'_>, e: &Expr) {
    let name = match e {
        Expr::Meta(_) => "meta",
        Expr::Cmp(..) => "cmp",
        Expr::Payload { .. } => "payload",
        Expr::Lookup { .. } => "lookup",
        Expr::Ct(_) => "ct",
        Expr::Bitwise { .. } => "bitwise",
        Expr::Verdict(_) => "immediate",
        Expr::Reject { .. } => "reject",
    };
    a.nest(NFTA_LIST_ELEM, |a| {
        a.str(NFTA_EXPR_NAME, name);
        a.nest(NFTA_EXPR_DATA, |a| match e {
            Expr::Meta(key) => {
                a.be32(NFTA_META_KEY, *key);
                a.be32(NFTA_META_DREG, NFT_REG_1);
            }
            Expr::Cmp(op, data) => {
                a.be32(NFTA_CMP_SREG, NFT_REG_1);
                a.be32(NFTA_CMP_OP, *op);
                a.data(NFTA_CMP_DATA, data);
            }
            Expr::Payload { base, offset, len } => {
                a.be32(NFTA_PAYLOAD_DREG, NFT_REG_1);
                a.be32(NFTA_PAYLOAD_BASE, *base);
                a.be32(NFTA_PAYLOAD_OFFSET, *offset);
                a.be32(NFTA_PAYLOAD_LEN, *len);
            }
            Expr::Lookup { set, id } => {
                a.str(NFTA_LOOKUP_SET, set);
                a.be32(NFTA_LOOKUP_SREG, NFT_REG_1);
                a.be32(NFTA_LOOKUP_SET_ID, *id);
            }
            Expr::Ct(key) => {
                a.be32(NFTA_CT_KEY, *key);
                a.be32(NFTA_CT_DREG, NFT_REG_1);
            }
            Expr::Bitwise { mask, xor } => {
                a.be32(NFTA_BITWISE_SREG, NFT_REG_1);
                a.be32(NFTA_BITWISE_DREG, NFT_REG_1);
                a.be32(
                    NFTA_BITWISE_LEN,
                    u32::try_from(mask.len()).unwrap_or_default(),
                );
                a.data(NFTA_BITWISE_MASK, mask);
                a.data(NFTA_BITWISE_XOR, xor);
            }
            Expr::Verdict(code) => {
                a.be32(NFTA_IMMEDIATE_DREG, NFT_REG_VERDICT);
                a.nest(NFTA_IMMEDIATE_DATA, |a| {
                    a.nest(NFTA_DATA_VERDICT, |a| a.be32(NFTA_VERDICT_CODE, *code));
                });
            }
            Expr::Reject { kind, code } => {
                a.be32(NFTA_REJECT_TYPE, *kind);
                a.put(NFTA_REJECT_ICMP_CODE, &[*code]);
            }
        });
    });
}

struct Batch {
    buf: Vec<u8>,
    seq: u32,
    acked: Vec<u32>,
    family: u8,
}

impl Batch {
    fn new(seq: u32) -> Self {
        Self::of_family(seq, NFPROTO_INET)
    }

    fn of_family(seq: u32, family: u8) -> Self {
        let mut b = Self {
            buf: Vec::with_capacity(4096),
            seq,
            acked: Vec::new(),
            family,
        };
        b.raw(
            NFNL_MSG_BATCH_BEGIN,
            NLM_F_REQUEST,
            0,
            NFNL_SUBSYS_NFTABLES,
            |_| {},
        );
        b
    }

    fn raw(
        &mut self,
        kind: u16,
        flags: u16,
        family: u8,
        res_id: u16,
        attrs: impl FnOnce(&mut Attrs<'_>),
    ) {
        let start = self.buf.len();
        self.buf.extend_from_slice(&[0; 4]);
        self.buf.extend_from_slice(&kind.to_ne_bytes());
        self.buf.extend_from_slice(&flags.to_ne_bytes());
        self.buf.extend_from_slice(&self.seq.to_ne_bytes());
        self.buf.extend_from_slice(&0_u32.to_ne_bytes());
        self.buf.push(family);
        self.buf.push(0);
        self.buf.extend_from_slice(&res_id.to_be_bytes());
        attrs(&mut Attrs(&mut self.buf));
        let len = u32::try_from(self.buf.len().saturating_sub(start)).unwrap_or(u32::MAX);
        if let Some(slot) = self.buf.get_mut(start..start.saturating_add(4)) {
            slot.copy_from_slice(&len.to_ne_bytes());
        }
        if flags & NLM_F_ACK != 0 {
            self.acked.push(self.seq);
        }
        self.seq = self.seq.wrapping_add(1);
    }

    fn msg(&mut self, kind: u16, flags: u16, attrs: impl FnOnce(&mut Attrs<'_>)) {
        self.raw(
            (NFNL_SUBSYS_NFTABLES << 8) | kind,
            NLM_F_REQUEST | NLM_F_ACK | flags,
            self.family,
            0,
            attrs,
        );
    }

    fn finish(mut self) -> (Vec<u8>, Vec<u32>, u32) {
        self.raw(
            NFNL_MSG_BATCH_END,
            NLM_F_REQUEST,
            0,
            NFNL_SUBSYS_NFTABLES,
            |_| {},
        );
        (self.buf, self.acked, self.seq)
    }
}

fn add_table_messages(b: &mut Batch, rules: &[Rule]) {
    b.msg(NFT_MSG_NEWTABLE, NLM_F_CREATE, |a| {
        a.str(NFTA_TABLE_NAME, TABLE);
    });
    for (name, id, kind, len) in [
        (SET4, SET4_ID, TYPE_IPADDR, 4_u32),
        (SET6, SET6_ID, TYPE_IP6ADDR, 16),
    ] {
        b.msg(NFT_MSG_NEWSET, NLM_F_CREATE, |a| {
            a.str(NFTA_SET_TABLE, TABLE);
            a.str(NFTA_SET_NAME, name);
            a.be32(NFTA_SET_FLAGS, NFT_SET_TIMEOUT);
            a.be32(NFTA_SET_KEY_TYPE, kind);
            a.be32(NFTA_SET_KEY_LEN, len);
            a.be32(NFTA_SET_ID, id);
        });
    }
    b.msg(NFT_MSG_NEWCHAIN, NLM_F_CREATE, |a| {
        a.str(NFTA_CHAIN_TABLE, TABLE);
        a.str(NFTA_CHAIN_NAME, CHAIN);
        a.nest(NFTA_CHAIN_HOOK, |a| {
            a.be32(NFTA_HOOK_HOOKNUM, NF_INET_LOCAL_OUT);
            a.be32(NFTA_HOOK_PRIORITY, 0);
        });
        a.be32(NFTA_CHAIN_POLICY, NF_DROP);
        a.str(NFTA_CHAIN_TYPE, "filter");
    });
    for r in rules {
        b.msg(NFT_MSG_NEWRULE, NLM_F_CREATE | NLM_F_APPEND, |a| {
            a.str(NFTA_RULE_TABLE, TABLE);
            a.str(NFTA_RULE_CHAIN, CHAIN);
            a.nest(NFTA_RULE_EXPRESSIONS, |a| {
                for m in &r.matches {
                    for e in exprs(*m) {
                        encode_expr(a, &e);
                    }
                }
                encode_expr(a, &verdict_exprs(r.verdict));
            });
        });
    }
}

fn set_of(addr: IpAddr) -> &'static str {
    if addr.is_ipv4() { SET4 } else { SET6 }
}

fn key_of(addr: IpAddr) -> Vec<u8> {
    match addr {
        IpAddr::V4(v4) => v4.octets().to_vec(),
        IpAddr::V6(v6) => v6.octets().to_vec(),
    }
}

fn element_message(
    b: &mut Batch,
    kind: u16,
    flags: u16,
    set: &str,
    elems: &[(IpAddr, Option<Duration>)],
) {
    if elems.is_empty() {
        return;
    }
    b.msg(kind, flags, |a| {
        a.str(NFTA_SET_ELEM_LIST_TABLE, TABLE);
        a.str(NFTA_SET_ELEM_LIST_SET, set);
        a.nest(NFTA_SET_ELEM_LIST_ELEMENTS, |a| {
            for (addr, timeout) in elems {
                a.nest(NFTA_LIST_ELEM, |a| {
                    a.data(NFTA_SET_ELEM_KEY, &key_of(*addr));
                    if let Some(t) = timeout {
                        a.be64(
                            NFTA_SET_ELEM_TIMEOUT,
                            u64::try_from(t.as_millis()).unwrap_or(u64::MAX),
                        );
                    }
                });
            }
        });
    });
}

/// The bridge table of a VM's network namespace: what the VM's frames may
/// reach.
///
/// ```text
/// table bridge tilo_guest {
///     chain forward { type filter hook forward priority 0; policy drop; … }
/// }
/// ```
///
/// The namespace bridges two ports: the VM's tap and the uplink (`pasta`'s
/// tap, which re-makes every connection from a host socket). Frames from
/// the uplink are answers (nothing is forwarded inwards) and pass. From the
/// VM, ARP passes, and IPv4 does unless its destination is in a blocked
/// network; the two exceptions are DNS to the uplink's forwarder and the one
/// [`GuestFilter::endpoint`] the VM is given. Everything else, IPv6
/// included, is dropped. Only destinations are matched, so no connection
/// tracking is needed on the bridge.
pub const GUEST_TABLE: &str = "tilo_guest";
pub const GUEST_CHAIN: &str = "forward";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestFilter {
    pub guest: String,
    pub uplink: String,
    /// The uplink's DNS forwarder, reachable on port 53 though blocked.
    pub dns: std::net::Ipv4Addr,
    pub endpoint: Option<(std::net::Ipv4Addr, u16)>,
    /// Networks the VM never reaches: the host's, the cluster's, the cloud's
    /// metadata endpoints, other VMs'.
    pub blocked: Vec<(std::net::Ipv4Addr, u8)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestMatch {
    InIface([u8; 16]),
    Arp,
    /// An IPv4 packet.
    Ipv4,
    Proto(Proto),
    DestPort(u16),
    /// Destination in `address/prefix` (follows [`GuestMatch::Ipv4`]).
    DestNet(std::net::Ipv4Addr, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestVerdict {
    Accept,
    Drop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestRule {
    pub matches: Vec<GuestMatch>,
    pub verdict: GuestVerdict,
}

fn ifname(name: &str) -> [u8; 16] {
    let mut out = [0_u8; 16];
    for (slot, byte) in out.iter_mut().zip(name.bytes().take(15)) {
        *slot = byte;
    }
    out
}

fn mask4(prefix: u8) -> [u8; 4] {
    let bits = u32::MAX
        .checked_shl(32_u32.saturating_sub(u32::from(prefix.min(32))))
        .unwrap_or(0);
    bits.to_be_bytes()
}

fn net4(address: std::net::Ipv4Addr, prefix: u8) -> [u8; 4] {
    let mask = u32::from_be_bytes(mask4(prefix));
    (u32::from(address) & mask).to_be_bytes()
}

impl GuestFilter {
    /// The forward chain, in order. Its policy is drop.
    pub fn rules(&self) -> Vec<GuestRule> {
        use GuestMatch::{Arp, DestNet, DestPort, InIface, Ipv4, Proto as P};
        let rule = |matches: &[GuestMatch], verdict| GuestRule {
            matches: matches.to_vec(),
            verdict,
        };
        let (guest, uplink) = (ifname(&self.guest), ifname(&self.uplink));
        let mut rules = vec![
            rule(&[InIface(uplink)], GuestVerdict::Accept),
            rule(&[InIface(guest), Arp], GuestVerdict::Accept),
        ];
        for proto in [Proto::Udp, Proto::Tcp] {
            rules.push(rule(
                &[
                    InIface(guest),
                    Ipv4,
                    DestNet(self.dns, 32),
                    P(proto),
                    DestPort(53),
                ],
                GuestVerdict::Accept,
            ));
        }
        if let Some((address, port)) = self.endpoint {
            for proto in [Proto::Udp, Proto::Tcp] {
                rules.push(rule(
                    &[
                        InIface(guest),
                        Ipv4,
                        DestNet(address, 32),
                        P(proto),
                        DestPort(port),
                    ],
                    GuestVerdict::Accept,
                ));
            }
        }
        for (address, prefix) in &self.blocked {
            rules.push(rule(
                &[InIface(guest), Ipv4, DestNet(*address, *prefix)],
                GuestVerdict::Drop,
            ));
        }
        rules.push(rule(&[InIface(guest), Ipv4], GuestVerdict::Accept));
        rules
    }

    pub fn render(&self) -> String {
        let mut out = format!(
            "table bridge {GUEST_TABLE} {{\n\tchain {GUEST_CHAIN} {{\n\
             \t\ttype filter hook forward priority 0; policy drop;\n"
        );
        for r in self.rules() {
            let mut words: Vec<String> = Vec::new();
            let mut proto = None;
            for m in &r.matches {
                match *m {
                    GuestMatch::InIface(name) => {
                        let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
                        words.push(format!(
                            "iifname \"{}\"",
                            String::from_utf8_lossy(name.get(..end).unwrap_or_default())
                        ));
                    }
                    GuestMatch::Arp => words.push("ether type arp".into()),
                    GuestMatch::Ipv4 => words.push("ether type ip".into()),
                    GuestMatch::Proto(p) => {
                        proto = Some(p);
                        words.push(format!("meta l4proto {}", p.nft_name()));
                    }
                    GuestMatch::DestPort(port) => words.push(format!(
                        "{} dport {port}",
                        proto.map_or("th", Proto::nft_name)
                    )),
                    GuestMatch::DestNet(address, 32) => words.push(format!("ip daddr {address}")),
                    GuestMatch::DestNet(address, prefix) => words.push(format!(
                        "ip daddr {}/{prefix}",
                        std::net::Ipv4Addr::from(net4(address, prefix))
                    )),
                }
            }
            words.push(
                match r.verdict {
                    GuestVerdict::Accept => "accept",
                    GuestVerdict::Drop => "drop",
                }
                .into(),
            );
            out.push_str("\t\t");
            out.push_str(&words.join(" "));
            out.push('\n');
        }
        out.push_str("\t}\n}\n");
        out
    }
}

fn guest_exprs(m: GuestMatch) -> Vec<Expr> {
    const ETH_P_IP: u16 = 0x0800;
    const ETH_P_ARP: u16 = 0x0806;
    match m {
        GuestMatch::InIface(name) => vec![
            Expr::Meta(NFT_META_IIFNAME),
            Expr::Cmp(NFT_CMP_EQ, name.to_vec()),
        ],
        GuestMatch::Arp => vec![
            Expr::Meta(NFT_META_PROTOCOL),
            Expr::Cmp(NFT_CMP_EQ, ETH_P_ARP.to_be_bytes().to_vec()),
        ],
        GuestMatch::Ipv4 => vec![
            Expr::Meta(NFT_META_PROTOCOL),
            Expr::Cmp(NFT_CMP_EQ, ETH_P_IP.to_be_bytes().to_vec()),
        ],
        GuestMatch::Proto(p) => exprs(Match::Proto(p)),
        GuestMatch::DestPort(port) => exprs(Match::DestPort(port)),
        GuestMatch::DestNet(address, prefix) => {
            let load = Expr::Payload {
                base: NFT_PAYLOAD_NETWORK_HEADER,
                offset: 16,
                len: 4,
            };
            let compare = Expr::Cmp(NFT_CMP_EQ, net4(address, prefix).to_vec());
            if prefix >= 32 {
                vec![load, compare]
            } else {
                vec![
                    load,
                    Expr::Bitwise {
                        mask: mask4(prefix).to_vec(),
                        xor: vec![0; 4],
                    },
                    compare,
                ]
            }
        }
    }
}

fn add_guest_messages(b: &mut Batch, filter: &GuestFilter) {
    b.msg(NFT_MSG_NEWTABLE, NLM_F_CREATE, |a| {
        a.str(NFTA_TABLE_NAME, GUEST_TABLE);
    });
    b.msg(NFT_MSG_NEWCHAIN, NLM_F_CREATE, |a| {
        a.str(NFTA_CHAIN_TABLE, GUEST_TABLE);
        a.str(NFTA_CHAIN_NAME, GUEST_CHAIN);
        a.nest(NFTA_CHAIN_HOOK, |a| {
            a.be32(NFTA_HOOK_HOOKNUM, NF_BR_FORWARD);
            a.be32(NFTA_HOOK_PRIORITY, 0);
        });
        a.be32(NFTA_CHAIN_POLICY, NF_DROP);
        a.str(NFTA_CHAIN_TYPE, "filter");
    });
    for r in filter.rules() {
        b.msg(NFT_MSG_NEWRULE, NLM_F_CREATE | NLM_F_APPEND, |a| {
            a.str(NFTA_RULE_TABLE, GUEST_TABLE);
            a.str(NFTA_RULE_CHAIN, GUEST_CHAIN);
            a.nest(NFTA_RULE_EXPRESSIONS, |a| {
                for m in &r.matches {
                    for e in guest_exprs(*m) {
                        encode_expr(a, &e);
                    }
                }
                encode_expr(
                    a,
                    &Expr::Verdict(match r.verdict {
                        GuestVerdict::Accept => NF_ACCEPT,
                        GuestVerdict::Drop => NF_DROP,
                    }),
                );
            });
        });
    }
}

#[derive(Debug)]
pub struct Netfilter {
    sock: OwnedFd,
    seq: u32,
    installed: bool,
}

const ACK_TIMEOUT: Duration = Duration::from_secs(5);

impl Netfilter {
    pub fn new(sock: OwnedFd) -> io::Result<Self> {
        let tv = libc::timeval {
            tv_sec: libc::time_t::try_from(ACK_TIMEOUT.as_secs()).unwrap_or(5),
            tv_usec: 0,
        };
        // SAFETY: `tv` is a valid timeval for the duration of the call.
        let rc = unsafe {
            libc::setsockopt(
                sock.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&raw const tv).cast(),
                socklen::<libc::timeval>(),
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // The sub-second nanoseconds always fit a u32.
        let seed = tilo_clock::try_unix_now_ns()
            .map_or(1, |ns| u32::try_from(ns % 1_000_000_000).unwrap_or(1));
        Ok(Self {
            sock,
            seq: seed,
            installed: false,
        })
    }

    pub const PROTOCOL: libc::c_int = NETLINK_NETFILTER;

    pub fn installed(&self) -> bool {
        self.installed
    }

    /// Create the table, its sets and the chain with [`rules`], in one
    /// transaction. Before this returns, nothing leaves the namespace that
    /// the rules do not allow; if it fails, nothing was installed.
    pub fn install(&mut self) -> io::Result<()> {
        if self.installed {
            return Ok(());
        }
        let mut b = Batch::new(self.seq);
        add_table_messages(&mut b, &rules());
        self.commit(b)?;
        self.installed = true;
        Ok(())
    }

    /// Install `filter` as the namespace's bridge table, in one
    /// transaction: before this returns nothing crosses the bridge that it
    /// does not allow, and if it fails nothing was installed.
    pub fn install_guest(&mut self, filter: &GuestFilter) -> io::Result<()> {
        let mut b = Batch::of_family(self.seq, NFPROTO_BRIDGE);
        add_guest_messages(&mut b, filter);
        self.commit(b)
    }

    pub fn remove(&mut self) -> io::Result<()> {
        if !self.installed {
            return Ok(());
        }
        let mut b = Batch::new(self.seq);
        b.msg(NFT_MSG_DELTABLE, 0, |a| a.str(NFTA_TABLE_NAME, TABLE));
        self.commit(b)?;
        self.installed = false;
        Ok(())
    }

    /// Put addresses in the allowed sets: `add` are new, `refresh` are there
    /// already and get the new timeout (replaced in the same transaction).
    pub fn admit(
        &mut self,
        add: &[(IpAddr, Duration)],
        refresh: &[(IpAddr, Duration)],
    ) -> io::Result<()> {
        if add.is_empty() && refresh.is_empty() {
            return Ok(());
        }
        let mut b = Batch::new(self.seq);
        for set in [SET4, SET6] {
            let gone: Vec<(IpAddr, Option<Duration>)> = refresh
                .iter()
                .filter(|(a, _)| set_of(*a) == set)
                .map(|(a, _)| (*a, None))
                .collect();
            element_message(&mut b, NFT_MSG_DELSETELEM, 0, set, &gone);
            let put: Vec<(IpAddr, Option<Duration>)> = add
                .iter()
                .chain(refresh)
                .filter(|(a, _)| set_of(*a) == set)
                .map(|(a, t)| (*a, Some(*t)))
                .collect();
            element_message(&mut b, NFT_MSG_NEWSETELEM, NLM_F_CREATE, set, &put);
        }
        self.commit(b)
    }

    /// Empty both sets: every address must be answered again.
    pub fn flush(&mut self) -> io::Result<()> {
        if !self.installed {
            return Ok(());
        }
        let mut b = Batch::new(self.seq);
        for set in [SET4, SET6] {
            b.msg(NFT_MSG_DELSETELEM, 0, |a| {
                a.str(NFTA_SET_ELEM_LIST_TABLE, TABLE);
                a.str(NFTA_SET_ELEM_LIST_SET, set);
            });
        }
        self.commit(b)
    }

    fn commit(&mut self, batch: Batch) -> io::Result<()> {
        let (bytes, acked, next) = batch.finish();
        self.seq = next;
        // SAFETY: an all-zero sockaddr_nl is valid; the fields set below
        // address the kernel.
        let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        kernel.nl_family = libc::sa_family_t::try_from(libc::AF_NETLINK).unwrap_or_default();
        // SAFETY: `bytes` and `kernel` are valid for the duration of the call.
        let sent = unsafe {
            libc::sendto(
                self.sock.as_raw_fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
                0,
                (&raw const kernel).cast(),
                socklen::<libc::sockaddr_nl>(),
            )
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        self.await_acks(&acked)
    }

    fn await_acks(&self, expected: &[u32]) -> io::Result<()> {
        let mut pending: Vec<u32> = expected.to_vec();
        let mut buf = vec![0_u8; 64 * 1024];
        while !pending.is_empty() {
            // SAFETY: `buf` is valid for writes of its length.
            let n =
                unsafe { libc::recv(self.sock.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            let Ok(n) = usize::try_from(n) else {
                let e = io::Error::last_os_error();
                return Err(if e.kind() == io::ErrorKind::WouldBlock {
                    io::Error::new(io::ErrorKind::TimedOut, "nf_tables did not answer")
                } else {
                    e
                });
            };
            for (seq, error) in errors_in(buf.get(..n).unwrap_or_default()) {
                if !pending.contains(&seq) {
                    continue;
                }
                if error != 0 {
                    return Err(io::Error::from_raw_os_error(error.saturating_neg()));
                }
                pending.retain(|s| *s != seq);
            }
        }
        Ok(())
    }
}

/// `(seq, error)` of every `NLMSG_ERROR` in a datagram; error 0 is an ack.
fn errors_in(datagram: &[u8]) -> Vec<(u32, i32)> {
    let mut out = Vec::new();
    let mut at = 0_usize;
    while let Some(head) = datagram.get(at..at.saturating_add(16)) {
        let word =
            |i: usize| -> Option<[u8; 4]> { head.get(i..i.saturating_add(4))?.try_into().ok() };
        let (Some(len), Some(kind)) = (
            word(0).map(u32::from_ne_bytes),
            head.get(4..6)
                .and_then(|b| <[u8; 2]>::try_from(b).ok())
                .map(u16::from_ne_bytes),
        ) else {
            break;
        };
        let seq = word(8).map(u32::from_ne_bytes).unwrap_or_default();
        let Ok(len) = usize::try_from(len) else { break };
        if len < 16 {
            break;
        }
        if kind == NLMSG_ERROR
            && let Some(code) = datagram
                .get(at.saturating_add(16)..at.saturating_add(20))
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map(i32::from_ne_bytes)
        {
            out.push((seq, code));
        }
        at = at.saturating_add(len.saturating_add(3) & !3);
    }
    out
}

fn socklen<T>() -> libc::socklen_t {
    libc::socklen_t::try_from(std::mem::size_of::<T>()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ruleset_renders_as_nft_would_print_it() {
        let text = render(&rules());
        let expected = "\
table inet tilo_egress {
\tset allowed4 { type ipv4_addr; flags timeout; }
\tset allowed6 { type ipv6_addr; flags timeout; }
\tchain output {
\t\ttype filter hook output priority 0; policy drop;
\t\toif \"lo\" accept
\t\tct state established,related accept
\t\tmeta l4proto udp udp dport 53 reject with icmpx admin-prohibited
\t\tmeta l4proto tcp tcp dport 53 reject with tcp reset
\t\tmeta nfproto ipv6 meta l4proto ipv6-icmp icmpv6 type 133-136 accept
\t\tmeta nfproto ipv4 meta l4proto tcp ip daddr @allowed4 accept
\t\tmeta nfproto ipv4 meta l4proto udp ip daddr @allowed4 accept
\t\tmeta nfproto ipv6 meta l4proto tcp ip6 daddr @allowed6 accept
\t\tmeta nfproto ipv6 meta l4proto udp ip6 daddr @allowed6 accept
\t\tmeta l4proto tcp reject with tcp reset
\t\treject with icmpx admin-prohibited
\t}
}
";
        assert_eq!(text, expected);
    }

    #[test]
    fn nothing_but_the_set_lookup_accepts_traffic_leaving_the_namespace() {
        for r in rules().iter().filter(|r| r.verdict == Verdict::Accept) {
            let ok = r.matches.contains(&Match::ToLoopback)
                || r.matches.contains(&Match::Established)
                || r.matches.iter().any(|m| matches!(m, Match::DestAllowed(_)))
                || r.matches
                    .iter()
                    .any(|m| matches!(m, Match::Icmpv6Types(133, 136)));
            assert!(ok, "{r:?} accepts more than it should");
        }
        let all = rules();
        let dns = all
            .iter()
            .rposition(|r| r.matches.contains(&Match::DestPort(53)))
            .unwrap();
        let first_set = all
            .iter()
            .position(|r| r.matches.iter().any(|m| matches!(m, Match::DestAllowed(_))))
            .unwrap();
        assert!(dns < first_set);
    }

    fn attrs(mut buf: &[u8]) -> Vec<(u16, Vec<u8>)> {
        let mut out = Vec::new();
        while buf.len() >= 4 {
            let len = usize::from(u16::from_ne_bytes([buf[0], buf[1]]));
            let kind = u16::from_ne_bytes([buf[2], buf[3]]);
            out.push((kind, buf[4..len].to_vec()));
            let next = (len + 3) & !3;
            buf = &buf[next.min(buf.len())..];
        }
        out
    }

    fn messages(mut buf: &[u8]) -> Vec<(u16, u16, u8, Vec<u8>)> {
        let mut out = Vec::new();
        while buf.len() >= 20 {
            let len = u32::from_ne_bytes(buf[0..4].try_into().unwrap()) as usize;
            let kind = u16::from_ne_bytes([buf[4], buf[5]]);
            let flags = u16::from_ne_bytes([buf[6], buf[7]]);
            out.push((kind, flags, buf[16], buf[20..len].to_vec()));
            buf = &buf[len..];
        }
        out
    }

    #[test]
    fn the_install_batch_is_one_transaction_of_table_sets_chain_and_rules() {
        let mut b = Batch::new(100);
        add_table_messages(&mut b, &rules());
        let (bytes, acked, next) = b.finish();
        let msgs = messages(&bytes);
        let kinds: Vec<u16> = msgs.iter().map(|m| m.0).collect();
        let nft = |m: u16| (NFNL_SUBSYS_NFTABLES << 8) | m;
        let mut expected = vec![
            NFNL_MSG_BATCH_BEGIN,
            nft(NFT_MSG_NEWTABLE),
            nft(NFT_MSG_NEWSET),
            nft(NFT_MSG_NEWSET),
            nft(NFT_MSG_NEWCHAIN),
        ];
        expected.extend(std::iter::repeat_n(nft(NFT_MSG_NEWRULE), rules().len()));
        expected.push(NFNL_MSG_BATCH_END);
        assert_eq!(kinds, expected);
        assert_eq!(acked.len(), msgs.len() - 2);
        assert_eq!(acked.first(), Some(&101));
        assert_eq!(next, 100 + u32::try_from(msgs.len()).unwrap());
        let chain = attrs(&msgs[4].3);
        assert!(chain.contains(&(NFTA_CHAIN_POLICY, NF_DROP.to_be_bytes().to_vec())));
        let hook = chain
            .iter()
            .find(|(k, _)| *k == NFTA_CHAIN_HOOK | NLA_F_NESTED)
            .unwrap();
        assert!(
            attrs(&hook.1).contains(&(NFTA_HOOK_HOOKNUM, NF_INET_LOCAL_OUT.to_be_bytes().to_vec()))
        );
        let set = attrs(&msgs[2].3);
        assert!(set.contains(&(NFTA_SET_FLAGS, NFT_SET_TIMEOUT.to_be_bytes().to_vec())));
        assert!(set.contains(&(NFTA_SET_KEY_LEN, 4_u32.to_be_bytes().to_vec())));
        assert!(msgs[1..msgs.len() - 1].iter().all(|m| m.2 == NFPROTO_INET));
    }

    fn guest_filter() -> GuestFilter {
        GuestFilter {
            guest: "tap0".into(),
            uplink: "pt0".into(),
            dns: std::net::Ipv4Addr::new(10, 0, 2, 3),
            endpoint: Some((std::net::Ipv4Addr::new(10, 0, 2, 4), 7704)),
            blocked: vec![
                (std::net::Ipv4Addr::new(10, 0, 0, 0), 8),
                (std::net::Ipv4Addr::new(169, 254, 169, 254), 16),
                (std::net::Ipv4Addr::new(168, 63, 129, 16), 32),
                (std::net::Ipv4Addr::new(127, 0, 0, 0), 8),
            ],
        }
    }

    fn passes(filter: &GuestFilter, address: std::net::Ipv4Addr, port: u16, proto: Proto) -> bool {
        for rule in filter.rules() {
            let matched = rule.matches.iter().all(|m| match *m {
                GuestMatch::InIface(name) => name == ifname(&filter.guest),
                GuestMatch::Ipv4 => true,
                GuestMatch::DestNet(network, prefix) => {
                    u32::from(address) & u32::from_be_bytes(mask4(prefix))
                        == u32::from(network) & u32::from_be_bytes(mask4(prefix))
                }
                GuestMatch::Proto(p) => p == proto,
                GuestMatch::DestPort(port_here) => port_here == port,
                GuestMatch::Arp => false,
            });
            if matched {
                return rule.verdict == GuestVerdict::Accept;
            }
        }
        false
    }

    #[test]
    fn a_vm_reaches_the_one_endpoint_it_is_given_and_no_other_private_address() {
        let filter = guest_filter();
        let endpoint = std::net::Ipv4Addr::new(10, 0, 2, 4);
        for proto in [Proto::Tcp, Proto::Udp] {
            assert!(
                passes(&filter, endpoint, 7704, proto),
                "the relay's endpoint passes, {proto:?}"
            );
            assert!(
                !passes(&filter, endpoint, 7705, proto),
                "and no other port on it"
            );
        }
        for address in ["10.0.2.5", "10.244.1.5", "169.254.169.254", "127.0.0.1"] {
            let address: std::net::Ipv4Addr = address.parse().unwrap();
            assert!(!passes(&filter, address, 7704, Proto::Udp), "{address}");
        }
        assert!(
            passes(&filter, "1.1.1.1".parse().unwrap(), 443, Proto::Tcp),
            "the internet still passes"
        );
        assert!(
            passes(
                &filter,
                std::net::Ipv4Addr::new(10, 0, 2, 3),
                53,
                Proto::Udp
            ),
            "and so does DNS to the uplink's forwarder"
        );
        let linux = GuestFilter {
            endpoint: None,
            ..guest_filter()
        };
        assert!(!passes(&linux, endpoint, 7704, Proto::Udp));
        assert!(!linux.render().contains("dport 7704"));
    }

    #[test]
    fn a_vms_bridge_passes_answers_dns_and_the_internet_and_drops_blocked_networks() {
        let expected = "\
table bridge tilo_guest {
\tchain forward {
\t\ttype filter hook forward priority 0; policy drop;
\t\tiifname \"pt0\" accept
\t\tiifname \"tap0\" ether type arp accept
\t\tiifname \"tap0\" ether type ip ip daddr 10.0.2.3 meta l4proto udp udp dport 53 accept
\t\tiifname \"tap0\" ether type ip ip daddr 10.0.2.3 meta l4proto tcp tcp dport 53 accept
\t\tiifname \"tap0\" ether type ip ip daddr 10.0.2.4 meta l4proto udp udp dport 7704 accept
\t\tiifname \"tap0\" ether type ip ip daddr 10.0.2.4 meta l4proto tcp tcp dport 7704 accept
\t\tiifname \"tap0\" ether type ip ip daddr 10.0.0.0/8 drop
\t\tiifname \"tap0\" ether type ip ip daddr 169.254.0.0/16 drop
\t\tiifname \"tap0\" ether type ip ip daddr 168.63.129.16 drop
\t\tiifname \"tap0\" ether type ip ip daddr 127.0.0.0/8 drop
\t\tiifname \"tap0\" ether type ip accept
\t}
}
";
        assert_eq!(guest_filter().render(), expected);
        assert_eq!(
            guest_exprs(GuestMatch::DestNet(
                std::net::Ipv4Addr::new(169, 254, 169, 254),
                16
            )),
            [
                Expr::Payload {
                    base: NFT_PAYLOAD_NETWORK_HEADER,
                    offset: 16,
                    len: 4
                },
                Expr::Bitwise {
                    mask: vec![255, 255, 0, 0],
                    xor: vec![0; 4]
                },
                Expr::Cmp(NFT_CMP_EQ, vec![169, 254, 0, 0]),
            ]
        );
        let mut b = Batch::of_family(1, NFPROTO_BRIDGE);
        add_guest_messages(&mut b, &guest_filter());
        let (bytes, _, _) = b.finish();
        let msgs = messages(&bytes);
        assert_eq!(msgs.len(), 2 + 2 + guest_filter().rules().len());
        assert!(
            msgs[1..msgs.len() - 1]
                .iter()
                .all(|m| m.2 == NFPROTO_BRIDGE)
        );
        let chain = attrs(&msgs[2].3);
        assert!(chain.contains(&(NFTA_CHAIN_POLICY, NF_DROP.to_be_bytes().to_vec())));
    }

    #[test]
    fn a_rule_encodes_its_expressions_in_order() {
        let mut b = Batch::new(1);
        add_table_messages(&mut b, &rules());
        let (bytes, ..) = b.finish();
        let msgs = messages(&bytes);
        // The v4 TCP set rule: nfproto, l4proto, payload daddr, lookup, accept.
        let set_rule = &msgs[5 + 5];
        let exprs_attr = attrs(&set_rule.3)
            .into_iter()
            .find(|(k, _)| *k == NFTA_RULE_EXPRESSIONS | NLA_F_NESTED)
            .unwrap();
        let names: Vec<String> = attrs(&exprs_attr.1)
            .iter()
            .map(|(_, e)| {
                let name = &attrs(e)[0].1;
                String::from_utf8(name[..name.len() - 1].to_vec()).unwrap()
            })
            .collect();
        assert_eq!(
            names,
            [
                "meta",
                "cmp",
                "meta",
                "cmp",
                "payload",
                "lookup",
                "immediate"
            ]
        );
    }

    #[test]
    fn admitting_replaces_refreshed_elements_in_the_same_transaction() {
        let mut b = Batch::new(1);
        let v4: IpAddr = "192.0.2.1".parse().unwrap();
        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        for set in [SET4, SET6] {
            let gone: Vec<_> = [(v6, None)]
                .into_iter()
                .filter(|(a, _)| set_of(*a) == set)
                .collect();
            element_message(&mut b, NFT_MSG_DELSETELEM, 0, set, &gone);
            let put: Vec<_> = [
                (v4, Some(Duration::from_secs(60))),
                (v6, Some(Duration::from_secs(300))),
            ]
            .into_iter()
            .filter(|(a, _)| set_of(*a) == set)
            .collect();
            element_message(&mut b, NFT_MSG_NEWSETELEM, NLM_F_CREATE, set, &put);
        }
        let (bytes, ..) = b.finish();
        let msgs = messages(&bytes);
        let nft = |m: u16| (NFNL_SUBSYS_NFTABLES << 8) | m;
        let kinds: Vec<u16> = msgs.iter().map(|m| m.0).collect();
        assert_eq!(
            kinds,
            [
                NFNL_MSG_BATCH_BEGIN,
                nft(NFT_MSG_NEWSETELEM),
                nft(NFT_MSG_DELSETELEM),
                nft(NFT_MSG_NEWSETELEM),
                NFNL_MSG_BATCH_END
            ]
        );
        // The v4 element carries its key and a 60 000 ms timeout.
        let list = attrs(&msgs[1].3)
            .into_iter()
            .find(|(k, _)| *k == NFTA_SET_ELEM_LIST_ELEMENTS | NLA_F_NESTED)
            .unwrap();
        let elem = attrs(&attrs(&list.1)[0].1);
        assert!(elem.contains(&(NFTA_SET_ELEM_TIMEOUT, 60_000_u64.to_be_bytes().to_vec())));
        let key = elem
            .iter()
            .find(|(k, _)| *k == NFTA_SET_ELEM_KEY | NLA_F_NESTED)
            .unwrap();
        assert_eq!(attrs(&key.1), vec![(NFTA_DATA_VALUE, vec![192, 0, 2, 1])]);
    }

    #[test]
    fn errors_are_read_from_a_datagram_of_acks() {
        let mut dgram = Vec::new();
        for (seq, code) in [(5_u32, 0_i32), (6, -1)] {
            dgram.extend_from_slice(&36_u32.to_ne_bytes());
            dgram.extend_from_slice(&NLMSG_ERROR.to_ne_bytes());
            dgram.extend_from_slice(&0_u16.to_ne_bytes());
            dgram.extend_from_slice(&seq.to_ne_bytes());
            dgram.extend_from_slice(&0_u32.to_ne_bytes());
            dgram.extend_from_slice(&code.to_ne_bytes());
            dgram.extend_from_slice(&[0; 16]);
        }
        assert_eq!(errors_in(&dgram), vec![(5, 0), (6, -1)]);
        assert!(errors_in(&dgram[..10]).is_empty());
    }
}

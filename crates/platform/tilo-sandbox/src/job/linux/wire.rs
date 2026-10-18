//! The job's init is a new program, not a copy of the worker: the worker
//! writes what the init does ([`Plan`], and where its descriptors are) to a
//! memory file in this encoding, and the init reads it back. Both ends are
//! the same binary, so the encoding is native-endian and unversioned.

use std::ffi::CString;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

use super::plan::{Op, Plan, Rule};

/// The init's descriptor numbers.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Control {
    pub sync: RawFd,
    pub status: RawFd,
    pub report: RawFd,
    pub landlock: Option<RawFd>,
    pub trees_at: RawFd,
    /// Every descriptor number the job keeps (its stdio and extras
    /// included), which the init closes once the command runs.
    pub kept: Vec<RawFd>,
}

#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_ne_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_ne_bytes());
    }
    fn fd(&mut self, fd: RawFd) {
        self.0.extend_from_slice(&fd.to_ne_bytes());
    }
    fn len(&mut self, n: usize) {
        self.u64(u64::try_from(n).unwrap_or(u64::MAX));
    }
    fn bytes(&mut self, b: &[u8]) {
        self.len(b.len());
        self.0.extend_from_slice(b);
    }
    fn c(&mut self, c: &CString) {
        self.bytes(c.as_bytes());
    }
    fn opt_c(&mut self, c: Option<&CString>) {
        match c {
            Some(c) => {
                self.u32(1);
                self.c(c);
            }
            None => self.u32(0),
        }
    }
    fn cs(&mut self, cs: &[CString]) {
        self.len(cs.len());
        for c in cs {
            self.c(c);
        }
    }
}

struct In<'a>(&'a [u8]);

impl<'a> In<'a> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_at_checked(N)?;
        self.0 = rest;
        head.try_into().ok()
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_ne_bytes)
    }
    fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_ne_bytes)
    }
    fn fd(&mut self) -> Option<RawFd> {
        self.take().map(RawFd::from_ne_bytes)
    }
    fn len(&mut self) -> Option<usize> {
        usize::try_from(self.u64()?).ok()
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.len()?;
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }
    fn c(&mut self) -> Option<CString> {
        CString::new(self.bytes()?).ok()
    }
    #[allow(
        clippy::option_option,
        reason = "outer: the input ended or is malformed; inner: the value"
    )]
    fn opt_c(&mut self) -> Option<Option<CString>> {
        match self.u32()? {
            0 => Some(None),
            _ => self.c().map(Some),
        }
    }
    fn cs(&mut self) -> Option<Vec<CString>> {
        let n = self.len()?;
        (0..n).map(|_| self.c()).collect()
    }
}

fn op(out: &mut Out, op: &Op) {
    match op {
        Op::Mkdir(p) => {
            out.u32(0);
            out.c(p);
        }
        Op::Touch(p) => {
            out.u32(1);
            out.c(p);
        }
        Op::Symlink { target, path } => {
            out.u32(2);
            out.c(target);
            out.c(path);
        }
        Op::Mount {
            src,
            dst,
            fstype,
            flags,
            data,
        } => {
            out.u32(3);
            out.opt_c(src.as_ref());
            out.c(dst);
            out.opt_c(fstype.as_ref());
            out.u64(*flags);
            out.opt_c(data.as_ref());
        }
        Op::SetAttr {
            path,
            set,
            recursive,
        } => {
            out.u32(4);
            out.c(path);
            out.u64(*set);
            out.u32(u32::from(*recursive));
        }
        Op::Attach { tree, dst } => {
            out.u32(5);
            out.len(*tree);
            out.c(dst);
        }
        Op::Write { path, contents } => {
            out.u32(6);
            out.c(path);
            out.bytes(contents);
        }
    }
}

fn read_op(r: &mut In<'_>) -> Option<Op> {
    Some(match r.u32()? {
        0 => Op::Mkdir(r.c()?),
        1 => Op::Touch(r.c()?),
        2 => Op::Symlink {
            target: r.c()?,
            path: r.c()?,
        },
        3 => Op::Mount {
            src: r.opt_c()?,
            dst: r.c()?,
            fstype: r.opt_c()?,
            flags: libc::c_ulong::try_from(r.u64()?).ok()?,
            data: r.opt_c()?,
        },
        4 => Op::SetAttr {
            path: r.c()?,
            set: r.u64()?,
            recursive: r.u32()? != 0,
        },
        5 => Op::Attach {
            tree: r.len()?,
            dst: r.c()?,
        },
        6 => Op::Write {
            path: r.c()?,
            contents: r.bytes()?.to_vec(),
        },
        _ => return None,
    })
}

/// What the init needs of `plan`, and its descriptors.
pub fn encode(plan: &Plan, control: &Control) -> Vec<u8> {
    let mut out = Out::default();
    out.fd(control.sync);
    out.fd(control.status);
    out.fd(control.report);
    out.fd(control.landlock.unwrap_or(-1));
    out.fd(control.trees_at);
    out.len(control.kept.len());
    for fd in &control.kept {
        out.fd(*fd);
    }
    out.c(&plan.stage);
    out.c(&plan.stage_old);
    out.c(&plan.stage_new);
    out.len(plan.ops.len());
    for o in &plan.ops {
        op(&mut out, o);
    }
    match plan.become_ids {
        Some((uid, gid)) => {
            out.u32(1);
            out.u32(uid);
            out.u32(gid);
        }
        None => out.u32(0),
    }
    out.c(&plan.new_root);
    out.c(&plan.hostname);
    out.c(&plan.cwd);
    out.cs(&plan.candidates);
    out.cs(&plan.argv);
    out.cs(&plan.envp);
    let rules = plan.landlock.as_ref().map_or(&[][..], |(_, r)| r);
    out.len(rules.len());
    for rule in rules {
        out.c(&rule.path);
        out.u64(rule.access);
    }
    out.len(plan.seccomp.len());
    for program in &plan.seccomp {
        out.len(program.len());
        for f in program {
            out.u32(u32::from(f.code));
            out.u32(u32::from(f.jt));
            out.u32(u32::from(f.jf));
            out.u32(f.k);
        }
    }
    out.u32(plan.cap_last);
    out.0
}

/// The plan and descriptors [`encode`] wrote, in the init. The Landlock
/// ruleset becomes the init's own descriptor; the plan has no idmapped
/// trees of its own (they are attached by number from `trees_at`).
///
/// # Safety
/// Only in the init, which owns the descriptor numbers in `bytes`.
pub unsafe fn decode(bytes: &[u8]) -> Option<(Plan, Control)> {
    let mut r = In(bytes);
    let mut control = Control {
        sync: r.fd()?,
        status: r.fd()?,
        report: r.fd()?,
        landlock: Some(r.fd()?).filter(|fd| *fd >= 0),
        trees_at: r.fd()?,
        kept: Vec::new(),
    };
    let kept = r.len()?;
    control.kept = (0..kept).map(|_| r.fd()).collect::<Option<_>>()?;
    let stage = r.c()?;
    let stage_old = r.c()?;
    let stage_new = r.c()?;
    let n = r.len()?;
    let ops = (0..n).map(|_| read_op(&mut r)).collect::<Option<_>>()?;
    let become_ids = match r.u32()? {
        0 => None,
        _ => Some((r.u32()?, r.u32()?)),
    };
    let new_root = r.c()?;
    let hostname = r.c()?;
    let cwd = r.c()?;
    let candidates = r.cs()?;
    let argv = r.cs()?;
    let envp = r.cs()?;
    let n = r.len()?;
    let rules: Vec<Rule> = (0..n)
        .map(|_| {
            Some(Rule {
                path: r.c()?,
                access: r.u64()?,
            })
        })
        .collect::<Option<_>>()?;
    let n = r.len()?;
    let mut seccomp = Vec::new();
    for _ in 0..n {
        let len = r.len()?;
        let program = (0..len)
            .map(|_| {
                Some(libc::sock_filter {
                    code: u16::try_from(r.u32()?).ok()?,
                    jt: u8::try_from(r.u32()?).ok()?,
                    jf: u8::try_from(r.u32()?).ok()?,
                    k: r.u32()?,
                })
            })
            .collect::<Option<_>>()?;
        seccomp.push(program);
    }
    let cap_last = r.u32()?;
    if !r.0.is_empty() {
        return None;
    }
    // SAFETY: the caller owns the descriptor the worker put at this number.
    let landlock = control
        .landlock
        .map(|fd| (unsafe { OwnedFd::from_raw_fd(fd) }, rules));
    let plan = Plan {
        stage,
        stage_old,
        stage_new,
        ops,
        trees: Vec::new(),
        unmapped: Vec::new(),
        become_ids,
        new_root,
        hostname,
        cwd,
        candidates,
        argv,
        envp,
        landlock,
        seccomp,
        cap_last,
    };
    Some((plan, control))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_reads_back_as_written() {
        let c = |s: &str| CString::new(s).unwrap();
        let plan = Plan {
            stage: c("/tmp"),
            stage_old: c("oldroot"),
            stage_new: c("newroot"),
            ops: vec![
                Op::Mkdir(c("/newroot/a")),
                Op::Mount {
                    src: None,
                    dst: c("/newroot/tmp"),
                    fstype: Some(c("tmpfs")),
                    flags: 6,
                    data: Some(c("size=1m")),
                },
                Op::Attach {
                    tree: 2,
                    dst: c("/newroot/w"),
                },
                Op::Write {
                    path: c("/newroot/etc/passwd"),
                    contents: b"root:x:0:0::/:/bin/sh\n\0".to_vec(),
                },
            ],
            trees: Vec::new(),
            unmapped: Vec::new(),
            become_ids: Some((0, 5)),
            new_root: c("/newroot"),
            hostname: c("job"),
            cwd: c("/"),
            candidates: vec![c("/bin/sh")],
            argv: vec![c("sh"), c("-c"), c("true")],
            envp: vec![c("A=b")],
            landlock: None,
            seccomp: vec![vec![libc::sock_filter {
                code: 6,
                jt: 1,
                jf: 2,
                k: 0x7fff_0000,
            }]],
            cap_last: 40,
        };
        let control = Control {
            sync: 7,
            status: 8,
            report: 9,
            landlock: None,
            trees_at: 11,
            kept: vec![0, 1, 2, 7, 8, 9],
        };
        let bytes = encode(&plan, &control);
        // SAFETY: no descriptor is taken over (no Landlock ruleset).
        let (back, ctl) = unsafe { decode(&bytes) }.unwrap();
        assert_eq!(ctl, control);
        assert_eq!(format!("{:?}", back.ops), format!("{:?}", plan.ops));
        assert_eq!(back.argv, plan.argv);
        assert_eq!(back.become_ids, plan.become_ids);
        assert_eq!(back.seccomp.len(), 1);
        assert_eq!(back.seccomp[0][0].k, 0x7fff_0000);
        // SAFETY: as above.
        assert!(unsafe { decode(bytes.split_last().unwrap().1) }.is_none());
    }
}

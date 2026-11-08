p = "/var/home/xaver/TiloData/src/tilo/.claude/worktrees/agent-ad45286f4567142d5/crates/session/session/src/agent_door.rs"
s = open(p).read()
start = s.index("        let (actor, reads_only) = Self::at_session(caller);")
end = s.index("    /// The session's own actions as rows, as the kind of publisher its own door is", start)
s = s[:start] + "    }\n\n" + s[end:]
# The old helper functions.
start = s.index("    /// Who `caller` acts as at the session's doors, and whether it may only read there:")
end = s.index("    /// The session's own actions as rows, as the kind of publisher its own door is", start)
s = s[:start] + s[end:]
s = s.replace(
    "use tilo_runtime::session_door::{Consent, Hello, SESSION_PROVIDER};",
    "use tilo_runtime::session_door::{Consent, Hello};",
    1,
)
open(p, "w").write(s)
print("ok")

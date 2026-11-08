p = "/var/home/xaver/TiloData/src/tilo/.claude/worktrees/agent-ad45286f4567142d5/crates/session/session/src/agent_door.rs"
s = open(p).read()

start = s.index("        match &self.state {\n        match &self.state {\n")
end = s.index("    /// The session's own actions as rows, as the kind of publisher its own door is", start)
s = s[:start] + """        match &self.state {
            Some(state) if at_seat => state.action_invoke_local_person(caller, &request),
            Some(state) => state.action_invoke(caller, None, None, &request),
            None => Pending::ready(Err(ActionHostError::Unavailable {
                reason: "this desktop's apps are not running here".to_owned(),
            })),
        }
    }

""" + s[end:]

# Drop the old `at_session` and `session_page`.
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

//! `tilo do --in-person` on a computer that holds its person's root and no server keeps anything
//! of theirs: the call goes to this computer's own daemon, which takes it as the person's own, and
//! a key named beside it is refused.

use crate::language::host;
use crate::language::hosted_turns::CHAT_PACKAGE;

/// The person's own write, made in person on the computer that founded them, is in their
/// workspace there; the same call naming a key is refused before it reaches anything.
#[test]
fn the_person_acts_in_person_on_their_own_computer_with_no_server() {
    let computer = host::Second::founded("Ada", &[CHAT_PACKAGE], &[]);
    let workspace = computer.workspace.clone();
    let made = computer.do_in(
        "so.tilo.chat#new_channel",
        &["--in-person", "name=In person"],
    );
    assert!(
        made.contains("label: In person") && made.contains("kind: tl.so.tilo.chat.TextChannel"),
        "the person's call answers the channel it made: {made}"
    );
    let listed = computer.run(&["ls", "-w", &workspace, "chat"]);
    let listed_said = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listed.status.success() && listed_said.contains("In person"),
        "the person's write is in their workspace: {listed_said}{}",
        String::from_utf8_lossy(&listed.stderr)
    );

    let keyed = computer.run(&[
        "--key-file",
        "/nonexistent/key",
        "do",
        "-w",
        &workspace,
        "--in-person",
        "so.tilo.chat#new_channel",
        "name=Keyed",
    ]);
    let refused = String::from_utf8_lossy(&keyed.stderr);
    assert!(
        !keyed.status.success() && refused.contains("--in-person"),
        "a key named beside --in-person is refused: {refused}"
    );
}

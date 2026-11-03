/// The agent composer: ask an agent from anywhere, about what the person points at or has
/// selected, and see the tasks running and what they did. It is the same one agent every way
/// in (the keys, a window's menu, the overview) reaches: the composer names the agent, the
/// subject and what was typed, and hands the session the ask.
///
/// It stands on the floating material, at the foot of the desk over what it is about; its field
/// is its header and Send and the keys are its foot. What the tasks did is task-legibility.md's:
/// each running task shows its state and what it is on, and opening one shows its log, where
/// each step is the program it ran and Undo reverses the run whole.
view composer(d: Desk) {
    lens words: Text = ""
    lens subject: Text = ""
    let live = d.tasks.filter(t -> t.state != "done")
    let agents = d.agents.filter(a -> a.state != "done")
    ui.FloatingPanel {
        header: {
            row(gap: .related, align: .center, wrap: true) {
                ui.Text(
                    if subject != "" { "Ask about {subject}" } else { "Ask an agent" },
                    role: .subheading,
                )
                spacer
                ui.Badge(count: live.len())
            }
        }
        body: {
            ui.TextInput(value: words, placeholder: "What should it do?", lines: 3) {
                on submit: session.ask(title: words, agent: agent_key(agents)).then(set(words, ""))
            }
            if live.is_empty() {
                ui.EmptyState(
                    "No task is running",
                    message: "What you ask becomes one, and its log stays here",
                    icon: .bot,
                    inline: true,
                )
            } else {
                ui.Section("Running") {
                    for t in live {
                        task_line(d, t)
                    }
                }
            }
            if not agents.is_empty() {
                ui.Section("Agents here") {
                    for a in agents {
                        agent_line(d, a)
                    }
                }
            }
        }
        footer: {
            row(gap: .related, align: .center, wrap: true) {
                ui.Button("Send", icon: .send, role: .primary) {
                    on press: session.ask(title: words, agent: agent_key(agents))
                }
                ui.Button("What I am on", icon: .paperclip, role: .plain, size: .small) {
                    on press: device.show_surface(device.desk, surface: "overview")
                }
                spacer
                ui.InputHint(["Enter"])
                ui.Text("Send", role: .caption, secondary: true)
            }
        }
    }
}

/// One running task: what it is on, its state, and the way into its log.
view task_line(d: Desk, t: Task) {
    ui.ListRow(
        t.title,
        secondary: .some(t.state),
        icon: if t.state == "waiting" { .pause } else { .bot },
    )
}

/// The agent an ask goes to: the first here that is not done, else the workspace's own.
fn agent_key(agents: List<Agent>) -> Text {
    return agents.first()?.key ?? "default"
}

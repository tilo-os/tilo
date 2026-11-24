# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-console = Console

d-so-tilo-console-keywords = agents, agent runs, mcp, connect an agent, stop agent, undo agent, grants, revoke, agenten, läufe, verbinden

d-cbpvmmnd = Window
    .doc = The window: which place it opens in.
    .noun = { $count ->
        [one] window
       *[other] windows
    }

d-e6xn3nds = Place
    .doc = The place the window opens in: `agents`, `workspaces`, `connect` or `usage`.

d-co6xee7i = Place title
    .w-agents = Agents
    .w-workspaces = Workspaces
    .w-connect = Connect
    .w-usage = Usage

d-y4vctfc6 = Made by
    .w-made-by-you = Made by you
    .w-made-by = Made by { $agent }

d-a4jk4ymn = Reach line
    .w-only = Only { $name }
    .w-only-one-workspace = Only one workspace
    .w-every-workspace-and-may-make-new = Every workspace, and may make new ones

d-vwss2xgt = Grant status
    .w-working-now = Working now
    .w-sessions-working-now = { $working } sessions working now
    .w-last-active = Last active { $when }
    .w-let-in = Let in { $when }

d-z3srezcw = Now line
    .w-running = Running { $now }…
    .w-editing = Editing { $now }
    .w-on = On { $now }
    .w-ran = Ran { $now }

d-rkm2dwcu = Started by
    .w-started-by-you = Started by you
    .w-started-by = Started by { $name }

d-fkxqnf4v = Actions title
    .w-1-action = 1 action
    .w-actions = { $count } actions

d-i3edslph = Size line
    .w-1-change = 1 change
    .w-changes = { $changes } changes

d-4h4nigsj = Console
    .w-console = Console
    .w-agents = Agents
    .w-workspaces = Workspaces
    .w-connect = Connect
    .w-usage = Usage
    .w-back = Back
    .w-last-worked = Last worked { $when }
    .w-no-sessions-yet = No sessions yet
    .w-access = Access
    .w-let-it-go-on = Let it go on
    .w-revoke = Revoke
    .w-sessions = Sessions
    .w-why = Why
    .w-it-changed-nothing-yet = It changed nothing yet
    .w-in = in { $cwd }
    .w-its-output-has-not-reached-this = Its output has not reached this device yet.
    .w-it-printed-more-than-its-shell = It printed more than its shell kept.
    .w-reading-every-line = Reading every line…
    .w-show-less = Show less
    .w-show-all-output = Show all output
    .w-open = Open
    .w-reading-this-session = Reading this session
    .w-this-session-is-not-here = This session is not here
    .w-the-history-lists-no-such-session = The history lists no such session, or it worked in a workspace you are not in.
    .w-every-agent = Every agent
    .w-every-workspace = Every workspace
    .w-show-everything = Show everything
    .w-reading-your-workspaces-history = Reading your workspaces' history
    .w-no-sessions-match = No sessions match
    .w-no-session-of-this-agent-in = No session of this agent in this workspace. Show everything to see the rest.
    .w-no-agent-has-worked-here-yet = No agent has worked here yet
    .w-connect-an-agent-and-ask-it = Connect an agent and ask it for something: you can follow each of its sessions while it works.
    .w-working-now = Working now
    .w-earlier = Earlier
    .w-clients = Clients
    .w-connected-no-sessions-yet = Connected, no sessions yet

d-d22uzx3p = Commands
    .w-stop = Stop
    .w-take-over = Take over
    .w-undo-session = Undo session
    .w-it-can-be-undone-here = It can be undone here

d-om4pkkxd = Command lines

d-quelstem = Command status
    .w-running = Running…
    .w-exited-with-after = Exited with { $code } after { $took }
    .w-exited-with = Exited with { $code }

d-ceombaci = Diff lines

d-fmguxyaa = Compared
    .w-this-change-shows-no-difference-to = This change shows no difference to preview.
    .w-reading-what-changed = Reading what changed

d-mgxu3hat = Workspaces
    .w-no-workspaces-yet = No workspaces yet
    .w-make-one-in-tilo-or-ask = Make one in Tilo, or ask an agent to make one for its work.
    .w-workspaces = Workspaces
    .w-open-in-tilo = Open in Tilo

d-i24mkgkc = Connect
    .w-your-agent-works-here-as-you = Your agent works here as you once Tilo is set up on its computer. One line you paste into it does that and signs it in as you.
    .w-set-up-your-agent = Set up your agent
    .w-add-this-server-to-the-mcp = Add this server to the MCP configuration of your agent. It asks you to sign in once, then works in your workspaces as itself. Everything it does shows under Agents, where you can stop or undo it.
    .w-connect-an-agent = Connect an agent
    .w-server-address = Server address
    .w-any-mcp-client = Any MCP client
    .w-connected-agents = Connected agents
    .w-no-agent-connected-yet = No agent connected yet
    .w-and-operates-web-pages = { $reach_line }, and operates web pages
    .w-let-it-go-on = Let it go on
    .w-revoke = Revoke

d-bys5sbff = Usage
    .w-no-linux-machine-time-yet = No Linux machine time yet
    .w-an-agent-that-needs-linux-runs = An agent that needs Linux runs it on a machine of its own, and that time is counted.

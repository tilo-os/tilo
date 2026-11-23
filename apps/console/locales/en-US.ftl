console-title = Console
console-places = Console places
console-place-agents = Agents
console-place-workspaces = Workspaces
console-place-connect = Connect
console-place-usage = Usage

console-filter-agent = Agent
console-filter-workspace = Workspace
console-every-agent = Every agent
console-every-workspace = Every workspace
console-show-everything = Show everything

console-sessions-working = Working now
console-sessions-earlier = Earlier
console-sessions-pending = Reading your workspaces' history
console-sessions-empty = No agent has worked here yet
console-sessions-empty-message = Connect an agent and ask it for something: you can follow each of its sessions while it works.
console-sessions-none-here = No sessions match
console-sessions-none-here-message = No session of this agent in this workspace. Show everything to see the rest.
console-session-actions = { $count ->
    [one] 1 action
   *[other] { $count } actions
}
console-session-changes = { $count ->
    [one] 1 change
   *[other] { $count } changes
}
console-session-started-by-you = Started by you
console-session-started-by = Started by { $name }

console-session = Agent session
console-session-copy-link = Copy link
console-session-pending = Reading this session
console-session-missing = This session is not here
console-session-missing-message = The history lists no such session, or it worked in a workspace you are not in.

console-session-stop = Stop
console-session-take-over = Take over
console-session-undo = Undo session
console-session-why = Why
console-session-timeline = { $count ->
    [one] 1 action
   *[other] { $count } actions
}
console-action-open = Open
console-action-no-preview = This change shows no difference to preview.
console-command-exit = Exited with { $code }
console-command-exit-after = Exited with { $code } after { $took }
console-command-running = Running…
console-command-in = in { $cwd }
console-command-full = Show all output
console-command-less = Show less
console-command-output-unavailable = Its output has not reached this device yet.
console-now-running = Running { $command }…
console-now-editing = Editing { $path }
console-now-browsing = On { $page }
console-now-ran = Ran { $command }

console-workspaces-empty = No workspaces yet
console-workspaces-empty-message = Make one in Tilo, or ask an agent to make one for its work.
console-workspace-made-by-you = Made by you
console-workspace-made-by = Made by { $agent }
console-workspace-open = Open in Tilo

console-connect-lead = Add this server to the MCP configuration of your agent. It asks you to sign in once, then works in your workspaces as itself. Everything it does shows under Agents, where you can stop or undo it.
console-connect-heading = Connect an agent
console-connect-setup-lead = Your agent works here as you once Tilo is set up on its computer. One line you paste into it does that and signs it in as you.
console-connect-setup = Set up your agent
console-client-url = Server address
console-client-other = Any MCP client
console-client-every-session = Every { $client } session
console-grants-heading = Connected agents
console-grants-empty = No agent connected yet
console-reach-account = Every workspace, and may make new ones
console-reach-workspace = Only { $workspace }
console-grant-browser-pages = Browser pages
console-grant-running = { $count ->
    [one] Working now
   *[other] { $count } sessions working now
}
console-grant-last-active = Last active { $when }
console-grant-granted = Let in { $when }
console-grant-revoke = Revoke
console-grant-go-on = Let it go on

console-usage-empty = No Linux machine time yet
console-usage-empty-message = An agent that needs Linux runs it on a machine of its own, and that time is counted.

console-agent-sessions = Sessions
console-agent-access = Access
console-agent-no-sessions = No sessions yet
console-agent-last-worked = Last worked { $when }

## What the app is called in a window and in the launcher
console-app-title = Console

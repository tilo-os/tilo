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

console-runs-working = Working now
console-runs-earlier = Earlier
console-runs-pending = Reading your workspaces' history
console-runs-empty = No agent has worked here yet
console-runs-empty-message = Connect an agent and ask it for something: you can follow each of its runs while it works.
console-runs-none-here = No runs match
console-runs-none-here-message = No run of this agent in this workspace. Show everything to see the rest.
console-run-actions = { $count ->
    [one] 1 action
   *[other] { $count } actions
}
console-run-changes = { $count ->
    [one] 1 change
   *[other] { $count } changes
}

console-run = Agent run
console-run-copy-link = Copy link
console-run-pending = Reading this run
console-run-missing = This run is not here
console-run-missing-message = The history lists no such run, or it worked in a workspace you are not in.

console-run-stop = Stop
console-run-take-over = Take over
console-run-undo = Undo run
console-run-why = Why
console-run-timeline = { $count ->
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

console-connect-lead = Run one of these where your agent runs. It asks you to sign in once, then works in your workspaces as itself: everything it does shows under Agents, and you can stop or undo it.
console-connect-heading = Connect an agent
console-client-claude-code = Claude Code
console-client-codex = Codex
console-client-other = Any MCP client
console-client-every-run = Every { $client } run
console-grants-heading = Connected agents
console-grants-empty = No agent connected yet
console-grants-empty-message = Run one of the lines above where your agent runs, then let it in.
console-reach-account = Every workspace, and may make new ones
console-reach-workspace = Only { $workspace }
console-grant-browser-pages = Browser pages
console-grant-running = { $count ->
    [one] Working now
   *[other] { $count } runs working now
}
console-grant-last-active = Last active { $when }
console-grant-granted = Let in { $when }
console-grant-revoke = Revoke

console-usage-empty = No Linux machine time yet
console-usage-empty-message = An agent that needs Linux runs it on a machine of its own, and that time is counted.

console-agent-runs = Runs
console-agent-access = Access
console-agent-no-runs = No runs yet
console-agent-last-worked = Last worked { $when }

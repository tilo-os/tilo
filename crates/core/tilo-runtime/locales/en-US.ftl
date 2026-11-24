# The runtime's own words: the camera and microphone consent prompt.
# $device is `camera` or `microphone`.
device-consent-question = { $device ->
    [camera] Allow { $app } to use the camera?
   *[microphone] Allow { $app } to use the microphone?
}
device-consent-detail = { $device ->
    [camera] { $app } gets nothing from the camera unless you allow it. Your choice holds on this device until you take it back from any of your devices.
   *[microphone] { $app } gets nothing from the microphone unless you allow it. Your choice holds on this device until you take it back from any of your devices.
}
device-consent-allow = Allow
device-consent-deny = Don’t Allow

# Where an agent of this login works on another of the person's devices: $device as it was
# named, $devices the ones known here.
other-devices-none = None of your other devices is known here.
other-devices-unknown = None of your devices here is called { $device }. Your devices here: { $devices }.
other-devices-name-workspace = { $device } is in more than one of your workspaces here. Name one with --workspace.

# The layout's own actions, as the window manager of a workspace reads them.
so-tilo-layout-read = Read the layout: the workspaces, strips and windows on it, the app and record each window shows, and the other workspaces the caller reaches here.
so-tilo-layout-place = Open a window of an app on the layout, as the caller's: beside the window in front, on the record it names or the app's own. A record a window shows already stays where it is.
so-tilo-layout-close = Close a window on the layout, everywhere it is shown. Its record stays in the workspace.

# The steps of an agent session's tools, as the daemon lists them.
agent-tools-resource = Agent tools
agent-tools-list = List the tools
agent-tools-list-detail = Lists the tools an agent works through in its session.
agent-tools-call = Call a tool
agent-tools-call-detail = Calls one of the agent's tools as a step of its session, and answers what it said.

# What the person's shell tells this device's apps about their windows, as the daemon lists it.
window-follow-resource = Windows
window-follow-follow = Follow someone's view
window-follow-follow-detail = Tells a workspace's apps on this device whose view you follow.
window-follow-handoff = Move a window
window-follow-handoff-detail = Asks an app on this device for the next step of moving its window to another device.

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
# What an agent acting for the person asks before it first acts in an app
# for a task. $action is the label the app declares for the action it starts
# with, $app the app's name: never text a web page wrote.
agent-confirm-question = Let your agent use “{ $action }” in { $app }?
agent-confirm-detail = This allows this one action; your agent asks again next time.
agent-confirm-allow = Allow
agent-confirm-deny = Don’t Allow
# What an app asks before it first uses another app's action. $asker is the
# asking app's name, $action and $app as above.
app-grant-question = Let { $asker } use “{ $action }” in { $app }?
app-grant-detail = It may from now on in this workspace. You can revoke this under Apps in Workspace settings, Privacy & security.

# Where an agent of this login works on another of the person's devices: $device as it was
# named, $devices the ones known here.
other-devices-none = None of your other devices is known here.
other-devices-unknown = None of your devices here is called { $device }. Your devices here: { $devices }.
other-devices-name-workspace = { $device } is in more than one of your workspaces here. Name one with --workspace.

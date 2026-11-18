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

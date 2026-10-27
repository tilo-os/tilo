# The runtime's own words: the camera and microphone consent prompt, and the
# device grants it exports as actions. $device is `camera` or `microphone`.
device-consent-question = { $device ->
    [camera] Allow { $app } to use the camera?
   *[microphone] Allow { $app } to use the microphone?
}
device-consent-detail = { $device ->
    [camera] Tilo keeps this choice for { $app } in this workspace until you revoke it. The app gets nothing from the camera unless you allow it.
   *[microphone] Tilo keeps this choice for { $app } in this workspace until you revoke it. The app gets nothing from the microphone unless you allow it.
}
device-consent-allow = Allow
device-consent-deny = Don’t Allow
device-grants = Camera and microphone access
device-grant-allowed = { $device ->
    [camera] { $app } may use the camera
   *[microphone] { $app } may use the microphone
}
device-grant-denied = { $device ->
    [camera] { $app } may not use the camera
   *[microphone] { $app } may not use the microphone
}
device-grant-room = in workspace { $room }
device-grant-revoke = Revoke
device-grant-revoke-description = Forget this decision: a running capture stops, and the person is asked again next time.
# What an agent acting for the person asks before it first acts in an app
# for a task. $action is the label the app declares for the action it starts
# with, $app the app's name: never text a web page wrote.
agent-confirm-question = Let your agent use “{ $action }” in { $app }?
agent-confirm-detail = This allows this one action; your agent asks again next time.
agent-confirm-allow = Allow
agent-confirm-deny = Don’t Allow

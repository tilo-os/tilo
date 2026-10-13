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
# The confirmation an agent acting for the person asks for, once, before an
# action whose contract asks for it. $action is the label the app declares
# for the action, $app the app's name: never text a web page wrote.
agent-confirm-question = Allow your agent to “{ $action }” in { $app }?
agent-confirm-detail = An agent acting for you asks to do this once. It asks again next time.
agent-confirm-allow = Allow once
agent-confirm-deny = Don’t Allow

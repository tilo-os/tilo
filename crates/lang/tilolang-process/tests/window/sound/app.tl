app so.tilo.lang.sound "Sound"

use so.tilo.device as device

/// What the window shows: one control over a device the app keeps no resource of.
type Card {
    title: Text
}

root Card

default view card for Card

view card(c: Card) {
    std.page(c) {
        ui.Button("Volume up", icon: .plus) {
            on press: device.volume_up(device.sound)
        }
    }
}

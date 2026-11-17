import AssistantCore

/// Where the app's views get everything from. This is the one place that names an
/// implementation of `AssistantSource`: the one that talks to Tilo goes here.
///
/// Until it does, the app is what it is before a device is paired: empty, at the way in, and a
/// link it is given finds no Tilo to pair with.
enum Source {
    static func make() -> any AssistantSource {
        MemorySource()
    }
}

#if os(macOS)
import AssistantCore
import AssistantUI

/// Where the program begins: asked with `--check` it says what it can tell of itself and
/// ends, with nothing on screen; asked with `--drive` it is driven with no window; otherwise
/// it is the app, `MacApp`, over the source `Source` names. The app's window, its menus and
/// its menu-bar item are in `AssistantUI`, where a check can hold them.
@main
enum Entry {
    @MainActor
    static func main() {
        if AssistantCheck.isAsked {
            AssistantCheck.run(source: Source.name)
        }
        if AssistantDrive.isAsked {
            AssistantDrive.run()
        }
        MacApp(source: Source.make(), engine: Source.engine, live: { Source.live($0) }, liveStage: { Source.liveStage($0) })
            .running()
    }
}

extension MacApp {
    /// Be the app, with the live views a stage puts away taken down from Tilo's page with it.
    @MainActor
    func running() -> Never {
        model.stopsLive = { id in await Source.stopLive(id) }
        return run()
    }
}
#else
@main
enum NotThisPlatform {
    static func main() {
        print("AssistantMac is the Mac app. The iPhone app is AssistantPhone.")
    }
}
#endif

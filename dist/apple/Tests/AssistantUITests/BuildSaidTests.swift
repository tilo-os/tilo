import AssistantUI
import XCTest

/// What the app's check says of which build it is. `build.sh` writes the three facts into a
/// bundle's `Info.plist` from the commit it builds; the check's first line says them, so
/// whoever reads a build's words can tell it from every other build.
final class BuildSaidTests: XCTestCase {
    func testTheCheckSaysTheReleaseTheBuildNumberBesideItAndTheCommit() {
        let info: [String: Any] = ["CFBundleShortVersionString": "0.1.13", "CFBundleVersion": "49852", "TiloCommit": "6fa80f0d9f2"]
        XCTAssertEqual(AssistantCheck.build(info: info), "version 0.1.13 (49852), built from 6fa80f0d9f2")
        let changed = info.merging(["TiloCommit": "6fa80f0d9f2+changes"]) { _, new in new }
        XCTAssertEqual(AssistantCheck.build(info: changed), "version 0.1.13 (49852), built from 6fa80f0d9f2+changes")
    }

    /// A bundle no build of `build.sh` wrote names no commit, and the check says that.
    func testABundleThatNamesNoCommitSaysSo() {
        let bare: [String: Any] = ["CFBundleShortVersionString": "0.0.0", "CFBundleVersion": "0.0.0"]
        XCTAssertEqual(AssistantCheck.build(info: bare), "version 0.0.0 (0.0.0), built from a commit it does not name")
        XCTAssertEqual(AssistantCheck.build(info: [:]), "version ? (?), built from a commit it does not name")
    }
}

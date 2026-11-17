import XCTest
@testable import ShellWeb

final class PageCheckTests: XCTestCase {
    func testTheCheckSaysWhetherTheAppMayUseTheSitesPasskeysAndWhatIsMissing() {
        // The system lets the app: nothing is missing.
        XCTAssertEqual(PageCheck.passkeys(host: "tilo.sh", named: true, allowed: true),
                       PageCheck.Line(name: "passkeys of tilo.sh", value: "this app may use them", passed: true))
        // A build whose signature names no site: a fact of that build, not a fault.
        let trial = PageCheck.passkeys(host: "tilo.example", named: false, allowed: false)
        XCTAssertNil(trial.passed)
        XCTAssertEqual(trial.value, "not in this build: its signature does not name the site")
        // A build that names the site and is still refused has something to fix, and hears what.
        let refused = PageCheck.passkeys(host: "tilo.sh", named: true, allowed: false)
        XCTAssertEqual(refused.passed, false)
        XCTAssertTrue(refused.value.contains("the site has to name the app"), refused.value)
        // A web view without passkeys at all.
        XCTAssertNil(PageCheck.passkeys(host: "tilo.sh", named: true, allowed: nil).passed)
    }
}

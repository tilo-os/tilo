import XCTest
@testable import ShellCore

final class PasskeySitesTests: XCTestCase {
    func testABuildMayUseThePasskeysOfTheSitesItsSignatureNamesAndOfNoOther() throws {
        let named = ShellSettings.passkeySites(info: ["TiloPasskeySites": ["tilo.sh", "*.Tilo.Example"]])
        XCTAssertEqual(named, ["tilo.sh", "*.tilo.example"])
        XCTAssertTrue(try ShellSettings(site: "https://tilo.sh").isNamed(among: named))
        // A passkey belongs to a host, whatever the port the site answers on.
        XCTAssertTrue(try ShellSettings(site: "https://tilo.sh:8443").isNamed(among: named))
        XCTAssertTrue(try ShellSettings(site: "https://eu.tilo.example").isNamed(among: named))
        // A name with `*.` is every name under it, not the name itself and not one that ends alike.
        XCTAssertFalse(try ShellSettings(site: "https://tilo.example").isNamed(among: named))
        XCTAssertFalse(try ShellSettings(site: "https://eviltilo.example").isNamed(among: named))
        XCTAssertFalse(try ShellSettings(site: "https://nottilo.sh").isNamed(among: named))
        XCTAssertFalse(try ShellSettings(site: "https://tilo.sh.example").isNamed(among: named))
    }

    func testASiteOnThisComputerIsNeverNamed() throws {
        // No system reads an association from a site that is plain http on this computer.
        XCTAssertFalse(try ShellSettings(site: "http://tilo.localhost:24120").isNamed(among: ["tilo.localhost"]))
    }

    func testABuildThatNamesNoSiteMayUseNone() throws {
        XCTAssertEqual(ShellSettings.passkeySites(info: [:]), [])
        XCTAssertEqual(ShellSettings.passkeySites(info: nil), [])
        // Only a list, as build.sh writes it, names sites.
        XCTAssertEqual(ShellSettings.passkeySites(info: ["TiloPasskeySites": "tilo.sh"]), [])
        XCTAssertFalse(try ShellSettings(site: "https://tilo.sh").isNamed(among: []))
    }
}

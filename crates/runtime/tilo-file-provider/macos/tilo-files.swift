// tilo-files add|remove: registers or removes the Tilo File Provider domain.
// It must run from inside the app bundle that embeds the extension
// (Contents/MacOS), which is how the system finds the extension.

import FileProvider
import Foundation

let domain = NSFileProviderDomain(identifier: NSFileProviderDomainIdentifier("workspace"), displayName: "Tilo")
let done = DispatchSemaphore(value: 0)
var failure: Error?

switch CommandLine.arguments.dropFirst().first {
case "add":
    NSFileProviderManager.add(domain) { failure = $0; done.signal() }
case "remove":
    NSFileProviderManager.remove(domain) { failure = $0; done.signal() }
default:
    FileHandle.standardError.write(Data("usage: tilo-files add|remove\n".utf8))
    exit(2)
}

if done.wait(timeout: .now() + 30) == .timedOut {
    FileHandle.standardError.write(Data("tilo-files: the File Provider service did not answer\n".utf8))
    exit(1)
}
if let failure {
    FileHandle.standardError.write(Data("tilo-files: \(failure.localizedDescription)\n".utf8))
    exit(1)
}
if CommandLine.arguments[1] == "add", let manager = NSFileProviderManager(for: domain) {
    manager.getUserVisibleURL(for: .rootContainer) { url, _ in
        if let url { print(url.path) }
        done.signal()
    }
    _ = done.wait(timeout: .now() + 30)
}

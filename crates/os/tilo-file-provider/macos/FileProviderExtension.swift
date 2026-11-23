// The Tilo workspace as a File Provider domain: a replicated extension that
// enumerates the runtime's workspace files and hydrates a file on first
// read, as the workspace filesystem does. Read only for now: the runtime
// serves its files read only (runtime_fs::workspace_server).
//
// Items are named by workspace-relative path; the root container is "".

import FileProvider
import UniformTypeIdentifiers

extension NSFileProviderItemIdentifier {
    init(workspacePath path: String) {
        self = path.isEmpty ? .rootContainer : NSFileProviderItemIdentifier(path)
    }

    var workspacePath: String {
        self == .rootContainer ? "" : rawValue
    }
}

/// One answer of the bridge: its "ok" value, or the error it names.
private struct Answer<Value: Decodable>: Decodable {
    let ok: Value?
    let error: String?
}

private struct Entry: Decodable {
    let name: String
    let kind: String
    let size: UInt64
    let modified: Int64
}

private struct Fetched: Decodable {
    let size: UInt64
}

/// The runtime's workspace, through the C bridge.
private final class Workspace {
    private let session: OpaquePointer

    init?() {
        guard let session = tilo_fp_open(nil) else { return nil }
        self.session = session
    }

    deinit {
        tilo_fp_close(session)
    }

    private func call<Value: Decodable>(
        _ body: (OpaquePointer) -> UnsafeMutablePointer<CChar>?
    ) throws -> Value {
        guard let raw = body(session) else {
            throw NSFileProviderError(.serverUnreachable)
        }
        defer { tilo_fp_string_free(raw) }
        let answer = try JSONDecoder().decode(Answer<Value>.self, from: Data(String(cString: raw).utf8))
        guard let value = answer.ok else {
            NSLog("Tilo files: %@", answer.error ?? "no answer")
            throw NSFileProviderError(.noSuchItem)
        }
        return value
    }

    func list(_ path: String) throws -> [Entry] {
        try call { tilo_fp_list($0, path) }
    }

    func stat(_ path: String) throws -> Entry {
        try call { tilo_fp_stat($0, path) }
    }

    func fetch(_ path: String, into destination: URL) throws -> Fetched {
        try call { tilo_fp_fetch($0, path, destination.path) }
    }
}

final class WorkspaceItem: NSObject, NSFileProviderItem {
    private let path: String
    private let entry: Entry

    fileprivate init(path: String, entry: Entry) {
        self.path = path
        self.entry = entry
    }

    var itemIdentifier: NSFileProviderItemIdentifier {
        NSFileProviderItemIdentifier(workspacePath: path)
    }

    var parentItemIdentifier: NSFileProviderItemIdentifier {
        let parent = path.split(separator: "/").dropLast().joined(separator: "/")
        return NSFileProviderItemIdentifier(workspacePath: parent)
    }

    var filename: String {
        path.isEmpty ? "Tilo" : entry.name
    }

    private var isDirectory: Bool {
        entry.kind == "directory"
    }

    var contentType: UTType {
        isDirectory ? .folder : UTType(filenameExtension: (entry.name as NSString).pathExtension) ?? .data
    }

    var capabilities: NSFileProviderItemCapabilities {
        isDirectory ? [.allowsReading, .allowsContentEnumerating] : [.allowsReading]
    }

    var documentSize: NSNumber? {
        isDirectory ? nil : NSNumber(value: entry.size)
    }

    var contentModificationDate: Date? {
        entry.modified > 0 ? Date(timeIntervalSince1970: TimeInterval(entry.modified)) : nil
    }

    var itemVersion: NSFileProviderItemVersion {
        let version = Data("\(entry.size):\(entry.modified)".utf8)
        return NSFileProviderItemVersion(contentVersion: version, metadataVersion: version)
    }
}

final class WorkspaceEnumerator: NSObject, NSFileProviderEnumerator {
    private let workspace: Workspace?
    private let path: String

    fileprivate init(workspace: Workspace?, path: String) {
        self.workspace = workspace
        self.path = path
    }

    func invalidate() {}

    func enumerateItems(for observer: NSFileProviderEnumerationObserver, startingAt page: NSFileProviderPage) {
        guard let workspace else {
            observer.finishEnumeratingWithError(NSFileProviderError(.serverUnreachable))
            return
        }
        do {
            let items = try workspace.list(path).map { entry in
                WorkspaceItem(path: path.isEmpty ? entry.name : "\(path)/\(entry.name)", entry: entry)
            }
            observer.didEnumerate(items)
            observer.finishEnumerating(upTo: nil)
        } catch {
            observer.finishEnumeratingWithError(error)
        }
    }

    func enumerateChanges(for observer: NSFileProviderChangeObserver, from anchor: NSFileProviderSyncAnchor) {
        observer.finishEnumeratingChanges(upTo: anchor, moreComing: false)
    }

    func currentSyncAnchor(completionHandler: @escaping (NSFileProviderSyncAnchor?) -> Void) {
        completionHandler(NSFileProviderSyncAnchor(Data("0".utf8)))
    }
}

@objc(TiloFileProviderExtension)
final class TiloFileProviderExtension: NSObject, NSFileProviderReplicatedExtension {
    private let domain: NSFileProviderDomain
    private let workspace = Workspace()

    required init(domain: NSFileProviderDomain) {
        self.domain = domain
        super.init()
    }

    func invalidate() {}

    func item(
        for identifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let path = identifier.workspacePath
        do {
            guard let workspace else { throw NSFileProviderError(.serverUnreachable) }
            completionHandler(WorkspaceItem(path: path, entry: try workspace.stat(path)), nil)
        } catch {
            completionHandler(nil, error)
        }
        return Progress()
    }

    func fetchContents(
        for itemIdentifier: NSFileProviderItemIdentifier,
        version requestedVersion: NSFileProviderItemVersion?,
        request: NSFileProviderRequest,
        completionHandler: @escaping (URL?, NSFileProviderItem?, Error?) -> Void
    ) -> Progress {
        let path = itemIdentifier.workspacePath
        do {
            guard let workspace, let manager = NSFileProviderManager(for: domain) else {
                throw NSFileProviderError(.serverUnreachable)
            }
            let destination = try manager.temporaryDirectoryURL().appendingPathComponent(UUID().uuidString)
            _ = try workspace.fetch(path, into: destination)
            completionHandler(destination, WorkspaceItem(path: path, entry: try workspace.stat(path)), nil)
        } catch {
            completionHandler(nil, nil, error)
        }
        return Progress()
    }

    func createItem(
        basedOn itemTemplate: NSFileProviderItem,
        fields: NSFileProviderItemFields,
        contents url: URL?,
        options: NSFileProviderCreateItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        completionHandler(nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
        return Progress()
    }

    func modifyItem(
        _ item: NSFileProviderItem,
        baseVersion version: NSFileProviderItemVersion,
        changedFields: NSFileProviderItemFields,
        contents newContents: URL?,
        options: NSFileProviderModifyItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (NSFileProviderItem?, NSFileProviderItemFields, Bool, Error?) -> Void
    ) -> Progress {
        completionHandler(nil, [], false, NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
        return Progress()
    }

    func deleteItem(
        identifier: NSFileProviderItemIdentifier,
        baseVersion version: NSFileProviderItemVersion,
        options: NSFileProviderDeleteItemOptions = [],
        request: NSFileProviderRequest,
        completionHandler: @escaping (Error?) -> Void
    ) -> Progress {
        completionHandler(NSError(domain: NSCocoaErrorDomain, code: NSFeatureUnsupportedError))
        return Progress()
    }

    func enumerator(
        for containerItemIdentifier: NSFileProviderItemIdentifier,
        request: NSFileProviderRequest
    ) throws -> NSFileProviderEnumerator {
        WorkspaceEnumerator(workspace: workspace, path: containerItemIdentifier.workspacePath)
    }
}

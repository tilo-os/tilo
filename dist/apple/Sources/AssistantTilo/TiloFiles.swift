import AssistantCore
import Foundation
import TiloLive
import UniformTypeIdentifiers

/// Files in a conversation, as the app shows them. In Tilo a file that a message carries is
/// content its workspace keeps and the message names (Chat's `attachments`). The app shows each
/// with the name and the size Chat gives, and keeps nothing of it.
///
/// A picked file is sent through Tilo's page: its bytes go into the person's workspace a piece
/// at a time, and the message that carries it names it as Chat does (`carried`). A file in a
/// message is opened by a link Tilo's page hands the app, which the system downloads into a
/// file of this device (`fetch`).
enum TiloFiles {
    /// The files a message carries, as the views show them. Each is named by its message's item
    /// and its place among what the message carries: nothing of a file is kept here.
    static func attachments(of message: ChatMessage) -> [Attachment] {
        message.files.map { file in
            Attachment(id: .init("\(message.id)#\(file.index)"), name: file.name, type: type(of: file), bytes: file.bytes)
        }
    }

    /// The system's name for the kind of file its name says it is. Where the name's ending
    /// says nothing the system knows: what Chat keeps it as, a picture, a recording or plain data.
    static func type(of file: ChatFile) -> String {
        let ending = (file.name as NSString).pathExtension
        if !ending.isEmpty, let known = UTType(filenameExtension: ending), !known.isDynamic { return known.identifier }
        switch file.kind {
        case .image: return UTType.image.identifier
        case .video: return UTType.movie.identifier
        case .file: return UTType.data.identifier
        }
    }
}

/// Sending. The bytes of a picked file go into the content store of the person's workspace
/// through Tilo's page, as the person on this device (`TiloEngine.putBegin` and its two
/// neighbours); the message then names the file by what the workspace keeps it under. The
/// file is read a piece at a time and is never whole in this app or in the page. Nothing of
/// it is kept here.
extension TiloFiles {
    /// What a person reads where Tilo's page hands the app no way to put a file in: the page
    /// of a site whose code is older.
    static let cannotSend = "A file cannot be sent from this app yet"

    /// Put the files of a draft into the person's workspace, in their order, and answer how
    /// the message names each (Chat's `attached`). A page that hands the app no way to put a
    /// file in is asked for nothing more, and nothing of any file is touched then.
    @MainActor
    static func carried(_ files: [Draft.File], through engine: TiloEngine) async throws -> [JSON] {
        guard !files.isEmpty else { return [] }
        guard try await engine.putIsOffered() else { throw SourceProblem.refused(cannotSend) }
        var carried: [JSON] = []
        for file in files { carried.append(try await put(file, through: engine)) }
        return carried
    }

    /// One file, a piece at a time. Its size is the file system's word, said to the page
    /// before a byte is read: a file too large for a message is refused there, by the size
    /// alone. A file that cannot be read, is empty or changed on the way is not sent.
    @MainActor
    private static func put(_ file: Draft.File, through engine: TiloEngine) async throws -> JSON {
        let name = file.name
        let said = try? file.url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
        guard said?.isRegularFile == true, let size = said?.fileSize, let handle = try? FileHandle(forReadingFrom: file.url) else {
            throw SourceProblem.refused("\(name) cannot be read on this device")
        }
        defer { try? handle.close() }
        // Chat takes no file with nothing in it.
        guard size > 0 else { throw SourceProblem.refused("\(name) is empty, and an empty file cannot be sent") }
        let upload: Int
        do {
            upload = try await engine.putBegin(size: size)
        } catch TiloEngine.Problem.refused(code: "invalid_request", reason: let reason) {
            throw SourceProblem.refused(tooLarge(name, size, said: reason))
        }
        var offset = 0
        while offset < size {
            guard let piece = try? handle.read(upToCount: min(TiloEngine.mostPieceBytes, size - offset)), !piece.isEmpty else {
                throw SourceProblem.refused("\(name) changed while it was being sent, so it was not sent")
            }
            try await engine.putAppend(piece, to: upload, at: offset)
            offset += piece.count
        }
        let kept = try await engine.putFinish(upload)
        return ["kind": "file", "name": .string(name), "blob": .string(kept), "size": .number(Double(size))]
    }

    /// What a person reads where Tilo's page refuses a file by its size, before any of it
    /// crossed: how large the file is and how large one may be, each as the system writes a
    /// file's size. The most is the page's own number, the first its sentence says. Where it
    /// says none, its sentence stands as it came.
    static func tooLarge(_ name: String, _ size: Int, said reason: String) -> String {
        let written = { (bytes: Int) in Int64(bytes).formatted(.byteCount(style: .file)) }
        let numbers = reason.split(whereSeparator: { !($0.isASCII && $0.isNumber) }).compactMap { Int($0) }
        guard let most = numbers.first(where: { $0 > 0 }) else { return "\(name) is too large to send: \(reason)" }
        return "\(name) is \(written(size)), and a file sent from here can be \(written(most)) at most"
    }
}

/// Opening. A file a message carries is fetched by a link: an address on the app's own Tilo
/// site that serves the content for a short time, asked of the workspace's server through
/// Tilo's page as the person on this device. The system downloads it straight into a file,
/// so nothing of it crosses the page or this app's memory. The link is in no log and in
/// nothing the app keeps: every opening asks for a new one.
extension TiloFiles {
    /// What a person reads where Tilo's page hands the app no link to a file: the page of a
    /// site whose code is older.
    static let cannotOpen = "A file cannot be fetched into this app yet"

    /// How a link becomes a file on this device: it answers where the fetched bytes lie, a
    /// file whoever asked then owns.
    typealias Download = @Sendable (URL) async throws -> URL

    /// The system's own download. Its session keeps nothing: no cache, no cookie, nothing to
    /// sign in with.
    static let download: Download = { link in
        let session = URLSession(configuration: .ephemeral)
        defer { session.finishTasksAndInvalidate() }
        let (file, response) = try await session.download(from: link)
        guard (response as? HTTPURLResponse)?.statusCode == 200 else {
            try? FileManager.default.removeItem(at: file)
            throw URLError(.badServerResponse)
        }
        return file
    }

    /// Where opened files lie: a folder of the app's own under its temporary folder.
    static var opened: URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("so.tilo.assistant.opened", isDirectory: true)
    }

    /// The file `attachment` names, on this device. A file is named by its message's item and
    /// its place among what the message carries; the message is read again, so what it
    /// carries now is what is opened. A page that hands the app no link is asked for nothing.
    @MainActor
    static func fetch(_ attachment: Attachment.ID, through engine: TiloEngine, by download: Download) async throws -> URL {
        guard try await engine.linkIsOffered() else { throw SourceProblem.refused(cannotOpen) }
        let gone = SourceProblem.refused("This file is no longer in the conversation")
        let named = attachment.value
        guard let cut = named.lastIndex(of: "#"), let index = Int(named[named.index(after: cut)...]) else { throw gone }
        let message: ChatMessage?
        do {
            message = try await engine.chatMessage(String(named[..<cut]))
        } catch TiloEngine.Problem.refused {
            message = nil
        }
        guard let file = message?.files.first(where: { $0.index == index }) else { throw gone }
        let link = try await engine.contentLink(to: file.content, named: file.name)
        let fetched: URL
        do {
            fetched = try await download(link)
        } catch {
            // Why it did not come may name the link: a person reads the file's name alone.
            throw SourceProblem.refused("\(file.name) could not be fetched")
        }
        do {
            return try keep(fetched, as: file.name)
        } catch {
            try? FileManager.default.removeItem(at: fetched)
            throw SourceProblem.refused("\(file.name) could not be kept on this device")
        }
    }

    /// Move what was fetched to where opened files lie, in a folder of its own, under the
    /// last part of the file's name alone: a name never says where on this device a file
    /// goes. What was opened more than an hour ago is removed first.
    static func keep(_ fetched: URL, as name: String) throws -> URL {
        let files = FileManager.default
        let old = (try? files.contentsOfDirectory(at: opened, includingPropertiesForKeys: [.creationDateKey])) ?? []
        for folder in old {
            let made = (try? folder.resourceValues(forKeys: [.creationDateKey]))?.creationDate ?? .distantPast
            if made < Date().addingTimeInterval(-3600) { try? files.removeItem(at: folder) }
        }
        let last = (name as NSString).lastPathComponent
        let folder = opened.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try files.createDirectory(at: folder, withIntermediateDirectories: true)
        let place = folder.appendingPathComponent(["", ".", "..", "/"].contains(last) ? "file" : last, isDirectory: false)
        try files.moveItem(at: fetched, to: place)
        return place
    }
}

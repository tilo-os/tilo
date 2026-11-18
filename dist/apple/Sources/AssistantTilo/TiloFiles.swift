import AssistantCore
import Foundation
import TiloLive
import UniformTypeIdentifiers

/// Files in a conversation, as the app shows them. In Tilo a file that a message carries is
/// content its workspace keeps and the message names (Chat's `attachments`). The app shows each
/// with the name and the size Chat gives, and keeps nothing of it.
///
/// Tilo's page hands a native app no way to a file's content yet, neither to put one in nor to
/// be handed one. So a file can be neither sent nor opened from here, and each of the two says
/// so in plain words instead of doing something of the app's own.
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

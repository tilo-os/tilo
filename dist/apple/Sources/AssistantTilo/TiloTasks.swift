import AssistantCore
import Foundation
import TiloLive

/// How the Tasks panel reads the tasks of the person's assistant from the rows the tracker's
/// read answered. A task is one issue. Where it is listed is decided by the first of Tasks'
/// own rules that fits, and the line under its name is the issue's own: nothing here adds to
/// what the issue keeps.
enum TiloTasks {
    /// The tasks, in the order the tracker answered them.
    static func listed(_ rows: [JSON]) -> [AssistantTask] {
        let named = Set(rows.compactMap(id))
        return rows.compactMap { row in
            guard let id = id(row) else { return nil }
            // A piece of a task that is listed stands under it in Tasks: the panel lists the task.
            if let parent = row["parent"]?.string?.split(separator: "/").last, named.contains(String(parent)) { return nil }
            let (state, note) = place(row)
            return AssistantTask(id: .init(id), title: row["name"]?.string ?? "", state: state, note: note)
        }
    }

    private static func id(_ row: JSON) -> String? {
        row["item"]?.string?.split(separator: "/").last.map(String.init)
    }

    /// Where the panel lists an issue, and the line under its name.
    private static func place(_ row: JSON) -> (AssistantTask.State, String?) {
        let status = word(row["status"])
        if status == "cancelled" { return (.resolved, nil) }
        let work = row["work"]
        // Work that stopped before it was finished needs its person, whatever its status says.
        if let stopped = work?["stopped"], !stopped.isNull { return (.needsInput, line(stopped["words"]?.string)) }
        if status == "done" { return (.resolved, nil) }
        if status == "in-review" { return (.readyForReview, nil) }
        if let work, work["until_ms"]?.number != nil {
            let waits = work["waits"]?.array ?? []
            // It waits for an answer of its person: the question, in words.
            if let asked = waits.first(where: { word($0["kind"]) == "answer" }) { return (.needsInput, line(asked["what"]?.string)) }
            // It waits for something else: each thing in words.
            return (.inProgress, line(waits.compactMap { $0["what"]?.string }.joined(separator: ", ")))
        }
        return (status == "in-progress" ? .inProgress : .upNext, nil)
    }

    /// One of a small set of words, as the server says it: bare, or as a named case.
    private static func word(_ value: JSON?) -> String {
        (value?.string ?? value?["$variant"]?.string ?? "").lowercased().replacingOccurrences(of: "_", with: "-")
    }

    private static func line(_ words: String?) -> String? {
        let said = words?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return said.isEmpty ? nil : said
    }
}

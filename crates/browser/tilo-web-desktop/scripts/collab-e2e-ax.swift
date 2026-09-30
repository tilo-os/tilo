// Accessibility client for one process: JSON commands on stdin, one JSON
// answer per line on stdout.
//   {"cmd":"dump"}                      nodes of the process's windows
//   {"cmd":"set","i":N,"value":"..."}   AXValue of node N of the last dump
//   {"cmd":"select","i":N,"loc":L,"len":0}  AXSelectedTextRange
//   {"cmd":"press","i":N}               AXPress
//   {"cmd":"focus","i":N}               AXFocused = true
// usage: collab-e2e-ax <pid>  (built and driven by collab-e2e.mjs)
import ApplicationServices
import Foundation

let pid = pid_t(CommandLine.arguments[1])!
let app = AXUIElementCreateApplication(pid)
var nodes: [AXUIElement] = []

func attr(_ el: AXUIElement, _ name: String) -> AnyObject? {
    var value: AnyObject?
    return AXUIElementCopyAttributeValue(el, name as CFString, &value) == .success ? value : nil
}

func frame(_ el: AXUIElement) -> CGRect? {
    guard let p = attr(el, kAXPositionAttribute), let s = attr(el, kAXSizeAttribute) else { return nil }
    var point = CGPoint.zero, size = CGSize.zero
    AXValueGetValue(p as! AXValue, .cgPoint, &point)
    AXValueGetValue(s as! AXValue, .cgSize, &size)
    return CGRect(origin: point, size: size)
}

func text(_ value: AnyObject?) -> String? {
    if let s = value as? String, !s.isEmpty { return s }
    if let n = value as? NSNumber { return n.stringValue }
    return nil
}

func dump() -> [String: Any] {
    nodes = []
    var out: [[String: Any]] = []
    var origin = CGPoint.zero
    var windowFrame: [Double] = []
    func walk(_ el: AXUIElement, _ depth: Int) {
        if depth > 60 { return }
        let index = nodes.count
        nodes.append(el)
        var row: [String: Any] = ["i": index, "role": (attr(el, kAXRoleAttribute) as? String) ?? ""]
        for (key, name) in [("title", kAXTitleAttribute), ("value", kAXValueAttribute), ("desc", kAXDescriptionAttribute)] {
            if let t = text(attr(el, name)) { row[key] = t }
        }
        var settable: DarwinBoolean = false
        if AXUIElementIsAttributeSettable(el, kAXValueAttribute as CFString, &settable) == .success, settable.boolValue {
            row["settable"] = true
        }
        if let f = frame(el) {
            row["frame"] = [Double(f.minX - origin.x), Double(f.minY - origin.y), Double(f.width), Double(f.height)]
        }
        if let range = attr(el, kAXSelectedTextRangeAttribute) {
            var r = CFRange()
            AXValueGetValue(range as! AXValue, .cfRange, &r)
            row["sel"] = [r.location, r.length]
        }
        out.append(row)
        for child in (attr(el, kAXChildrenAttribute) as? [AXUIElement]) ?? [] { walk(child, depth + 1) }
    }
    for window in (attr(app, kAXWindowsAttribute) as? [AXUIElement]) ?? [] {
        if let f = frame(window) {
            origin = f.origin
            windowFrame = [Double(f.minX), Double(f.minY), Double(f.width), Double(f.height)]
        }
        walk(window, 0)
    }
    return ["ok": true, "window": windowFrame, "nodes": out]
}

func node(_ command: [String: Any]) -> AXUIElement? {
    guard let i = command["i"] as? Int, i >= 0, i < nodes.count else { return nil }
    return nodes[i]
}

func answer(_ value: [String: Any]) {
    let data = try! JSONSerialization.data(withJSONObject: value)
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write("\n".data(using: .utf8)!)
}

AXUIElementSetMessagingTimeout(app, 2.0)
while let line = readLine() {
    guard let data = line.data(using: .utf8),
          let command = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
          let cmd = command["cmd"] as? String else { answer(["ok": false, "error": "bad command"]); continue }
    if cmd == "dump" { answer(dump()); continue }
    guard let el = node(command) else { answer(["ok": false, "error": "no node"]); continue }
    var status: AXError
    switch cmd {
    case "set": status = AXUIElementSetAttributeValue(el, kAXValueAttribute as CFString, (command["value"] as? String ?? "") as CFString)
    case "press": status = AXUIElementPerformAction(el, kAXPressAction as CFString)
    case "focus": status = AXUIElementSetAttributeValue(el, kAXFocusedAttribute as CFString, kCFBooleanTrue)
    case "select":
        var r = CFRange(location: command["loc"] as? Int ?? 0, length: command["len"] as? Int ?? 0)
        status = AXUIElementSetAttributeValue(el, kAXSelectedTextRangeAttribute as CFString, AXValueCreate(.cfRange, &r)!)
    default: answer(["ok": false, "error": "unknown command"]); continue
    }
    answer(["ok": status == .success, "status": status.rawValue])
}

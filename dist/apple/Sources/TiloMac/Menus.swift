#if os(macOS)
import AppKit
import ShellCore

/// The menu bar's menus. The page has its own commands; these are the ones a Mac app owes:
/// the app's own, the pasteboard's, the window's.
@MainActor
enum Menus {
    @discardableResult
    static func add(_ words: Words, _ action: Selector?, key: String = "", with modifiers: NSEvent.ModifierFlags = .command,
                    to menu: NSMenu, target: AnyObject? = nil) -> NSMenuItem {
        let item = NSMenuItem(title: words.said, action: action, keyEquivalent: key)
        item.keyEquivalentModifierMask = modifiers
        item.target = target
        menu.addItem(item)
        return item
    }

    static func main(for app: AppDelegate, name: String) -> NSMenu {
        let main = NSMenu()
        let submenu = { (title: String) -> NSMenu in
            let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
            let menu = NSMenu(title: title)
            item.submenu = menu
            main.addItem(item)
            return menu
        }

        let own = submenu(name)
        add(.about, #selector(NSApplication.orderFrontStandardAboutPanel(_:)), to: own, target: NSApp)
        own.addItem(.separator())
        add(.settings, #selector(AppDelegate.showSettings(_:)), key: ",", to: own, target: app)
        add(.openAtLogin, #selector(AppDelegate.toggleOpenAtLogin(_:)), to: own, target: app)
        own.addItem(.separator())
        add(.hide, #selector(NSApplication.hide(_:)), key: "h", to: own, target: NSApp)
        add(.hideOthers, #selector(NSApplication.hideOtherApplications(_:)), key: "h", with: [.command, .option],
            to: own, target: NSApp)
        add(.showAll, #selector(NSApplication.unhideAllApplications(_:)), to: own, target: NSApp)
        own.addItem(.separator())
        add(.quit, #selector(NSApplication.terminate(_:)), key: "q", to: own, target: NSApp)

        let file = submenu(Words.fileMenu.said)
        add(.openLink, #selector(AppDelegate.openLinkFromClipboard(_:)), key: "o", with: [.command, .shift],
            to: file, target: app)
        add(.dictate, #selector(AppDelegate.dictate(_:)), key: "d", with: [.command, .shift], to: file, target: app)
        file.addItem(.separator())
        add(.closeWindow, #selector(NSWindow.performClose(_:)), key: "w", to: file)

        // To whatever has the keyboard, which in the window is the page.
        let edit = submenu(Words.editMenu.said)
        add(.undo, Selector(("undo:")), key: "z", to: edit)
        add(.redo, Selector(("redo:")), key: "z", with: [.command, .shift], to: edit)
        edit.addItem(.separator())
        add(.cut, #selector(NSText.cut(_:)), key: "x", to: edit)
        add(.copy, #selector(NSText.copy(_:)), key: "c", to: edit)
        add(.paste, #selector(NSText.paste(_:)), key: "v", to: edit)
        add(.selectAll, #selector(NSText.selectAll(_:)), key: "a", to: edit)

        let view = submenu(Words.viewMenu.said)
        add(.reload, #selector(AppDelegate.reload(_:)), key: "r", to: view, target: app)

        let window = submenu(Words.windowMenu.said)
        add(.minimize, #selector(NSWindow.performMiniaturize(_:)), key: "m", to: window)
        add(.openApp, #selector(AppDelegate.showWindow(_:)), key: "0", to: window, target: app)
        NSApp.windowsMenu = window
        return main
    }
}
#endif

import AppKit
import WorkspaceCore

// xctx                 the window
// xctx ctl CMD k=v...  talk to the running window (what agents in a space use: "$XCTX_CTL" ctl ...)
//
// XCTX_DATA_DIR      data folder (default ~/Library/Application Support/xctx): spaces.json, control.sock
// XCTX_INDEX_ROOTS   colon-separated folders to index instead of the saved ones
// XCTX_AGENT_CMD  replaces the agent command (default: pi, told about its space)
// XCTX_SELFTEST=<file>  drive the real window through the main flows, write PASS/FAIL lines to <file>, quit

func dataDir() -> URL {
    if let d = ProcessInfo.processInfo.environment["XCTX_DATA_DIR"], !d.isEmpty { return URL(fileURLWithPath: d) }
    return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("xctx")
}

if CommandLine.arguments.dropFirst().first == "ctl" {
    let env = ProcessInfo.processInfo.environment
    let args = Array(CommandLine.arguments.dropFirst(2))
    guard !args.isEmpty else {
        print("usage: xctx ctl CMD [key=value ...]\ncommands: " + SpacesController.commands.joined(separator: " "))
        exit(2)
    }
    let sock = env["XCTX_CTL_SOCK"] ?? Control.socketPath(dataDir: dataDir())
    do {
        let reply = try Control.send(Control.request(args, defaultSpace: env["XCTX_SPACE"]), socket: sock)
        let out = try JSONSerialization.data(withJSONObject: reply, options: [.prettyPrinted, .sortedKeys])
        print(String(decoding: out, as: UTF8.self))
        exit(reply["ok"] as? Bool == true ? 0 : 1)
    } catch {
        FileHandle.standardError.write(Data("\(error)\n".utf8))
        exit(1)
    }
}

@MainActor final class AppDelegate: NSObject, NSApplicationDelegate {
    var controller: SpacesController?
    var server: ControlServer?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.mainMenu = buildMenu()
        let dir = dataDir()
        let store = SpaceStore(dir: dir)
        var roots = store.data.roots
        if let r = ProcessInfo.processInfo.environment["XCTX_INDEX_ROOTS"] { roots = r.split(separator: ":").map(String.init) }
        let c = SpacesController(store: store, index: RepoIndex(roots: roots), dataDir: dir)
        controller = c
        let s = ControlServer(path: Control.socketPath(dataDir: dir)) { [weak c] req in
            c?.handle(req) ?? ["ok": false, "error": "no window"]
        }
        do { try s.start() } catch { c.alert("The harness socket did not start", "\(error)") }
        server = s
        c.window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        if let out = ProcessInfo.processInfo.environment["XCTX_SELFTEST"] { SelfTest.run(c, out: out) }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func applicationWillTerminate(_ notification: Notification) {
        if let s = server { unlink(s.path) }
    }

    // MARK: menu actions

    @objc func newTask() { controller?.focusPrompt() }
    @objc func launch() { controller?.launchCurrent() }
    @objc func ship() { controller?.shipCurrent() }
    @objc func discard() { if let c = controller, let s = c.space { c.discard(s.id) } }
    @objc func addFolder() { controller?.open(""); controller?.addFolder() }
    @objc func home() { controller?.open("") }
    @objc func nextSpace() { controller?.cycleSpace(1) }
    @objc func prevSpace() { controller?.cycleSpace(-1) }
    @objc func mount(_ item: NSMenuItem) { if let k = PaneKind(rawValue: item.tag) { controller?.mountFocused(k) } }
    @objc func splitRight() { controller?.splitFocused(sideBySide: true) }
    @objc func splitDown() { controller?.splitFocused(sideBySide: false) }
    @objc func closePane() { controller?.closeFocused() }
    @objc func nextPane() { controller?.cycleFocus(1) }
    @objc func prevPane() { controller?.cycleFocus(-1) }
    @objc func focusAddress() { controller?.focusedBrowser?.focusAddress() }
    @objc func browserBack() { controller?.focusedBrowser?.goBack() }
    @objc func browserForward() { controller?.focusedBrowser?.goForward() }
    @objc func browserReload() { controller?.focusedBrowser?.reload() }
    @objc func shape(_ item: NSMenuItem) { controller?.reshapeMenu(item) }

    private func buildMenu() -> NSMenu {
        let bar = NSMenu()
        func menu(_ title: String, _ items: [NSMenuItem]) -> NSMenu {
            let top = NSMenuItem()
            let m = NSMenu(title: title)
            items.forEach { m.addItem($0) }
            top.submenu = m
            bar.addItem(top)
            return m
        }
        func item(_ title: String, _ action: Selector, _ key: String, _ mods: NSEvent.ModifierFlags = .command,
                  tag: Int = 0, object: Any? = nil) -> NSMenuItem {
            let i = NSMenuItem(title: title, action: action, keyEquivalent: key)
            i.keyEquivalentModifierMask = mods
            i.tag = tag
            i.representedObject = object
            return i
        }
        let arrow = { (k: Int) in String(UnicodeScalar(UInt16(k))!) }
        _ = menu("xctx", [
            item("Hide xctx", #selector(NSApplication.hide(_:)), "h"),
            .separator(),
            item("Quit xctx", #selector(NSApplication.terminate(_:)), "q"),
        ])
        _ = menu("Space", [
            item("New Task…", #selector(newTask), "n"),
            item("Launch", #selector(launch), "\r"),
            item("Ship…", #selector(ship), "s", [.command, .shift]),
            item("Discard…", #selector(discard), "\u{8}", [.command, .shift]),
            .separator(),
            item("Shape: Dev", #selector(shape(_:)), "1", [.command, .option], object: "dev"),
            item("Shape: Debug", #selector(shape(_:)), "2", [.command, .option], object: "debug"),
            item("Shape: Review", #selector(shape(_:)), "3", [.command, .option], object: "review"),
            .separator(),
            item("Home", #selector(home), "0"),
            item("Next Space", #selector(nextSpace), "]", [.command, .shift]),
            item("Previous Space", #selector(prevSpace), "[", [.command, .shift]),
            .separator(),
            item("Index a Folder…", #selector(addFolder), "i", [.command, .shift]),
        ])
        _ = menu("Edit", [
            item("Undo", Selector(("undo:")), "z"),
            item("Redo", Selector(("redo:")), "z", [.command, .shift]),
            .separator(),
            item("Cut", #selector(NSText.cut(_:)), "x"),
            item("Copy", #selector(NSText.copy(_:)), "c"),
            item("Paste", #selector(NSText.paste(_:)), "v"),
            item("Select All", #selector(NSText.selectAll(_:)), "a"),
        ])
        let keys: [PaneKind: (String, NSEvent.ModifierFlags)] = [
            .shell: ("t", .command), .agent: ("a", [.command, .shift]), .git: ("g", [.command, .shift]),
            .browser: ("b", [.command, .shift]), .editor: ("e", [.command, .shift]),
        ]
        _ = menu("Pane", PaneKind.allCases.map { k in
            item("Mount \(k.label)", #selector(mount(_:)), keys[k]!.0, keys[k]!.1, tag: k.rawValue)
        } + [
            .separator(),
            item("Split Right", #selector(splitRight), "d"),
            item("Split Down", #selector(splitDown), "d", [.command, .shift]),
            item("Next Pane", #selector(nextPane), arrow(NSRightArrowFunctionKey), [.command, .option]),
            item("Previous Pane", #selector(prevPane), arrow(NSLeftArrowFunctionKey), [.command, .option]),
            item("Close Pane", #selector(closePane), "w"),
        ])
        _ = menu("Browser", [
            item("Open Location", #selector(focusAddress), "l"),
            item("Back", #selector(browserBack), "["),
            item("Forward", #selector(browserForward), "]"),
            item("Reload Page", #selector(browserReload), "r"),
        ])
        let window = menu("Window", [
            item("Minimize", #selector(NSWindow.performMiniaturize(_:)), "m"),
            item("Enter Full Screen", #selector(NSWindow.toggleFullScreen(_:)), "f", [.command, .control]),
        ])
        NSApp.windowsMenu = window
        return bar
    }
}

MainActor.assumeIsolated {
    let app = NSApplication.shared
    let delegate = AppDelegate()
    app.delegate = delegate
    app.setActivationPolicy(.regular)
    app.run()
}

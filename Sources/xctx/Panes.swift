import AppKit
import SwiftTerm
import WebKit
import WorkspaceCore

enum Theme {
    static let bg = NSColor(srgbRed: 0.075, green: 0.08, blue: 0.095, alpha: 1)
    static let panel = NSColor(srgbRed: 0.105, green: 0.11, blue: 0.13, alpha: 1)
    static let ink = NSColor(white: 0.92, alpha: 1)
    static let dim = NSColor(white: 0.55, alpha: 1)
    static let accent = NSColor(srgbRed: 0.11, green: 0.65, blue: 0.88, alpha: 1)
    static let ring = NSColor(white: 1, alpha: 0.09)
    static let mono = NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
}

enum PaneKind: Int, CaseIterable {
    case agent, shell, git, browser, editor

    var label: String { ["Agent", "Shell", "Git", "Browser", "Editor"][rawValue] }
    var symbol: String { ["sparkles", "terminal", "arrow.triangle.branch", "globe", "pencil.line"][rawValue] }

    /// The program a terminal pane runs (nil: the login shell itself).
    var command: String? {
        let env = ProcessInfo.processInfo.environment
        switch self {
        case .agent: return env["XCTX_AGENT_CMD"] ?? UserDefaults.standard.string(forKey: "agentCommand") ?? "pi"
        case .shell, .browser: return nil
        case .git: return "lazygit"
        case .editor: return (env["EDITOR"].flatMap { $0.isEmpty ? nil : $0 } ?? "nvim") + " ."
        }
    }
}

/// What a pane shows: a terminal program, a web page, or the launcher of an empty pane.
@MainActor protocol PaneContent: AnyObject {
    var kind: PaneKind? { get } // nil for the launcher
    var view: NSView { get }
    var focusView: NSView { get }
    var title: String { get }
    var onChange: (() -> Void)? { get set }
    func close()
}

func symbolButton(_ name: String, _ tip: String, _ target: AnyObject, _ action: Selector) -> NSButton {
    let b = NSButton(image: NSImage(systemSymbolName: name, accessibilityDescription: tip) ?? NSImage(), target: target, action: action)
    b.isBordered = false
    b.bezelStyle = .regularSquare
    b.contentTintColor = Theme.dim
    b.toolTip = tip
    b.setAccessibilityLabel(tip)
    b.translatesAutoresizingMaskIntoConstraints = false
    b.widthAnchor.constraint(equalToConstant: 22).isActive = true
    return b
}

// MARK: terminal

@MainActor final class TerminalPane: NSObject, PaneContent, LocalProcessTerminalViewDelegate {
    let kind: PaneKind?
    let term: LocalProcessTerminalView
    private(set) var title: String
    var onChange: (() -> Void)?
    var onExit: ((Int32?) -> Void)?
    private(set) var exited = false
    var view: NSView { term }
    var focusView: NSView { term }

    init(kind: PaneKind, cwd: String, env extra: [String: String] = [:], command: String? = nil, title: String? = nil) {
        self.kind = kind
        self.title = title ?? kind.label
        term = LocalProcessTerminalView(frame: NSRect(x: 0, y: 0, width: 640, height: 400))
        super.init()
        term.font = Theme.mono
        term.nativeBackgroundColor = Theme.bg
        term.nativeForegroundColor = Theme.ink
        term.processDelegate = self
        let shell = ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh"
        var env = ProcessInfo.processInfo.environment
        env["TERM"] = "xterm-256color"
        env["COLORTERM"] = "truecolor"
        env["TERM_PROGRAM"] = "xctx"
        env.merge(extra) { _, new in new }
        // a login + interactive shell, so PATH matches what the user's own terminal has (brew, nvm, ...)
        let args = (command ?? kind.command).map { ["-l", "-i", "-c", "exec \($0)"] } ?? ["-l"]
        term.startProcess(executable: shell, args: args, environment: env.map { "\($0.key)=\($0.value)" }, currentDirectory: cwd)
    }

    func sizeChanged(source: LocalProcessTerminalView, newCols: Int, newRows: Int) {}
    func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {}

    func setTerminalTitle(source: LocalProcessTerminalView, title: String) {
        guard !title.isEmpty else { return }
        self.title = title
        onChange?()
    }

    func processTerminated(source: TerminalView, exitCode: Int32?) {
        exited = true
        term.feed(text: "\r\n\u{1b}[2m[process exited\(exitCode.map { " \($0)" } ?? "") · ⌘W closes the pane]\u{1b}[0m\r\n")
        onExit?(exitCode)
    }

    func send(_ text: String) {
        term.send(data: ArraySlice(Array(text.utf8)))
    }

    /// The visible text, for checks (selects all, reads, unselects).
    func screenText() -> String {
        term.selectAll()
        defer { term.selectNone() }
        return term.getSelection() ?? ""
    }

    func close() {
        if let p = term.process, p.running { p.terminate() }
    }
}

// MARK: browser

@MainActor final class BrowserPane: NSObject, PaneContent, WKNavigationDelegate, WKUIDelegate {
    let kind: PaneKind? = .browser
    let root = NSView()
    let web: WKWebView
    let address = NSTextField()
    var onChange: (() -> Void)?
    private var observers: [NSKeyValueObservation] = []
    var view: NSView { root }
    var focusView: NSView { web }
    var title: String { (web.title?.isEmpty == false ? web.title : nil) ?? "Browser" }

    init(url: String) {
        let cfg = WKWebViewConfiguration()
        cfg.preferences.isElementFullscreenEnabled = true
        web = WKWebView(frame: NSRect(x: 0, y: 0, width: 640, height: 400), configuration: cfg)
        super.init()
        web.navigationDelegate = self
        web.uiDelegate = self
        web.allowsBackForwardNavigationGestures = true
        web.allowsMagnification = true

        address.placeholderString = "Search or enter address"
        address.bezelStyle = .roundedBezel
        address.font = .systemFont(ofSize: 12)
        address.lineBreakMode = .byTruncatingTail
        address.cell?.isScrollable = true
        address.target = self
        address.action = #selector(go)
        address.setAccessibilityLabel("Address")
        let bar = NSStackView(views: [
            symbolButton("chevron.left", "Back", self, #selector(goBack)),
            symbolButton("chevron.right", "Forward", self, #selector(goForward)),
            symbolButton("arrow.clockwise", "Reload", self, #selector(reload)),
            address,
        ])
        bar.orientation = .horizontal
        bar.spacing = 4
        bar.edgeInsets = NSEdgeInsets(top: 0, left: 6, bottom: 0, right: 6)
        for v in [bar, web] as [NSView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            root.addSubview(v)
        }
        NSLayoutConstraint.activate([
            bar.topAnchor.constraint(equalTo: root.topAnchor, constant: 4),
            bar.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            bar.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            bar.heightAnchor.constraint(equalToConstant: 26),
            web.topAnchor.constraint(equalTo: bar.bottomAnchor, constant: 4),
            web.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            web.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            web.bottomAnchor.constraint(equalTo: root.bottomAnchor),
        ])
        observers = [
            web.observe(\.title) { [weak self] _, _ in MainActor.assumeIsolated { self?.onChange?() } },
            web.observe(\.url) { [weak self] w, _ in
                MainActor.assumeIsolated {
                    guard let self, self.address.currentEditor() == nil else { return }
                    self.address.stringValue = w.url?.absoluteString ?? ""
                }
            },
        ]
        load(url)
    }

    func load(_ text: String) {
        web.load(URLRequest(url: Address.url(for: text)))
    }

    @objc func go() {
        load(address.stringValue)
        root.window?.makeFirstResponder(web)
    }
    @objc func goBack() { web.goBack() }
    @objc func goForward() { web.goForward() }
    @objc func reload() { web.reload() }
    func focusAddress() {
        root.window?.makeFirstResponder(address)
        address.currentEditor()?.selectAll(nil)
    }

    /// Links that want a new window open in this pane.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if let url = navigationAction.request.url { webView.load(URLRequest(url: url)) }
        return nil
    }

    func close() {
        web.stopLoading()
        observers = []
    }
}

// MARK: launcher (an empty pane)

@MainActor final class LauncherPane: NSObject, PaneContent {
    let kind: PaneKind? = nil
    let root = NSView()
    var onChoose: ((PaneKind) -> Void)?
    var onChange: (() -> Void)?
    private var first: NSButton!
    var view: NSView { root }
    var focusView: NSView { first }
    var title: String { "New pane" }

    init(message: String = "Open something here") {
        super.init()
        let label = NSTextField(labelWithString: message)
        label.textColor = Theme.dim
        let buttons = PaneKind.allCases.map { k -> NSButton in
            let b = NSButton(title: k.label, image: NSImage(systemSymbolName: k.symbol, accessibilityDescription: nil) ?? NSImage(),
                             target: self, action: #selector(pick(_:)))
            b.tag = k.rawValue
            b.bezelStyle = .rounded
            b.controlSize = .large
            b.imagePosition = .imageLeading
            return b
        }
        first = buttons[0]
        let row = NSStackView(views: buttons)
        row.spacing = 8
        let col = NSStackView(views: [label, row])
        col.orientation = .vertical
        col.spacing = 12
        col.translatesAutoresizingMaskIntoConstraints = false
        root.addSubview(col)
        NSLayoutConstraint.activate([
            col.centerXAnchor.constraint(equalTo: root.centerXAnchor),
            col.centerYAnchor.constraint(equalTo: root.centerYAnchor),
            col.leadingAnchor.constraint(greaterThanOrEqualTo: root.leadingAnchor, constant: 8),
        ])
    }

    @objc func pick(_ b: NSButton) {
        if let k = PaneKind(rawValue: b.tag) { onChoose?(k) }
    }

    func close() {}
}

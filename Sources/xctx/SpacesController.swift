import AppKit
import WorkspaceCore

/// A launched space's panes. `roles` names what each pane is for (`agent:payments`, `shell:payments`, `git:...`,
/// `extra:3`), so a reshape can keep the same agents and swap the rest; agents not in the new shape are parked.
@MainActor final class SpaceView {
    let id: String
    var tree = PaneTree()
    var panes: [Int: PaneContent] = [:]
    var containers: [Int: PaneContainer] = [:]
    var roles: [Int: String] = [:]
    var parked: [String: PaneContent] = [:]
    var extras = 0
    var focused: PaneContent? { panes[tree.focus] }

    init(id: String) { self.id = id }

    func content(role: String) -> PaneContent? {
        roles.first { $0.value == role }.flatMap { panes[$0.key] } ?? parked[role]
    }
}

func shellQuote(_ s: String) -> String { "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'" }

@MainActor final class SpacesController: NSObject, NSWindowDelegate, NSTextFieldDelegate {
    let store: SpaceStore
    let index: RepoIndex
    let dataDir: URL
    let window: FocusWindow
    /// "" is Home; otherwise a space id.
    var current = ""
    private(set) var views: [String: SpaceView] = [:]
    private let tabBar = NSSegmentedControl()
    private let body = NSView()
    let prompt = NSTextField()
    private let status = NSTextField(labelWithString: "")
    private var homeLauncher: LauncherPane?
    private var extraFound: [String: [RepoMatch]] = [:] // draft id → repos found with "find another repo"

    init(store: SpaceStore, index: RepoIndex, dataDir: URL) {
        self.store = store
        self.index = index
        self.dataDir = dataDir
        window = FocusWindow(contentRect: NSRect(x: 0, y: 0, width: 1320, height: 860),
                             styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                             backing: .buffered, defer: false)
        super.init()
        window.title = "xctx"
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.appearance = NSAppearance(named: .darkAqua)
        window.minSize = NSSize(width: 760, height: 460)
        window.delegate = self
        window.isReleasedWhenClosed = false
        window.onResponder = { [weak self] in self?.followResponder() }
        window.contentView = buildRoot()
        window.setFrameAutosaveName("xctxMain")
        if window.frame.width < 800 { window.center() }
        index.scan()
        render()
    }

    var space: Space? { store.space(current) }
    var view: SpaceView? { views[current] }

    // MARK: frame

    private func buildRoot() -> NSView {
        let root = NSView()
        root.wantsLayer = true
        root.layer?.backgroundColor = Theme.bg.cgColor
        tabBar.segmentStyle = .separated
        tabBar.trackingMode = .selectOne
        tabBar.target = self
        tabBar.action = #selector(tabClicked)
        tabBar.setAccessibilityLabel("Spaces")
        let newTask = NSButton(title: "New Task", image: NSImage(systemSymbolName: "plus", accessibilityDescription: nil)!,
                               target: self, action: #selector(focusPrompt))
        newTask.bezelStyle = .rounded
        newTask.imagePosition = .imageLeading
        let top = NSStackView(views: [tabBar, NSView(), newTask])
        top.spacing = 8

        prompt.placeholderString = "Type a task and press Return"
        prompt.font = .systemFont(ofSize: 14)
        prompt.bezelStyle = .roundedBezel
        prompt.focusRingType = .none
        prompt.target = self
        prompt.action = #selector(promptEntered)
        prompt.setAccessibilityLabel("Prompt")
        status.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
        status.textColor = Theme.dim
        status.lineBreakMode = .byTruncatingTail

        for v in [top, body, prompt, status] as [NSView] {
            v.translatesAutoresizingMaskIntoConstraints = false
            root.addSubview(v)
        }
        NSLayoutConstraint.activate([
            top.topAnchor.constraint(equalTo: root.topAnchor, constant: 6),
            top.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 84), // clear of the window buttons
            top.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -12),
            top.heightAnchor.constraint(equalToConstant: 26),
            body.topAnchor.constraint(equalTo: top.bottomAnchor, constant: 8),
            body.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 8),
            body.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -8),
            prompt.topAnchor.constraint(equalTo: body.bottomAnchor, constant: 8),
            prompt.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 10),
            prompt.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -10),
            prompt.heightAnchor.constraint(equalToConstant: 30),
            status.topAnchor.constraint(equalTo: prompt.bottomAnchor, constant: 6),
            status.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 14),
            status.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -14),
            status.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -8),
        ])
        return root
    }

    /// Redraw the tab bar, the body for the current tab, the prompt hint and the status line.
    func render() {
        let spaces = store.data.spaces
        if !current.isEmpty, store.space(current) == nil { current = "" }
        tabBar.segmentCount = spaces.count + 1
        tabBar.setLabel("Home", forSegment: 0)
        tabBar.setImage(NSImage(systemSymbolName: "house", accessibilityDescription: nil), forSegment: 0)
        for (i, s) in spaces.enumerated() {
            tabBar.setLabel("\(s.name)  [\(s.shape.rawValue)]", forSegment: i + 1)
            tabBar.setImage(nil, forSegment: i + 1)
        }
        for i in 0..<tabBar.segmentCount { tabBar.setWidth(0, forSegment: i) }
        tabBar.selectedSegment = current.isEmpty ? 0 : (spaces.firstIndex { $0.id == current } ?? -1) + 1

        body.subviews.forEach { $0.removeFromSuperview() }
        let v: NSView
        if let s = space, s.launched {
            v = spaceBody(s)
        } else if let s = space {
            v = draftBody(s)
        } else {
            v = homeBody()
        }
        v.frame = body.bounds
        v.autoresizingMask = [.width, .height]
        body.addSubview(v)
        if let f = view?.focused?.focusView, space?.launched == true { _ = window.makeFirstResponder(f) }
        refreshStatus()
    }

    private func refreshStatus() {
        let pending = store.data.inbox.filter { $0.state == .pending }.count
        if let s = space, s.launched {
            let agents = view.map { v in v.roles.values.filter { $0.hasPrefix("agent:") }.count + v.parked.count } ?? 0
            status.stringValue = "\(s.shape.rawValue)   \(s.repos.map(\.name).joined(separator: " ↔ "))   \(agents) agent\(agents == 1 ? "" : "s")   PORT \(s.port)   MISE_ENV=\(s.id)   inbox \(pending)   ⌥⌘1 dev  ⌥⌘2 debug  ⌥⌘3 review  ⇧⌘S ship"
            prompt.placeholderString = "Talk to the focused agent, or ⌘N for a new task"
        } else if let s = space {
            status.stringValue = "not launched   \(s.repos.count) repo\(s.repos.count == 1 ? "" : "s")   PORT \(s.port)   ⌘↩ launch   discard closes it"
            prompt.placeholderString = "Type another task, or ⌘↩ to launch this one"
        } else {
            status.stringValue = "\(store.data.spaces.count) space\(store.data.spaces.count == 1 ? "" : "s") alive   inbox \(pending)   \(index.repos.count) repos indexed   ⇧⌘] next space   ⌘N new task"
            prompt.placeholderString = "Type a task and press Return. xctx finds the repos."
        }
    }

    @objc private func tabClicked() {
        let i = tabBar.selectedSegment
        open(i == 0 ? "" : store.data.spaces[safe: i - 1]?.id ?? "")
    }

    func open(_ id: String) {
        current = id
        if let s = store.space(id), s.launched { ensureView(s) }
        render()
    }

    func cycleSpace(_ delta: Int) {
        let ids = [""] + store.data.spaces.map(\.id)
        let i = ids.firstIndex(of: current) ?? 0
        open(ids[((i + delta) % ids.count + ids.count) % ids.count])
    }

    @objc func focusPrompt() {
        _ = window.makeFirstResponder(prompt)
    }

    @objc private func promptEntered() {
        let text = prompt.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return }
        prompt.stringValue = ""
        if let s = space, s.launched, let t = (view?.focused as? TerminalPane) ?? agentPanes(s).first {
            t.send(text + "\r") // talk to an agent
            if let id = view?.panes.first(where: { $0.value === t })?.key { focusPane(id) }
            return
        }
        let d = propose(task: text)
        open(d.id)
    }

    // MARK: home

    private func section(_ title: String) -> NSTextField {
        let l = NSTextField(labelWithString: title.uppercased())
        l.font = .systemFont(ofSize: 11, weight: .semibold)
        l.textColor = Theme.dim
        return l
    }

    private func label(_ s: String, size: CGFloat = 13, color: NSColor = Theme.ink, weight: NSFont.Weight = .regular) -> NSTextField {
        let l = NSTextField(labelWithString: s)
        l.font = .systemFont(ofSize: size, weight: weight)
        l.textColor = color
        l.lineBreakMode = .byTruncatingTail
        l.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return l
    }

    private func button(_ title: String, _ action: Selector, tag: Int = 0, id: String? = nil) -> NSButton {
        let b = NSButton(title: title, target: self, action: action)
        b.bezelStyle = .rounded
        b.tag = tag
        b.identifier = id.map { NSUserInterfaceItemIdentifier($0) }
        return b
    }

    private func row(_ views: [NSView]) -> NSStackView {
        let r = NSStackView(views: views)
        r.spacing = 10
        r.alignment = .centerY
        return r
    }

    /// Scrollable column of rows, aligned to the top.
    private func page(_ rows: [NSView]) -> NSView {
        let col = NSStackView(views: rows)
        col.orientation = .vertical
        col.alignment = .leading
        col.spacing = 10
        col.edgeInsets = NSEdgeInsets(top: 16, left: 24, bottom: 24, right: 24)
        col.translatesAutoresizingMaskIntoConstraints = false
        let doc = FlippedView()
        doc.addSubview(col)
        let scroll = NSScrollView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.documentView = doc
        doc.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            doc.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
            col.topAnchor.constraint(equalTo: doc.topAnchor),
            col.leadingAnchor.constraint(equalTo: doc.leadingAnchor),
            col.trailingAnchor.constraint(lessThanOrEqualTo: doc.trailingAnchor),
            col.bottomAnchor.constraint(equalTo: doc.bottomAnchor),
        ])
        return scroll
    }

    private func homeBody() -> NSView {
        var rows: [NSView] = [label("Every living space, and what each needs from you", size: 20, weight: .semibold),
                              label("Type a task below. The repos it touches are found in the folders you index.", color: Theme.dim)]
        rows.append(spacer(8))
        rows.append(section("Living spaces"))
        if store.data.spaces.isEmpty {
            rows.append(label("No spaces yet.", color: Theme.dim))
        }
        for (i, s) in store.data.spaces.enumerated() {
            let open = button("Open", #selector(openSpaceButton(_:)), tag: i)
            rows.append(row([label(s.name, weight: .medium), label(s.shape.rawValue, color: Theme.accent),
                             label(s.repos.map(\.name).joined(separator: " ↔ "), color: Theme.dim), open]))
        }
        rows.append(spacer(8))
        rows.append(section("Inbox"))
        let inbox = store.data.inbox.suffix(12).reversed()
        if inbox.isEmpty { rows.append(label("Nothing needs you.", color: Theme.dim)) }
        for item in inbox {
            let who = store.space(item.space)?.name ?? item.space
            var r: [NSView] = [label(who, color: Theme.dim), label(item.title, weight: .medium), label(item.detail, color: Theme.dim)]
            if item.state == .pending {
                r.append(button("Approve", #selector(answerGate(_:)), id: "approve:\(item.id)"))
                r.append(button("Deny", #selector(answerGate(_:)), id: "deny:\(item.id)"))
            } else {
                r.append(label(item.state.rawValue, color: item.state == .approved ? Theme.accent : .systemRed))
            }
            rows.append(row(r))
        }
        rows.append(spacer(8))
        rows.append(section("Indexed folders"))
        if index.roots.isEmpty {
            rows.append(label("Choose the folders where your code lives. Only their git repos are read, on this Mac.", color: Theme.dim))
            for (i, c) in IndexCandidates.found.enumerated() {
                rows.append(row([label(c.path), label("\(c.repos) repos", color: Theme.dim),
                                 button("Index", #selector(addCandidate(_:)), tag: i)]))
            }
        }
        for (i, r) in index.roots.enumerated() {
            let n = index.repos.filter { $0.path.hasPrefix(r + "/") || $0.path == r }.count
            rows.append(row([label(r), label("\(n) repos", color: Theme.dim), button("Remove", #selector(removeRoot(_:)), tag: i)]))
        }
        rows.append(row([button("Add Folder…", #selector(addFolder)), button("Rescan", #selector(rescan))]))
        return page(rows)
    }

    private func spacer(_ h: CGFloat) -> NSView {
        let v = NSView()
        v.heightAnchor.constraint(equalToConstant: h).isActive = true
        return v
    }

    @objc private func openSpaceButton(_ b: NSButton) {
        if let s = store.data.spaces[safe: b.tag] { open(s.id) }
    }

    @objc private func answerGate(_ b: NSButton) {
        guard let raw = b.identifier?.rawValue, let colon = raw.firstIndex(of: ":") else { return }
        store.answer(String(raw[raw.index(after: colon)...]), raw.hasPrefix("approve") ? .approved : .denied)
        render()
    }

    @objc private func addCandidate(_ b: NSButton) {
        if let c = IndexCandidates.found[safe: b.tag] { setRoots(index.roots + [c.path]) }
    }

    @objc private func removeRoot(_ b: NSButton) {
        var r = index.roots
        if r.indices.contains(b.tag) { r.remove(at: b.tag) }
        setRoots(r)
    }

    @objc func addFolder() {
        let p = NSOpenPanel()
        p.canChooseDirectories = true
        p.canChooseFiles = false
        p.allowsMultipleSelection = true
        p.message = "Choose folders that hold your git repositories"
        p.prompt = "Index"
        p.beginSheetModal(for: window) { [weak self] r in
            guard let self, r == .OK else { return }
            self.setRoots(self.index.roots + p.urls.map(\.path))
        }
    }

    @objc private func rescan() {
        index.scan()
        render()
    }

    func setRoots(_ roots: [String]) {
        var seen = Set<String>()
        let r = roots.filter { seen.insert($0).inserted }
        index.setRoots(r)
        store.data.roots = r
        store.save()
        index.scan()
        render()
    }

    // MARK: drafts

    /// A proposal for `task` with the repos the index finds (the best, and any close to it).
    @discardableResult
    func propose(task: String, repos: [String]? = nil) -> Space {
        let found = index.search(task)
        let best = found.first?.score ?? 0
        let picked = repos ?? found.filter { $0.score * 2 >= best }.prefix(3).map(\.repo.path)
        let s = store.propose(task: task, repos: picked)
        render()
        return s
    }

    private func draftBody(_ s: Space) -> NSView {
        var rows: [NSView] = [label(s.task, size: 20, weight: .semibold),
                              label("Not launched. Nothing runs until you launch.", color: Theme.dim), spacer(6),
                              section("It touches")]
        let found = index.search(s.task, limit: 8) + (extraFound[s.id] ?? [])
        var shown = Set<String>()
        let candidates = s.repos.map { r in found.first { $0.repo.path == r.repo } ?? RepoMatch(repo: IndexedRepo(path: r.repo), score: 0, why: []) }
            + found.filter { m in !s.repos.contains { $0.repo == m.repo.path } }
        if candidates.isEmpty {
            rows.append(label(index.repos.isEmpty ? "No repos indexed yet. Add a folder on Home." : "No indexed repo matches this task. Find one below.",
                              color: Theme.dim))
        }
        for m in candidates where shown.insert(m.repo.path).inserted {
            let on = s.repos.contains { $0.repo == m.repo.path }
            let box = NSButton(checkboxWithTitle: m.repo.name, target: self, action: #selector(toggleRepo(_:)))
            box.state = on ? NSControl.StateValue.on : .off
            box.identifier = NSUserInterfaceItemIdentifier(m.repo.path)
            rows.append(row([box, label(m.why.joined(separator: "  "), color: Theme.dim), label(m.repo.path, size: 11, color: Theme.dim)]))
        }
        let find = NSSearchField()
        find.placeholderString = "Find another repo"
        find.target = self
        find.action = #selector(findRepo(_:))
        find.sendsSearchStringImmediately = false
        find.widthAnchor.constraint(equalToConstant: 280).isActive = true
        rows.append(row([find]))
        rows.append(spacer(6))
        rows.append(section("Shape"))
        let shape = NSSegmentedControl(labels: ["dev", "debug"], trackingMode: .selectOne, target: nil, action: nil)
        shape.selectedSegment = 0
        shape.identifier = NSUserInterfaceItemIdentifier("shape")
        rows.append(row([shape, label("dev: an agent per repo, with a shell under each.  debug: agents beside shells for logs and repro.",
                                      color: Theme.dim)]))
        rows.append(row([label("Each repo gets a worktree on \(s.branch), PORT \(s.port) and MISE_ENV=\(s.id).", color: Theme.dim)]))
        rows.append(spacer(6))
        let launch = button("Launch", #selector(launchButton(_:)))
        launch.keyEquivalent = "\r"
        launch.keyEquivalentModifierMask = .command
        launch.isEnabled = !s.repos.isEmpty
        rows.append(row([launch, button("Discard", #selector(discardButton))]))
        let v = page(rows)
        draftShape = shape
        return v
    }

    private weak var draftShape: NSSegmentedControl?

    @objc private func toggleRepo(_ b: NSButton) {
        guard var s = space, let path = b.identifier?.rawValue else { return }
        if b.state == .on { if !s.repos.contains(where: { $0.repo == path }) { s.repos.append(SpaceRepo(repo: path)) } }
        else { s.repos.removeAll { $0.repo == path } }
        store.update(s)
        render()
    }

    @objc private func findRepo(_ f: NSSearchField) {
        guard let s = space else { return }
        let q = f.stringValue.lowercased()
        guard !q.isEmpty else { return }
        let hits = index.repos.filter { $0.name.lowercased().contains(q) }.prefix(5).map { RepoMatch(repo: $0, score: 0, why: ["name"]) }
        extraFound[s.id, default: []].append(contentsOf: hits.isEmpty ? index.search(q) : Array(hits))
        render()
    }

    @objc private func launchButton(_ sender: Any?) {
        guard let s = space, !s.launched else { return }
        launch(s.id, shape: draftShape?.selectedSegment == 1 ? .debug : .dev)
    }

    @objc func launchCurrent() { launchButton(nil) }

    @objc private func discardButton() {
        if let s = space { discard(s.id) }
    }

    @discardableResult
    func launch(_ id: String, shape: Shape = .dev) -> String? {
        guard let s = store.space(id), !s.launched else { return "not a draft" }
        guard !s.repos.isEmpty else { return "pick at least one repo" }
        do {
            let l = try SpaceOps.launch(s, shape: shape)
            store.update(l)
            extraFound[id] = nil
            ensureView(l)
            render()
            return nil
        } catch {
            alert("Cannot launch \(s.name)", "\(error)")
            return "\(error)"
        }
    }

    /// Close a space without shipping: drafts just go; launched spaces remove their worktrees (branches stay).
    func discard(_ id: String, confirm: Bool = true) {
        guard let s = store.space(id) else { return }
        let go = { [weak self] in
            guard let self else { return }
            do {
                if s.launched { try SpaceOps.dissolve(s) }
                self.closeAll(id)
                self.store.remove(id)
                self.current = ""
                self.render()
            } catch {
                self.alert("Cannot discard \(s.name)", "\(error)\n\nCommit or stash the changes first; nothing was removed.")
            }
        }
        guard confirm, s.launched else { return go() }
        let a = NSAlert()
        a.messageText = "Discard \(s.name)?"
        a.informativeText = "Stops its agents and removes its worktrees. The branch \(s.branch) is kept in each repo."
        a.alertStyle = .warning
        a.addButton(withTitle: "Discard")
        a.addButton(withTitle: "Cancel")
        a.beginSheetModal(for: window) { if $0 == .alertFirstButtonReturn { go() } }
    }

    @objc func shipCurrent() {
        guard let s = space, s.launched else { return }
        let plan = SpaceOps.plan(s)
        let a = NSAlert()
        a.messageText = "Ship \(s.name)?"
        a.informativeText = plan.map { l in
            l.dirty ? "\(l.repo): uncommitted changes (commit them first)" :
            l.ahead == 0 ? "\(l.repo): no new commits" :
            l.remote ? "\(l.repo): push \(l.ahead) commit(s) and open a pull request" : "\(l.repo): \(l.ahead) commit(s), no remote: the branch is kept"
        }.joined(separator: "\n") + "\n\nThen the worktrees are removed and the space closes."
        a.addButton(withTitle: "Ship")
        a.addButton(withTitle: "Cancel")
        a.beginSheetModal(for: window) { [weak self] r in
            guard let self, r == .alertFirstButtonReturn else { return }
            do {
                let report = try SpaceOps.ship(s)
                self.closeAll(s.id)
                self.store.remove(s.id)
                self.current = ""
                self.render()
                self.alert("Shipped \(s.name)", report.joined(separator: "\n"))
            } catch {
                self.alert("Cannot ship \(s.name)", "\(error)")
            }
        }
    }

    private func closeAll(_ id: String) {
        guard let v = views.removeValue(forKey: id) else { return }
        (Array(v.panes.values) + Array(v.parked.values)).forEach { $0.close() }
    }

    func alert(_ title: String, _ text: String) {
        let a = NSAlert()
        a.messageText = title
        a.informativeText = text
        if window.isVisible, window.attachedSheet == nil { a.beginSheetModal(for: window) } else { a.runModal() }
    }

    // MARK: launched spaces: shapes

    func ensureView(_ s: Space) {
        if views[s.id] == nil {
            let v = SpaceView(id: s.id)
            views[s.id] = v
            compose(v, s)
        }
    }

    /// Lay a space out for its shape. Agents are kept across reshapes (parked when the shape hides them); shells,
    /// git and extra panes are rebuilt.
    func compose(_ v: SpaceView, _ s: Space) {
        let names = s.repos.map(\.name)
        var old: [String: PaneContent] = v.parked
        for (id, role) in v.roles { if let c = v.panes[id] { old[role] = c } }
        let groups: [[String]]
        var sideBySide = true // how the groups sit
        var ratio = 0.5
        switch s.shape {
        case .new, .dev:
            groups = [names.map { "agent:\($0)" }, names.map { "shell:\($0)" }]
            sideBySide = false
            ratio = 0.68
        case .debug:
            groups = [names.map { "agent:\($0)" }, names.map { "shell:\($0)" }]
            ratio = 0.5
        case .review:
            groups = [names.map { "git:\($0)" }]
        }
        v.panes = [:]
        v.roles = [:]
        v.containers = [:]
        v.parked = [:]
        var id = 1
        var lines: [PaneNode] = []
        for g in groups {
            var leaves: [PaneNode] = []
            for role in g {
                let content = old.removeValue(forKey: role) ?? make(role: role, s)
                attach(content, to: v, id: id, role: role)
                leaves.append(.leaf(id))
                id += 1
            }
            lines.append(PaneTree.line(leaves, sideBySide: !sideBySide, nextId: &id)) // rows of a stack, columns of a row
        }
        var root = lines[0]
        if lines.count > 1 {
            root = .split(id: id, sideBySide: sideBySide, ratio: ratio, a: lines[0], b: PaneTree.line(Array(lines.dropFirst()), sideBySide: sideBySide, nextId: &id))
            id += 1
        }
        v.tree = PaneTree(root: root, focus: 1)
        for (role, c) in old {
            if role.hasPrefix("agent:") { v.parked[role] = c } else { c.close() } // same agents, new panes
        }
    }

    private func worktree(_ s: Space, _ repo: String?) -> String {
        let r = s.repos.first { $0.name == repo } ?? s.repos.first
        return r?.worktree ?? r?.repo ?? NSHomeDirectory()
    }

    private var ctlEnv: [String: String] {
        ["XCTX_CTL": (Bundle.main.executablePath ?? CommandLine.arguments[0]),
         "XCTX_CTL_SOCK": Control.socketPath(dataDir: dataDir)]
    }

    /// What the agent of `repo` runs: `pi`, told about its space and the harness (no first message, so no turn
    /// starts until someone talks to it). XCTX_AGENT_CMD replaces the whole command.
    func agentCommand(_ s: Space, repo: String) -> String {
        if let c = ProcessInfo.processInfo.environment["XCTX_AGENT_CMD"] { return c }
        let others = s.repos.map(\.name).filter { $0 != repo }
        let brief = """
        You are the \(repo) agent of the xctx space "\(s.name)". Task: \(s.task)
        You work in the git worktree at \(worktree(s, repo)) on branch \(s.branch). Write only in this repo.\
        \(others.isEmpty ? "" : " Other agents in this space own \(others.joined(separator: ", ")); agree on the boundary with them instead of editing their repos.")
        PORT=\(s.port) and MISE_ENV=\(s.id) are set for this space; start servers on $PORT.
        You can drive the workspace window: "$XCTX_CTL" ctl <command> key=value ...
          pane.mount kind=browser|shell|git|editor|agent [url=...] [repo=...] [split=right|down]
          page.open url=http://localhost:$PORT     pane.list     pane.read pane=N     pane.send pane=N text=...
          pane.close pane=N     space.shape shape=dev|debug|review
          gate.ask title="..." detail="..."  then  gate.status id=...   (ask before widening the space: new repos, production, migrations)
        Commit your work on \(s.branch); the user ships the space.
        """
        let base = UserDefaults.standard.string(forKey: "agentCommand") ?? "pi"
        return "\(base) --name \(shellQuote("\(s.name) · \(repo)")) --append-system-prompt \(shellQuote(brief))"
    }

    private func make(role: String, _ s: Space) -> PaneContent {
        let parts = role.split(separator: ":", maxSplits: 1).map(String.init)
        let repo = parts.count > 1 ? parts[1] : nil
        let env = SpaceOps.env(s).merging(ctlEnv) { a, _ in a }
        switch parts[0] {
        case "agent":
            return TerminalPane(kind: .agent, cwd: worktree(s, repo), env: env, command: agentCommand(s, repo: repo ?? ""), title: "\(repo ?? "") agent")
        case "git":
            return TerminalPane(kind: .git, cwd: worktree(s, repo), env: env, title: "\(repo ?? "") git")
        default:
            return TerminalPane(kind: .shell, cwd: worktree(s, repo), env: env, title: "\(repo ?? "") shell")
        }
    }

    func makeKind(_ kind: PaneKind, _ s: Space, repo: String?, url: String?, cmd: String?) -> PaneContent {
        let env = SpaceOps.env(s).merging(ctlEnv) { a, _ in a }
        switch kind {
        case .browser:
            return BrowserPane(url: url ?? "http://localhost:\(s.port)")
        case .agent:
            return TerminalPane(kind: .agent, cwd: worktree(s, repo), env: env, command: cmd ?? agentCommand(s, repo: repo ?? s.repos.first?.name ?? ""))
        default:
            return TerminalPane(kind: kind, cwd: worktree(s, repo), env: env, command: cmd)
        }
    }

    /// Put `content` in pane `id`, wiring its title, exit and launcher callbacks.
    private func attach(_ content: PaneContent, to v: SpaceView, id: Int, role: String) {
        if let old = v.panes[id], old !== content { old.close() }
        v.panes[id] = content
        v.roles[id] = role
        content.onChange = { [weak v] in v?.containers[id]?.refreshTitle() }
        if let t = content as? TerminalPane, t.kind == .git, role.hasPrefix("extra:") {
            t.onExit = { [weak self, weak v] _ in if let self, let v { self.closePane(id, in: v) } }
        }
        if let l = content as? LauncherPane {
            l.onChoose = { [weak self, weak v] k in
                guard let self, let v, let s = self.store.space(v.id) else { return }
                self.attach(self.makeKind(k, s, repo: nil, url: nil, cmd: nil), to: v, id: id, role: role)
                v.tree.focus = id
                self.render()
            }
        }
        if let c = v.containers[id] {
            c.setContent(content)
        } else {
            let c = PaneContainer(id: id, content: content)
            c.onAction = { [weak self, weak v] action in
                guard let self, let v else { return }
                switch action {
                case "right": self.split(v, id, sideBySide: true)
                case "down": self.split(v, id, sideBySide: false)
                case "close": self.closePane(id, in: v)
                default: self.focusPane(id)
                }
            }
            v.containers[id] = c
        }
    }

    func reshape(_ id: String, _ shape: Shape) {
        guard var s = store.space(id), s.launched, shape != .new else { return }
        s.shape = shape
        store.update(s)
        if let v = views[id] { compose(v, s) } else { ensureView(s) }
        render()
    }

    @objc func reshapeMenu(_ item: NSMenuItem) {
        if let s = space, let shape = Shape(rawValue: item.representedObject as? String ?? "") { reshape(s.id, shape) }
    }

    // MARK: panes

    func agentPanes(_ s: Space) -> [TerminalPane] {
        guard let v = views[s.id] else { return [] }
        return v.roles.sorted { $0.key < $1.key }.filter { $0.value.hasPrefix("agent:") }.compactMap { v.panes[$0.key] as? TerminalPane }
    }

    @discardableResult
    func split(_ v: SpaceView, _ pane: Int, sideBySide: Bool, with content: PaneContent? = nil) -> Int? {
        guard let new = v.tree.split(pane, sideBySide: sideBySide) else { return nil }
        v.extras += 1
        attach(content ?? LauncherPane(), to: v, id: new, role: "extra:\(v.extras)")
        if current == v.id { render() }
        return new
    }

    func splitFocused(sideBySide: Bool) {
        if let v = view { split(v, v.tree.focus, sideBySide: sideBySide) }
    }

    func mountFocused(_ kind: PaneKind) {
        guard let s = space, s.launched, let v = view else {
            return alert("Open a launched space first", "Panes belong to a space. Launch one, or open it from Home.")
        }
        split(v, v.tree.focus, sideBySide: true, with: makeKind(kind, s, repo: nil, url: nil, cmd: nil))
    }

    /// Close a pane. The last one becomes an empty launcher, so a space always shows something.
    func closePane(_ id: Int, in v: SpaceView) {
        let role = v.roles[id] ?? ""
        if v.tree.leaves.count == 1 {
            attach(LauncherPane(), to: v, id: id, role: role)
        } else {
            if role.hasPrefix("agent:"), let c = v.panes[id] { v.parked[role] = c } else { v.panes[id]?.close() }
            v.panes.removeValue(forKey: id)
            v.roles.removeValue(forKey: id)
            v.containers.removeValue(forKey: id)
            v.tree.close(id)
        }
        if current == v.id { render() }
    }

    func closeFocused() {
        if let v = view { closePane(v.tree.focus, in: v) }
    }

    func focusPane(_ id: Int) {
        guard let v = view else { return }
        v.tree.focus = id
        if let f = v.panes[id]?.focusView { _ = window.makeFirstResponder(f) }
        refreshFocus()
    }

    func cycleFocus(_ delta: Int) {
        guard let v = view else { return }
        v.tree.cycleFocus(delta)
        focusPane(v.tree.focus)
    }

    private func followResponder() {
        guard let v = view, var r = window.firstResponder as? NSView else { return }
        while let p = r.superview, !(r is PaneContainer) { r = p }
        guard let c = r as? PaneContainer, v.containers[c.paneId] === c, v.tree.focus != c.paneId else { return }
        v.tree.focus = c.paneId
        refreshFocus()
    }

    private func refreshFocus() {
        guard let v = view else { return }
        for (id, c) in v.containers { c.focused = id == v.tree.focus && v.containers.count > 1 }
    }

    private func spaceBody(_ s: Space) -> NSView {
        ensureView(s)
        guard let v = views[s.id] else { return NSView() }
        let host = NSView()
        let tree = build(v, v.tree.root)
        tree.frame = body.bounds
        tree.autoresizingMask = [.width, .height]
        host.addSubview(tree)
        refreshFocus()
        return host
    }

    private func build(_ v: SpaceView, _ node: PaneNode) -> NSView {
        switch node {
        case .leaf(let id):
            return v.containers[id] ?? NSView()
        case .split(let id, let sideBySide, let ratio, let a, let b):
            let sv = PaneSplitView(id: id, sideBySide: sideBySide, ratio: ratio)
            sv.onRatio = { [weak v] split, r in v?.tree.setRatio(split: split, r) }
            sv.addSubview(build(v, a))
            sv.addSubview(build(v, b))
            return sv
        }
    }

    var focusedBrowser: BrowserPane? { view?.focused as? BrowserPane }

    func windowWillClose(_ notification: Notification) {
        for id in Array(views.keys) { closeAll(id) }
        NSApp.terminate(nil)
    }
}

/// Code folders found on this Mac, for the first-run index choice (computed once).
@MainActor enum IndexCandidates {
    static let found = RepoIndex.candidates()
}

final class FlippedView: NSView {
    override var isFlipped: Bool { true }
}

extension Array {
    subscript(safe i: Int) -> Element? { indices.contains(i) ? self[i] : nil }
}

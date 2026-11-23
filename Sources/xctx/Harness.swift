import AppKit
import WorkspaceCore

/// The harness: a unix socket (`control.sock` in the data folder, owner-only) that the parent agent and the
/// space agents use to shape the window. One JSON request per connection; handled on the main thread.
final class ControlServer: @unchecked Sendable {
    let path: String
    private var fd: Int32 = -1
    private let handle: @MainActor ([String: Any]) -> [String: Any]

    init(path: String, handle: @escaping @MainActor ([String: Any]) -> [String: Any]) {
        self.path = path
        self.handle = handle
    }

    func start() throws {
        unlink(path)
        fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw GitError(description: "socket failed") }
        var addr = Control.unixAddress(path)
        let listener = fd
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(listener, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard bound == 0 else { throw GitError(description: "cannot bind \(path): \(String(cString: strerror(errno)))") }
        chmod(path, 0o600)
        listen(listener, 16)
        Thread.detachNewThread { [self] in
            while true {
                let c = accept(listener, nil, nil)
                if c < 0 { if errno == EINTR { continue } else { return } }
                serve(c)
            }
        }
    }

    private func serve(_ c: Int32) {
        defer { close(c) }
        var tv = timeval(tv_sec: 5, tv_usec: 0)
        setsockopt(c, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        var data = Data()
        var buf = [UInt8](repeating: 0, count: 65536)
        while data.count < 4 << 20 {
            let n = read(c, &buf, buf.count)
            if n <= 0 { break }
            data.append(buf, count: n)
            if buf[n - 1] == 10 { break }
        }
        let req = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        let reply: [String: Any] = req.map { r in DispatchQueue.main.sync { MainActor.assumeIsolated { handle(r) } } }
            ?? ["ok": false, "error": "send one JSON object per line"]
        var out = (try? JSONSerialization.data(withJSONObject: reply)) ?? Data("{\"ok\":false}".utf8)
        out.append(10)
        _ = out.withUnsafeBytes { write(c, $0.baseAddress, $0.count) }
    }
}

extension SpacesController {
    nonisolated static let commands = ["spaces.list", "index.search", "space.propose", "space.launch", "space.shape", "space.open",
                           "pane.list", "pane.mount", "pane.close", "pane.send", "pane.read", "page.open", "gate.ask", "gate.status"]

    /// One harness request. Every reply has `ok`; failures carry `error`.
    func handle(_ req: [String: Any]) -> [String: Any] {
        func str(_ k: String) -> String? {
            if let s = req[k] as? String, !s.isEmpty { return s }
            return (req[k] as? NSNumber)?.stringValue
        }
        func fail(_ e: String) -> [String: Any] { ["ok": false, "error": e] }
        let cmd = str("cmd") ?? ""
        let spaceId = str("space") ?? (current.isEmpty ? nil : current)

        // commands that don't need a space
        switch cmd {
        case "spaces.list":
            return ["ok": true, "spaces": store.data.spaces.map(describe)]
        case "index.search":
            guard let q = str("query") else { return fail("query= is required") }
            return ["ok": true, "repos": index.search(q, limit: Int(str("limit") ?? "") ?? 5).map {
                ["name": $0.repo.name, "path": $0.repo.path, "score": $0.score, "why": $0.why]
            }]
        case "space.propose":
            guard let task = str("task") else { return fail("task= is required") }
            let repos = str("repos")?.split(separator: ",").map { r -> String in
                let r = String(r).trimmingCharacters(in: .whitespaces)
                return index.repos.first { $0.name == r }?.path ?? r
            }
            let keep = current
            let s = propose(task: task, repos: repos)
            current = keep // a new tab appears; the user stays where they are
            render()
            return ["ok": true, "space": describe(s)]
        case "gate.status":
            guard let id = str("id"), let item = store.data.inbox.first(where: { $0.id == id }) else { return fail("no such gate") }
            return ["ok": true, "state": item.state.rawValue]
        case "":
            return fail("cmd= is required; one of " + Self.commands.joined(separator: " "))
        default:
            break
        }

        guard let id = spaceId, var s = store.space(id) else { return fail("space= is required (no space is open)") }
        switch cmd {
        case "space.open":
            open(id)
            return ["ok": true]
        case "space.launch":
            if let e = launch(id, shape: Shape(rawValue: str("shape") ?? "") ?? .dev) { return fail(e) }
            return ["ok": true, "space": describe(store.space(id)!)]
        case "space.shape":
            guard let shape = Shape(rawValue: str("shape") ?? ""), shape != .new else { return fail("shape= is dev, debug or review") }
            guard s.launched else { return fail("launch the space first") }
            reshape(id, shape)
            return ["ok": true, "space": describe(store.space(id)!)]
        case "gate.ask":
            guard let title = str("title") else { return fail("title= is required") }
            let item = store.ask(space: id, title: title, detail: str("detail") ?? "")
            render()
            NSApp.requestUserAttention(.informationalRequest)
            return ["ok": true, "id": item.id, "state": item.state.rawValue]
        default:
            break
        }

        guard s.launched else { return fail("\(s.name) is not launched") }
        ensureView(s)
        guard let v = views[id] else { return fail("no panes") }
        s = store.space(id)!
        let pane = Int(str("pane") ?? "")
        switch cmd {
        case "pane.list":
            return ["ok": true, "focus": v.tree.focus, "panes": v.tree.leaves.map { p -> [String: Any] in
                let c = v.panes[p]
                return ["pane": p, "role": v.roles[p] ?? "", "kind": c?.kind.map { "\($0)" } ?? "empty", "title": c?.title ?? ""]
            }]
        case "pane.mount":
            guard let kind = PaneKind.allCases.first(where: { "\($0)" == str("kind") }) else {
                return fail("kind= is one of " + PaneKind.allCases.map { "\($0)" }.joined(separator: " "))
            }
            let target = pane ?? v.tree.focus
            guard v.tree.leaves.contains(target) else { return fail("no pane \(target)") }
            let content = makeKind(kind, s, repo: str("repo"), url: str("url"), cmd: str("cmd"))
            guard let new = split(v, target, sideBySide: str("split") != "down", with: content) else { return fail("cannot split") }
            if current == id { render() }
            return ["ok": true, "pane": new]
        case "pane.close":
            guard let p = pane, v.tree.leaves.contains(p) else { return fail("pane= names a pane from pane.list") }
            closePane(p, in: v)
            return ["ok": true]
        case "pane.send":
            guard let p = pane, let t = v.panes[p] as? TerminalPane else { return fail("pane= must be a terminal pane") }
            t.send((str("text") ?? "") + (str("enter") == "false" ? "" : "\r"))
            return ["ok": true]
        case "pane.read":
            guard let p = pane, let c = v.panes[p] else { return fail("pane= names a pane from pane.list") }
            if let t = c as? TerminalPane { return ["ok": true, "text": t.screenText()] }
            if let b = c as? BrowserPane { return ["ok": true, "title": b.title, "url": b.web.url?.absoluteString ?? ""] }
            return ["ok": true, "text": ""]
        case "page.open":
            guard let url = str("url") else { return fail("url= is required") }
            if let (p, b) = v.panes.first(where: { $0.value is BrowserPane }).map({ ($0.key, $0.value as! BrowserPane) }) {
                b.load(url)
                return ["ok": true, "pane": p]
            }
            guard let new = split(v, v.tree.focus, sideBySide: true, with: BrowserPane(url: url)) else { return fail("cannot split") }
            return ["ok": true, "pane": new]
        default:
            return fail("unknown cmd \(cmd); one of " + Self.commands.joined(separator: " "))
        }
    }

    private func describe(_ s: Space) -> [String: Any] {
        ["id": s.id, "name": s.name, "task": s.task, "shape": s.shape.rawValue, "port": s.port, "branch": s.branch,
         "repos": s.repos.map { ["name": $0.name, "repo": $0.repo, "worktree": $0.worktree ?? ""] }]
    }
}

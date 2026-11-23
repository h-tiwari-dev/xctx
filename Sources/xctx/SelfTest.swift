import AppKit
import WebKit
import WorkspaceCore

/// XCTX_SELFTEST=<file>: drive the real window (real ptys, real WebKit, the real harness socket) and write one
/// `PASS name` / `FAIL name: detail` line per check to <file>, then quit. Run it with XCTX_INDEX_ROOTS pointing at
/// throwaway repos (see selftest.sh); it launches and discards a space in them.
@MainActor enum SelfTest {
    static func run(_ c: SpacesController, out: String) {
        Task { @MainActor in
            var lines: [String] = []
            func check(_ name: String, _ ok: Bool, _ detail: @autoclosure () -> String = "") {
                lines.append(ok ? "PASS \(name)" : "FAIL \(name): \(detail())")
            }
            func until(_ secs: Double, _ cond: () -> Bool) async -> Bool {
                let end = Date().addingTimeInterval(secs)
                while Date() < end {
                    if cond() { return true }
                    try? await Task.sleep(nanoseconds: 100_000_000)
                }
                return cond()
            }
            let sock = Control.socketPath(dataDir: c.dataDir)
            func ctl(_ req: [String: String]) async -> [String: Any] {
                await Task.detached { (try? Control.send(req, socket: sock)) ?? ["ok": false, "error": "no reply"] }.value
            }
            try? await Task.sleep(nanoseconds: 600_000_000)

            // home and the index
            check("window is on screen", c.window.isVisible)
            check("index finds the repos", Set(c.index.repos.map(\.name)) == ["payments", "checkout-web"], c.index.repos.map(\.name).description)

            // a task typed in the prompt becomes a draft with the repos it touches
            c.prompt.stringValue = "add rate limiting to checkout payments"
            c.prompt.sendAction(c.prompt.action, to: c.prompt.target)
            guard let draft = c.space else { check("prompt proposes a space", false, "no space"); return finish(lines, out) }
            check("prompt proposes a space and opens it", !draft.launched && c.current == draft.id, "\(draft)")
            check("the draft found both repos", Set(draft.repos.map(\.name)) == ["payments", "checkout-web"], draft.repos.map(\.name).description)

            // launch: worktrees, mise env, an agent and a shell per repo
            c.launchCurrent()
            guard let s = c.space, s.launched, let v = c.view else { check("launch", false, "\(String(describing: c.space))"); return finish(lines, out) }
            check("launch makes a worktree per repo", s.repos.allSatisfy { $0.worktree.map { FileManager.default.fileExists(atPath: $0) } ?? false },
                  s.repos.map { $0.worktree ?? "nil" }.description)
            check("each worktree has its mise env", s.repos.allSatisfy { r in
                (try? String(contentsOfFile: (r.worktree ?? "") + "/" + s.miseFile, encoding: .utf8))?.contains("PORT = \"\(s.port)\"") ?? false
            })
            check("dev shape: agents over shells", Set(v.roles.values) == ["agent:payments", "agent:checkout-web", "shell:payments", "shell:checkout-web"],
                  v.roles.description)
            let shellId = v.roles.first { $0.value == "shell:payments" }!.key
            let shell = v.panes[shellId] as! TerminalPane
            _ = await until(6) { shell.screenText().count > 2 }
            shell.send("echo port=$PORT env=$MISE_ENV dir=$(basename $(pwd))\r")
            let envOk = await until(8) { shell.screenText().contains("port=\(s.port) env=\(s.id) dir=\(s.id)") }
            check("terminals run in the worktree with the space env", envOk, String(shell.screenText().suffix(300)))

            // an agent's path: the ctl CLI from inside the space mounts a browser
            shell.send("\"$XCTX_CTL\" ctl pane.mount kind=browser 'url=data:text/html,<title>from-agent</title>' split=down\r")
            let mounted = await until(10) { v.panes.values.contains { ($0 as? BrowserPane)?.web.title == "from-agent" } }
            check("ctl from a space terminal mounts a browser pane", mounted, String(shell.screenText().suffix(400)))

            // the harness over the socket
            let list = await ctl(["cmd": "pane.list", "space": s.id])
            check("pane.list over the socket", (list["panes"] as? [[String: Any]])?.count == 5, "\(list)")
            let gate = await ctl(["cmd": "gate.ask", "space": s.id, "title": "run a migration", "detail": "orders table"])
            let gateId = gate["id"] as? String ?? ""
            check("gate.ask lands in the inbox", c.store.data.inbox.contains { $0.id == gateId && $0.state == .pending }, "\(gate)")
            c.store.answer(gateId, .approved)
            let gs = await ctl(["cmd": "gate.status", "id": gateId])
            check("gate.status reports the answer", gs["state"] as? String == "approved", "\(gs)")

            // reshape keeps the same agents
            let agentsBefore = c.agentPanes(s).map(ObjectIdentifier.init)
            let r1 = await ctl(["cmd": "space.shape", "space": s.id, "shape": "review"])
            check("space.shape review: lazygit per repo", r1["ok"] as? Bool == true && Set(v.roles.values) == ["git:payments", "git:checkout-web"],
                  "\(r1) \(v.roles)")
            check("review parks the agents instead of stopping them", v.parked.count == 2 && v.parked.values.allSatisfy { !(($0 as? TerminalPane)?.exited ?? true) },
                  "\(v.parked.keys)")
            _ = await ctl(["cmd": "space.shape", "space": s.id, "shape": "dev"])
            check("back to dev brings the same agents back", Set(c.agentPanes(s).map(ObjectIdentifier.init)) == Set(agentsBefore))
            check("the tab bar shows the shape", c.store.space(s.id)?.shape == .dev)

            // a proposal from the harness appears as a tab without taking the user away
            let p = await ctl(["cmd": "space.propose", "task": "fix checkout copy", "repos": "checkout-web"])
            let pid = (p["space"] as? [String: Any])?["id"] as? String ?? ""
            check("space.propose adds a tab, the user stays put", c.store.space(pid) != nil && c.current == s.id, "\(p)")
            check("spaces persist to disk", (try? String(contentsOf: c.store.file, encoding: .utf8))?.contains(pid) ?? false)
            c.discard(pid, confirm: false)

            // discard removes the worktrees, keeps the branches
            c.discard(s.id, confirm: false)
            check("discard removes the worktrees and the tab", c.store.space(s.id) == nil && s.repos.allSatisfy { !FileManager.default.fileExists(atPath: $0.worktree ?? "/") },
                  "\(c.store.data.spaces.map(\.id))")
            check("the branch is kept", ((try? Git.run(s.repos[0].repo, ["branch"])) ?? "").contains(s.branch))
            check("back on Home", c.current.isEmpty)
            finish(lines, out)
        }
    }

    private static func finish(_ lines: [String], _ out: String) {
        let report = lines.joined(separator: "\n") + "\n"
        try? report.write(toFile: out, atomically: true, encoding: .utf8)
        print(report, terminator: "")
        NSApp.terminate(nil)
    }
}

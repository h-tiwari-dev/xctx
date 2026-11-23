import Foundation
import XCTest
@testable import WorkspaceCore

/// Two small repos (payments, checkout-web) and a non-repo folder under a temp root.
func makeRepos() throws -> String {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("xctx-test-\(UUID().uuidString)")
        .resolvingSymlinksInPath().path.replacingOccurrences(of: "/var/", with: "/private/var/", options: .anchored)
    let files = [
        "code/payments": ["README.md": "Payments service: charges carts.\n", "src/limit.ts": "", "src/charge.ts": ""],
        "code/web/checkout-web": ["README.md": "Checkout frontend\n", "src/Checkout.tsx": ""],
        "code/notes": ["todo.txt": "checkout"],
    ]
    for (dir, fs) in files {
        let d = root + "/" + dir
        try FileManager.default.createDirectory(atPath: d + "/src", withIntermediateDirectories: true)
        for (f, text) in fs { try text.write(toFile: d + "/" + f, atomically: true, encoding: .utf8) }
        if dir != "code/notes" {
            try Git.run(d, ["init", "-q", "-b", "main"])
            try Git.run(d, ["add", "."])
            try Git.run(d, ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "first"])
        }
    }
    return root
}

final class RepoIndexTests: XCTestCase {
    func testScanFindsReposAndSearchRanksByNameThenFiles() throws {
        let root = try makeRepos()
        defer { try? FileManager.default.removeItem(atPath: root) }
        let index = RepoIndex(roots: [root + "/code"])
        index.scan()
        XCTAssertEqual(Set(index.repos.map(\.name)), ["payments", "checkout-web"], "notes has no .git")

        let m = index.search("add rate limiting to checkout payments")
        XCTAssertEqual(m.map(\.repo.name).sorted(), ["checkout-web", "payments"])
        XCTAssertTrue(m.first { $0.repo.name == "payments" }!.why.contains("src/limit.ts"), "\(m)")
        XCTAssertEqual(index.search("charge").first?.repo.name, "payments", "found through a file path")
        XCTAssertTrue(index.search("the and for").isEmpty, "stop words alone match nothing")
    }
}

final class SpaceTests: XCTestCase {
    func testProposeLaunchMiseExcludeAndDissolve() throws {
        let root = try makeRepos()
        defer { try? FileManager.default.removeItem(atPath: root) }
        let store = SpaceStore(dir: URL(fileURLWithPath: root + "/data"))
        let pay = root + "/code/payments", web = root + "/code/web/checkout-web"

        var s = store.propose(task: "Add rate limiting to checkout", repos: [pay, web])
        XCTAssertEqual(s.id, "rate-limiting-checkout")
        XCTAssertEqual(s.port, 4001)
        XCTAssertFalse(s.launched)
        XCTAssertEqual(store.propose(task: "add rate limiting to checkout", repos: []).id, "rate-limiting-checkout-2")
        XCTAssertEqual(store.data.spaces[1].port, 4002, "ports never clash")
        store.remove("rate-limiting-checkout-2")

        s = try SpaceOps.launch(s)
        store.update(s)
        XCTAssertEqual(s.shape, .dev)
        let wt = try XCTUnwrap(s.repos[0].worktree)
        XCTAssertEqual(wt, pay + "-worktrees/rate-limiting-checkout")
        XCTAssertEqual(try Git.run(wt, ["branch", "--show-current"]).trimmingCharacters(in: .whitespacesAndNewlines), "space/rate-limiting-checkout")
        let mise = try String(contentsOfFile: wt + "/mise.rate-limiting-checkout.toml", encoding: .utf8)
        XCTAssertTrue(mise.contains("PORT = \"4001\""), mise)
        XCTAssertFalse(Git.isDirty(wt), "the mise file is ignored, so the worktree stays clean")
        XCTAssertFalse(Git.isDirty(pay), "the main checkout is untouched")
        XCTAssertEqual(SpaceOps.env(s)["MISE_ENV"], "rate-limiting-checkout")

        // reload from disk
        XCTAssertEqual(SpaceStore(dir: URL(fileURLWithPath: root + "/data")).space(s.id), s)

        // ship with no commits and no remote: nothing to push, worktrees go away, branches stay
        try "x".write(toFile: wt + "/new.txt", atomically: true, encoding: .utf8)
        XCTAssertThrowsError(try SpaceOps.ship(s), "uncommitted work blocks shipping")
        try Git.run(wt, ["add", "new.txt"])
        try Git.run(wt, ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "work"])
        XCTAssertEqual(SpaceOps.plan(s).map(\.ahead), [1, 0])
        let report = try SpaceOps.ship(s)
        XCTAssertEqual(report, ["payments: 1 commit(s) kept on space/rate-limiting-checkout (no remote)", "checkout-web: no new commits"])
        XCTAssertFalse(FileManager.default.fileExists(atPath: wt))
        XCTAssertTrue(try Git.run(pay, ["branch"]).contains("space/rate-limiting-checkout"))
    }

    func testLaunchFailureRemovesWhatItMade() throws {
        let root = try makeRepos()
        defer { try? FileManager.default.removeItem(atPath: root) }
        let store = SpaceStore(dir: URL(fileURLWithPath: root + "/data"))
        let pay = root + "/code/payments"
        let s = store.propose(task: "broken", repos: [pay, root + "/code/notes"]) // notes is not a repo
        XCTAssertThrowsError(try SpaceOps.launch(s))
        XCTAssertFalse(FileManager.default.fileExists(atPath: pay + "-worktrees/broken"))
        XCTAssertFalse(try Git.run(pay, ["branch"]).contains("space/broken"))
    }

    func testInboxAndCliRequests() throws {
        let root = try makeRepos()
        defer { try? FileManager.default.removeItem(atPath: root) }
        let store = SpaceStore(dir: URL(fileURLWithPath: root + "/data"))
        let item = store.ask(space: "x", title: "run a migration", detail: "orders")
        XCTAssertEqual(item.state, .pending)
        store.answer(item.id, .approved)
        XCTAssertEqual(store.data.inbox.first?.state, .approved)
        XCTAssertEqual(Control.request(["pane.mount", "kind=browser", "url=http://a/?b=c"], defaultSpace: "s1"),
                       ["cmd": "pane.mount", "kind": "browser", "url": "http://a/?b=c", "space": "s1"])
    }

    func testLineLayoutHasEqualShares() {
        var next = 10
        let n = PaneTree.line([.leaf(1), .leaf(2), .leaf(3)], sideBySide: true, nextId: &next)
        XCTAssertEqual(n, .split(id: 10, sideBySide: true, ratio: 1.0 / 3, a: .leaf(1),
                                 b: .split(id: 11, sideBySide: true, ratio: 0.5, a: .leaf(2), b: .leaf(3))))
        var t = PaneTree(root: n, focus: 1)
        XCTAssertEqual(t.split(3, sideBySide: false), 13, "ids continue after the highest")
    }
}

import Foundation
import XCTest
@testable import WorkspaceCore

final class PaneTreeTests: XCTestCase {
    func testSplitFocusesTheNewPaneAndCloseGivesTheSpaceBack() {
        var t = PaneTree()
        XCTAssertEqual(t.split(1, sideBySide: true), 3)
        XCTAssertEqual(t.leaves, [1, 3])
        XCTAssertEqual(t.focus, 3)
        XCTAssertEqual(t.split(3, sideBySide: false), 5)
        XCTAssertEqual(t.leaves, [1, 3, 5])
        t.setRatio(split: 2, 0.3)
        guard case .split(2, true, 0.3, .leaf(1), _) = t.root else { return XCTFail("\(t.root)") }
        XCTAssertTrue(t.close(5))
        XCTAssertEqual(t.focus, 3)
        XCTAssertTrue(t.close(1))
        XCTAssertEqual(t.root, .leaf(3))
        XCTAssertFalse(t.close(3), "the last pane stays")
    }

    func testCycleFocusWraps() {
        var t = PaneTree()
        t.split(1, sideBySide: true)
        t.cycleFocus(1)
        XCTAssertEqual(t.focus, 1)
        t.cycleFocus(-1)
        XCTAssertEqual(t.focus, 3)
    }
}

final class AddressTests: XCTestCase {
    func testUrlsHostsLocalhostAndSearches() {
        XCTAssertEqual(Address.url(for: "https://example.com/a").absoluteString, "https://example.com/a")
        XCTAssertEqual(Address.url(for: " example.com ").absoluteString, "https://example.com")
        XCTAssertEqual(Address.url(for: "localhost:3000").absoluteString, "http://localhost:3000")
        XCTAssertEqual(Address.url(for: "rust borrow checker").absoluteString, "https://duckduckgo.com/?q=rust%20borrow%20checker")
        XCTAssertEqual(Address.url(for: "data:text/html,hi").scheme, "data")
    }
}

final class GitTests: XCTestCase {
    func testPorcelainParsingSkipsBareAndKeepsDetached() {
        let text = "worktree /r/.bare\nbare\n\nworktree /r/main\nHEAD abc\nbranch refs/heads/main\n\nworktree /r/x\nHEAD def\ndetached\n"
        let (s, bare) = Git.parseWorktrees(text)
        XCTAssertEqual(bare, "/r/.bare")
        XCTAssertEqual(s, [Session(path: "/r/main", branch: "main"), Session(path: "/r/x", branch: nil)])
        XCTAssertEqual(s[1].name, "x")
    }

    func testWorktreeDirs() {
        XCTAssertEqual(Git.worktreeDir(main: "/a/repo", bare: nil, name: "feat/x"), "/a/repo-worktrees/feat-x")
        XCTAssertEqual(Git.worktreeDir(main: "/r/main", bare: "/r/.bare", name: "y"), "/r/y")
    }

    func testCreateAndRemoveASessionInARealRepo() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("xctx-test-\(UUID().uuidString)").resolvingSymlinksInPath().path
            .replacingOccurrences(of: "/var/", with: "/private/var/", options: .anchored)
        let main = root + "/main"
        try FileManager.default.createDirectory(atPath: main, withIntermediateDirectories: true)
        try Git.run(main, ["init", "-q", "-b", "main"])
        try Git.run(main, ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "first"])
        let dir = try Git.createSession(repo: main, name: "feat/x", base: "main")
        XCTAssertEqual(dir, main + "-worktrees/feat-x")
        XCTAssertEqual(try Git.listSessions(repo: main).map(\.name), ["main", "feat/x"])
        XCTAssertThrowsError(try Git.createSession(repo: main, name: "has space", base: "main"))
        try Git.removeSession(repo: main, path: dir)
        XCTAssertEqual(try Git.listSessions(repo: main).map(\.name), ["main"])
        try? FileManager.default.removeItem(atPath: root)
    }
}

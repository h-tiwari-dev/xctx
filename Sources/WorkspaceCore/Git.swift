import Foundation

/// One git worktree of the repository: a session.
public struct Session: Equatable {
    public var path: String
    public var branch: String? // nil when detached
    public var name: String { branch ?? (path as NSString).lastPathComponent }

    public init(path: String, branch: String?) {
        self.path = path
        self.branch = branch
    }
}

public struct GitError: Error, CustomStringConvertible {
    public let description: String
    public init(description: String) { self.description = description }
}

public enum Git {
    /// Run git in `dir`; stdout on success, the trimmed stderr as the error otherwise.
    @discardableResult
    public static func run(_ dir: String, _ args: [String]) throws -> String {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        p.arguments = ["-C", dir] + args
        let out = Pipe(), err = Pipe()
        p.standardOutput = out
        p.standardError = err
        try p.run()
        let o = out.fileHandleForReading.readDataToEndOfFile()
        let e = err.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        guard p.terminationStatus == 0 else {
            let msg = String(decoding: e, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
            throw GitError(description: msg.isEmpty ? "git \(args.joined(separator: " ")) failed" : msg)
        }
        return String(decoding: o, as: UTF8.self)
    }

    /// `git worktree list --porcelain` → sessions; the bare entry (if any) is reported separately.
    public static func parseWorktrees(_ text: String) -> (sessions: [Session], bare: String?) {
        var sessions: [Session] = []
        var bare: String?
        for block in text.components(separatedBy: "\n\n") {
            var path: String?, branch: String?, isBare = false
            for line in block.split(separator: "\n") {
                if line.hasPrefix("worktree ") { path = String(line.dropFirst(9)) }
                else if line.hasPrefix("branch refs/heads/") { branch = String(line.dropFirst(18)) }
                else if line == "bare" { isBare = true }
            }
            guard let path else { continue }
            if isBare { bare = path } else { sessions.append(Session(path: path, branch: branch)) }
        }
        return (sessions, bare)
    }

    public static func listSessions(repo: String) throws -> [Session] {
        parseWorktrees(try run(repo, ["worktree", "list", "--porcelain"])).sessions
    }

    /// Where a new worktree goes: `<repo>-worktrees/<name>` next to the main checkout, or next to the bare dir's
    /// other worktrees. A `/` in the name becomes `-`.
    public static func worktreeDir(main: String, bare: String?, name: String) -> String {
        let flat = name.replacingOccurrences(of: "/", with: "-")
        if let bare {
            return ((bare as NSString).deletingLastPathComponent as NSString).appendingPathComponent(flat)
        }
        return main + "-worktrees/" + flat
    }

    /// New branch `name` from `base`, checked out in a new worktree; returns its path.
    public static func createSession(repo: String, name: String, base: String) throws -> String {
        let name = name.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty, !name.contains(" ") else { throw GitError(description: "a session name needs letters and no spaces") }
        let parsed = parseWorktrees(try run(repo, ["worktree", "list", "--porcelain"]))
        guard let main = parsed.sessions.first?.path ?? parsed.bare else { throw GitError(description: "no worktrees found") }
        let dir = worktreeDir(main: main, bare: parsed.bare, name: name)
        try run(repo, ["worktree", "add", "-b", name, dir, base])
        return dir
    }

    /// Remove a worktree (git refuses when it has changes; that message is the error).
    public static func removeSession(repo: String, path: String) throws {
        try run(repo, ["worktree", "remove", path])
    }

    /// The main checkout of `repo` (and the bare dir, for bare layouts).
    public static func mainWorktree(_ repo: String) throws -> (main: String, bare: String?) {
        let parsed = parseWorktrees(try run(repo, ["worktree", "list", "--porcelain"]))
        guard let main = parsed.sessions.first?.path ?? parsed.bare else { throw GitError(description: "no worktrees in \(repo)") }
        return (main, parsed.bare)
    }

    public static func head(_ dir: String) throws -> String {
        try run(dir, ["rev-parse", "HEAD"]).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    public static func isDirty(_ dir: String) -> Bool {
        ((try? run(dir, ["status", "--porcelain"])) ?? "").contains { !$0.isWhitespace }
    }

    /// Commits on HEAD that `base` doesn't have.
    public static func ahead(_ dir: String, of base: String) -> Int {
        Int(((try? run(dir, ["rev-list", "--count", "\(base)..HEAD"])) ?? "0").trimmingCharacters(in: .whitespacesAndNewlines)) ?? 0
    }

    public static func hasRemote(_ dir: String) -> Bool {
        ((try? run(dir, ["remote"])) ?? "").contains { !$0.isWhitespace }
    }

    /// Ignore `name` in every worktree of `dir`'s repository, without touching tracked files.
    public static func exclude(_ dir: String, _ name: String) throws {
        var path = try run(dir, ["rev-parse", "--git-path", "info/exclude"]).trimmingCharacters(in: .whitespacesAndNewlines)
        if !path.hasPrefix("/") { path = (dir as NSString).appendingPathComponent(path) }
        let line = "/" + name
        let old = (try? String(contentsOfFile: path, encoding: .utf8)) ?? ""
        guard !old.split(separator: "\n").contains(Substring(line)) else { return }
        try FileManager.default.createDirectory(atPath: (path as NSString).deletingLastPathComponent, withIntermediateDirectories: true)
        try (old + (old.isEmpty || old.hasSuffix("\n") ? "" : "\n") + line + "\n").write(toFile: path, atomically: true, encoding: .utf8)
    }
}

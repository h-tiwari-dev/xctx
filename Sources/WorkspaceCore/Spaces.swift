import Foundation

/// What a space is laid out for. `new` is a proposal nothing runs in yet.
public enum Shape: String, Codable, CaseIterable {
    case new, dev, debug, review
}

public struct SpaceRepo: Codable, Equatable {
    public var repo: String       // the repository's main checkout
    public var worktree: String?  // this space's worktree, once launched
    public var base: String?      // the commit the worktree started from
    public var name: String { (repo as NSString).lastPathComponent }

    public init(repo: String, worktree: String? = nil, base: String? = nil) {
        self.repo = repo
        self.worktree = worktree
        self.base = base
    }
}

/// A task with its repos: one worktree per repo on branch `space/<id>`, its own port and mise env.
public struct Space: Codable, Equatable, Identifiable {
    public var id: String // slug, also the mise env name
    public var name: String
    public var task: String
    public var shape: Shape
    public var repos: [SpaceRepo]
    public var port: Int
    public var created: Date
    public var branch: String { "space/\(id)" }
    public var launched: Bool { shape != .new }
    public var miseFile: String { "mise.\(id).toml" }
}

public enum GateState: String, Codable {
    case pending, approved, denied
}

/// Something an agent asked the user before going further.
public struct InboxItem: Codable, Equatable, Identifiable {
    public var id: String
    public var space: String
    public var title: String
    public var detail: String
    public var state: GateState
    public var created: Date
}

public struct StoreData: Codable, Equatable {
    public var spaces: [Space] = []
    public var inbox: [InboxItem] = []
    public var roots: [String] = []
    public init() {}
}

/// Spaces, the inbox and the indexed folders, saved as JSON in the app's data folder.
public final class SpaceStore {
    public let file: URL
    public var data: StoreData

    public init(dir: URL) {
        file = dir.appendingPathComponent("spaces.json")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        data = (try? Data(contentsOf: file)).flatMap { try? Self.decoder.decode(StoreData.self, from: $0) } ?? StoreData()
    }

    static let decoder: JSONDecoder = { let d = JSONDecoder(); d.dateDecodingStrategy = .iso8601; return d }()
    static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.dateEncodingStrategy = .iso8601
        e.outputFormatting = [.prettyPrinted, .sortedKeys]
        return e
    }()

    public func save() {
        if let d = try? Self.encoder.encode(data) { try? d.write(to: file, options: .atomic) }
    }

    public func space(_ id: String) -> Space? { data.spaces.first { $0.id == id } }

    public func update(_ s: Space) {
        if let i = data.spaces.firstIndex(where: { $0.id == s.id }) { data.spaces[i] = s } else { data.spaces.append(s) }
        save()
    }

    public func remove(_ id: String) {
        data.spaces.removeAll { $0.id == id }
        data.inbox.removeAll { $0.space == id }
        save()
    }

    /// A short readable id from the task, unique among the spaces.
    public func slug(for task: String) -> String {
        let base = RepoIndex.words(task).prefix(4).joined(separator: "-")
        let root = base.isEmpty ? "space" : String(base.prefix(40))
        var id = root, n = 2
        while space(id) != nil { id = "\(root)-\(n)"; n += 1 }
        return id
    }

    /// The lowest port from 4001 no other space holds.
    public func freePort() -> Int {
        let used = Set(data.spaces.map(\.port))
        return (4001...4999).first { !used.contains($0) } ?? 4999
    }

    /// A proposal: nothing is created until `launch`.
    public func propose(task: String, repos: [String]) -> Space {
        let id = slug(for: task)
        let words = task.split(separator: " ").prefix(4).joined(separator: " ")
        let s = Space(id: id, name: words.isEmpty ? id : words, task: task, shape: .new,
                      repos: repos.map { SpaceRepo(repo: $0) }, port: freePort(),
                      created: Date(timeIntervalSince1970: Date().timeIntervalSince1970.rounded(.down))) // whole seconds survive JSON
        update(s)
        return s
    }

    public func ask(space: String, title: String, detail: String) -> InboxItem {
        let item = InboxItem(id: UUID().uuidString.prefix(8).lowercased(), space: space, title: title, detail: detail,
                             state: .pending, created: Date())
        data.inbox.append(item)
        save()
        return item
    }

    public func answer(_ id: String, _ state: GateState) {
        if let i = data.inbox.firstIndex(where: { $0.id == id }) { data.inbox[i].state = state }
        save()
    }
}

public enum SpaceOps {
    /// Make the worktrees (branch `space/<id>` from each repo's HEAD) and each one's mise env file. All or none:
    /// if one repo fails, the worktrees already made are removed again.
    public static func launch(_ s: Space, shape: Shape = .dev) throws -> Space {
        var s = s
        var made: [(String, String)] = []
        do {
            for i in s.repos.indices {
                let repo = s.repos[i].repo
                let (main, bare) = try Git.mainWorktree(repo)
                let dir = Git.worktreeDir(main: main, bare: bare, name: s.id)
                let base = try Git.head(main)
                try Git.run(main, ["worktree", "add", "-b", s.branch, dir, base])
                made.append((main, dir))
                try writeMise(s, dir: dir)
                s.repos[i].worktree = dir
                s.repos[i].base = base
            }
        } catch {
            for (main, dir) in made.reversed() {
                try? FileManager.default.removeItem(atPath: (dir as NSString).appendingPathComponent(s.miseFile))
                _ = try? Git.run(main, ["worktree", "remove", "--force", dir])
                _ = try? Git.run(main, ["branch", "-D", s.branch])
            }
            throw error
        }
        s.shape = shape
        return s
    }

    /// `mise.<id>.toml`: picked up when MISE_ENV=<id>; ignored by git.
    static func writeMise(_ s: Space, dir: String) throws {
        let text = """
        # xctx space "\(s.name)". Used when MISE_ENV=\(s.id); mise.local.toml still wins.
        [env]
        PORT = "\(s.port)"
        XCTX_SPACE = "\(s.id)"

        """
        try text.write(toFile: (dir as NSString).appendingPathComponent(s.miseFile), atomically: true, encoding: .utf8)
        try Git.exclude(dir, s.miseFile)
    }

    /// The environment a space's terminals run with.
    public static func env(_ s: Space) -> [String: String] {
        ["PORT": String(s.port), "XCTX_SPACE": s.id, "MISE_ENV": s.id]
    }

    public struct ShipLine: Equatable {
        public var repo: String
        public var ahead: Int
        public var dirty: Bool
        public var remote: Bool
    }

    /// What shipping would do per repo.
    public static func plan(_ s: Space) -> [ShipLine] {
        s.repos.compactMap { r in
            guard let wt = r.worktree else { return nil }
            return ShipLine(repo: r.name, ahead: r.base.map { Git.ahead(wt, of: $0) } ?? 0, dirty: Git.isDirty(wt), remote: Git.hasRemote(wt))
        }
    }

    /// Push and open a PR for every repo with new commits (when it has a remote and `gh` exists), then dissolve.
    /// Refuses while any worktree has uncommitted changes. Returns one report line per repo.
    public static func ship(_ s: Space, gh: String? = "/opt/homebrew/bin/gh") throws -> [String] {
        let lines = plan(s)
        if let d = lines.first(where: \.dirty) {
            throw GitError(description: "\(d.repo) has uncommitted changes; commit or discard them first")
        }
        var report: [String] = []
        for (r, line) in zip(s.repos, lines) {
            guard let wt = r.worktree else { continue }
            if line.ahead == 0 { report.append("\(r.name): no new commits"); continue }
            guard line.remote else { report.append("\(r.name): \(line.ahead) commit(s) kept on \(s.branch) (no remote)"); continue }
            try Git.run(wt, ["push", "-u", "origin", s.branch])
            if let gh, FileManager.default.isExecutableFile(atPath: gh) {
                let pr = try run(gh, ["pr", "create", "--fill", "--head", s.branch], in: wt)
                report.append("\(r.name): \(pr.trimmingCharacters(in: .whitespacesAndNewlines))")
            } else {
                report.append("\(r.name): pushed \(s.branch)")
            }
        }
        try dissolve(s)
        return report
    }

    /// Remove the worktrees (branches are kept). Refuses, before removing anything, if one has changes.
    public static func dissolve(_ s: Space, force: Bool = false) throws {
        if !force, let d = s.repos.first(where: { $0.worktree.map(Git.isDirty) ?? false }) {
            throw GitError(description: "\(d.name) has uncommitted changes")
        }
        for r in s.repos {
            guard let wt = r.worktree, FileManager.default.fileExists(atPath: wt) else { continue }
            try? FileManager.default.removeItem(atPath: (wt as NSString).appendingPathComponent(s.miseFile))
            try Git.run(r.repo, ["worktree", "remove"] + (force ? ["--force"] : []) + [wt])
        }
    }

    static func run(_ exe: String, _ args: [String], in dir: String) throws -> String {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: exe)
        p.arguments = args
        p.currentDirectoryURL = URL(fileURLWithPath: dir)
        let out = Pipe()
        p.standardOutput = out
        p.standardError = out
        try p.run()
        let o = String(decoding: out.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        p.waitUntilExit()
        guard p.terminationStatus == 0 else { throw GitError(description: o) }
        return o
    }
}

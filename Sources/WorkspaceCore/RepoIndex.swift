import Foundation

public struct IndexedRepo: Codable, Equatable, Hashable {
    public var path: String
    public var name: String { (path as NSString).lastPathComponent }
    public var touched: Date? // last change to the git index: a cheap "recently used"

    public init(path: String, touched: Date? = nil) {
        self.path = path
        self.touched = touched
    }
}

public struct RepoMatch: Equatable {
    public var repo: IndexedRepo
    public var score: Int
    public var why: [String] // what matched: the name, the README, a few file paths

    public init(repo: IndexedRepo, score: Int, why: [String]) {
        self.repo = repo
        self.score = score
        self.why = why
    }
}

/// The git repositories under the folders the user chose to index, and a task → repos search over them.
public final class RepoIndex {
    public private(set) var roots: [String]
    public private(set) var repos: [IndexedRepo] = []
    private var fileCache: [String: [String]] = [:]
    private var readmeCache: [String: String] = [:]

    /// Folders that are never worth descending into.
    static let skip: Set<String> = ["node_modules", ".build", "target", "dist", "build", ".git", "Library", "vendor",
                                    ".venv", "venv", "Pods", "DerivedData", ".next", ".cache", ".Trash", "Applications"]
    static let stop: Set<String> = ["the", "and", "for", "with", "that", "this", "from", "into", "when", "make", "add",
                                    "fix", "our", "all", "are", "not", "can", "use", "new", "get", "set", "should", "need"]

    public init(roots: [String]) {
        self.roots = roots
    }

    /// Folders people usually keep code in, that exist here, with how many repos each holds.
    public static func candidates(home: String = NSHomeDirectory()) -> [(path: String, repos: Int)] {
        ["Documents", "Developer", "code", "Code", "projects", "Projects", "src", "dev", "work", "repos", "git", "Sites"]
            .map { (home as NSString).appendingPathComponent($0) }
            .filter { var d: ObjCBool = false; return FileManager.default.fileExists(atPath: $0, isDirectory: &d) && d.boolValue }
            .reduce(into: [(String, Int)]()) { out, p in
                if !out.contains(where: { $0.0 == p }) { out.append((p, find(under: p, depth: 3).count)) }
            }
            .filter { $0.1 > 0 }
    }

    public func setRoots(_ r: [String]) {
        roots = r
    }

    /// Find every repository under the roots. A folder with a `.git` is a repo and is not searched further;
    /// worktrees this app made (`*-worktrees/`) are left out.
    public func scan(depth: Int = 4) {
        var seen = Set<String>()
        repos = roots.flatMap { Self.find(under: $0, depth: depth) }
            .filter { !$0.contains("-worktrees/") && seen.insert($0).inserted }
            .map { p in
                let index = (p as NSString).appendingPathComponent(".git/index")
                let touched = (try? FileManager.default.attributesOfItem(atPath: index))?[.modificationDate] as? Date
                return IndexedRepo(path: p, touched: touched)
            }
            .sorted { ($0.touched ?? .distantPast) > ($1.touched ?? .distantPast) }
        fileCache = [:]
        readmeCache = [:]
    }

    static func find(under root: String, depth: Int) -> [String] {
        let fm = FileManager.default
        if fm.fileExists(atPath: (root as NSString).appendingPathComponent(".git")) { return [root] }
        guard depth > 0, let names = try? fm.contentsOfDirectory(atPath: root) else { return [] }
        return names.sorted().flatMap { n -> [String] in
            guard !n.hasPrefix("."), !skip.contains(n) else { return [] }
            let p = (root as NSString).appendingPathComponent(n)
            var d: ObjCBool = false
            guard fm.fileExists(atPath: p, isDirectory: &d), d.boolValue,
                  (try? fm.destinationOfSymbolicLink(atPath: p)) == nil else { return [] }
            return find(under: p, depth: depth - 1)
        }
    }

    static func words(_ s: String) -> [String] {
        s.lowercased().split { !$0.isLetter && !$0.isNumber }.map(String.init).filter { $0.count >= 3 && !stop.contains($0) }
    }

    private func files(_ repo: String) -> [String] {
        if let f = fileCache[repo] { return f }
        let out = (try? Git.run(repo, ["ls-files"])) ?? ""
        let f = out.split(separator: "\n").prefix(30000).map { String($0).lowercased() }
        fileCache[repo] = f
        return f
    }

    private func readme(_ repo: String) -> String {
        if let r = readmeCache[repo] { return r }
        let r = ["README.md", "readme.md", "README", "README.txt"].lazy
            .compactMap { FileHandle(forReadingAtPath: (repo as NSString).appendingPathComponent($0)) }
            .first.map { h -> String in defer { try? h.close() }; return String(decoding: h.readData(ofLength: 4096), as: UTF8.self).lowercased() } ?? ""
        readmeCache[repo] = r
        return r
    }

    /// Repos a task probably touches, best first. A word in the repo's name counts most, then the README, then
    /// file paths; ties go to the most recently used repo.
    public func search(_ task: String, limit: Int = 5) -> [RepoMatch] {
        let ws = Array(Set(Self.words(task)))
        guard !ws.isEmpty else { return [] }
        let matches = repos.compactMap { r -> RepoMatch? in
            var score = 0, why: [String] = []
            let name = Set(Self.words(r.name))
            let fs = files(r.path), rd = readme(r.path)
            for w in ws {
                if name.contains(where: { $0.hasPrefix(w) || w.hasPrefix($0) }) { score += 8; why.append("name: \(w)") }
                let stem = w.count > 5 ? String(w.prefix(5)) : w // limiting → limit, payments → payme
                if rd.contains(stem) { score += 2 }
                let hits = fs.filter { $0.contains(stem) }
                if !hits.isEmpty {
                    score += min(hits.count, 4)
                    why.append(contentsOf: hits.prefix(2))
                }
            }
            return score > 0 ? RepoMatch(repo: r, score: score, why: Array(why.prefix(4))) : nil
        }
        return Array(matches.sorted {
            $0.score != $1.score ? $0.score > $1.score : ($0.repo.touched ?? .distantPast) > ($1.repo.touched ?? .distantPast)
        }.prefix(limit))
    }
}

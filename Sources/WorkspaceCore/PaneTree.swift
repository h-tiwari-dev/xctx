import Foundation

/// A binary split tree of panes. Leaves are pane ids; the app keeps each pane's view keyed by that id.
public indirect enum PaneNode: Equatable {
    case leaf(Int)
    /// `sideBySide`: a | b (vertical divider); otherwise a above b. `ratio` is a's share.
    case split(id: Int, sideBySide: Bool, ratio: Double, a: PaneNode, b: PaneNode)
}

public struct PaneTree: Equatable {
    public private(set) var root: PaneNode
    public var focus: Int
    private var next: Int

    public init() {
        root = .leaf(1)
        focus = 1
        next = 2
    }

    /// A tree laid out up front (a space's shape); new ids continue after the highest one used.
    public init(root: PaneNode, focus: Int) {
        self.root = root
        self.focus = focus
        func ids(_ n: PaneNode) -> [Int] {
            switch n {
            case .leaf(let id): return [id]
            case .split(let id, _, _, let a, let b): return [id] + ids(a) + ids(b)
            }
        }
        next = (ids(root).max() ?? 0) + 1
    }

    /// Panes in a row (or column), equal sizes, split ids taken from `nextId`.
    public static func line(_ panes: [PaneNode], sideBySide: Bool, nextId: inout Int) -> PaneNode {
        guard panes.count > 1 else { return panes.first ?? .leaf(1) }
        let id = nextId
        nextId += 1
        let rest = line(Array(panes.dropFirst()), sideBySide: sideBySide, nextId: &nextId)
        return .split(id: id, sideBySide: sideBySide, ratio: 1 / Double(panes.count), a: panes[0], b: rest)
    }

    public var leaves: [Int] {
        func walk(_ n: PaneNode) -> [Int] {
            switch n {
            case .leaf(let id): return [id]
            case .split(_, _, _, let a, let b): return walk(a) + walk(b)
            }
        }
        return walk(root)
    }

    /// Split `pane`; the new pane goes right of (or below) it, is focused and returned.
    @discardableResult
    public mutating func split(_ pane: Int, sideBySide: Bool) -> Int? {
        guard leaves.contains(pane) else { return nil }
        let splitId = next, new = next + 1
        next += 2
        func walk(_ n: PaneNode) -> PaneNode {
            switch n {
            case .leaf(let id) where id == pane:
                return .split(id: splitId, sideBySide: sideBySide, ratio: 0.5, a: .leaf(id), b: .leaf(new))
            case .leaf: return n
            case .split(let id, let s, let r, let a, let b): return .split(id: id, sideBySide: s, ratio: r, a: walk(a), b: walk(b))
            }
        }
        root = walk(root)
        focus = new
        return new
    }

    /// Remove `pane`; its sibling takes the space and the focus moves there if it was on `pane`.
    /// False when it is the only pane.
    @discardableResult
    public mutating func close(_ pane: Int) -> Bool {
        guard leaves.count > 1, leaves.contains(pane) else { return false }
        var heir: Int?
        func first(_ n: PaneNode) -> Int {
            switch n {
            case .leaf(let id): return id
            case .split(_, _, _, let a, _): return first(a)
            }
        }
        func walk(_ n: PaneNode) -> PaneNode {
            switch n {
            case .leaf: return n
            case .split(let id, let s, let r, let a, let b):
                if a == .leaf(pane) { heir = first(b); return b }
                if b == .leaf(pane) { heir = first(a); return a }
                return .split(id: id, sideBySide: s, ratio: r, a: walk(a), b: walk(b))
            }
        }
        root = walk(root)
        if focus == pane, let heir { focus = heir }
        return true
    }

    public mutating func setRatio(split: Int, _ ratio: Double) {
        let r = min(max(ratio, 0.1), 0.9)
        func walk(_ n: PaneNode) -> PaneNode {
            switch n {
            case .leaf: return n
            case .split(let id, let s, let old, let a, let b):
                return .split(id: id, sideBySide: s, ratio: id == split ? r : old, a: walk(a), b: walk(b))
            }
        }
        root = walk(root)
    }

    /// Next (+1) / previous (-1) pane in reading order, wrapping.
    public mutating func cycleFocus(_ delta: Int) {
        let ids = leaves
        guard let i = ids.firstIndex(of: focus) else { focus = ids[0]; return }
        focus = ids[((i + delta) % ids.count + ids.count) % ids.count]
    }
}

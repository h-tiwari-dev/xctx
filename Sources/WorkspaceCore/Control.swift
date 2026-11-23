import Foundation

/// The harness's wire format: one JSON object per line over a unix socket, one JSON object back.
/// `{"cmd": "pane.mount", "space": "...", "kind": "browser", ...}` → `{"ok": true, ...}` or `{"ok": false, "error": "..."}`.
public enum Control {
    public static func socketPath(dataDir: URL) -> String {
        dataDir.appendingPathComponent("control.sock").path
    }

    /// `cmd key=value ...` (the CLI form) → a request object. Numbers stay strings; the window converts.
    public static func request(_ args: [String], defaultSpace: String?) -> [String: String] {
        var req: [String: String] = [:]
        if let c = args.first { req["cmd"] = c }
        for a in args.dropFirst() {
            if let eq = a.firstIndex(of: "=") { req[String(a[..<eq])] = String(a[a.index(after: eq)...]) }
        }
        if req["space"] == nil, let s = defaultSpace, !s.isEmpty { req["space"] = s }
        return req
    }

    public static func send(_ req: [String: Any], socket path: String, timeout: TimeInterval = 10) throws -> [String: Any] {
        let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw GitError(description: "socket: \(String(cString: strerror(errno)))") }
        defer { close(fd) }
        var tv = timeval(tv_sec: Int(timeout), tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        var addr = unixAddress(path)
        let ok = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard ok == 0 else { throw GitError(description: "xctx is not running (\(path))") }
        var data = try JSONSerialization.data(withJSONObject: req)
        data.append(10)
        _ = data.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
        var reply = Data()
        var buf = [UInt8](repeating: 0, count: 65536)
        while true {
            let n = read(fd, &buf, buf.count)
            if n <= 0 { break }
            reply.append(buf, count: n)
            if buf[n - 1] == 10 { break }
        }
        guard let obj = try JSONSerialization.jsonObject(with: reply) as? [String: Any] else {
            throw GitError(description: "bad reply from the window")
        }
        return obj
    }

    public static func unixAddress(_ path: String) -> sockaddr_un {
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let cap = MemoryLayout.size(ofValue: addr.sun_path)
        withUnsafeMutablePointer(to: &addr.sun_path) { p in
            p.withMemoryRebound(to: CChar.self, capacity: cap) { _ = strncpy($0, path, cap - 1) }
        }
        return addr
    }
}

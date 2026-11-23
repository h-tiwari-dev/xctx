import Foundation

/// What the browser's address field means: a URL, a bare host, or a search.
public enum Address {
    public static func url(for input: String) -> URL {
        let t = input.trimmingCharacters(in: .whitespacesAndNewlines)
        if let u = URL(string: t), let s = u.scheme?.lowercased(), ["http", "https", "file", "about", "data"].contains(s) {
            return u
        }
        let local = ["localhost", "127.0.0.1", "0.0.0.0"].contains { t == $0 || t.hasPrefix($0 + ":") || t.hasPrefix($0 + "/") }
        if local, let u = URL(string: "http://" + t) { return u }
        if !t.isEmpty, !t.contains(" "), t.contains("."), let u = URL(string: "https://" + t), u.host != nil { return u }
        var c = URLComponents(string: "https://duckduckgo.com/")!
        c.queryItems = [URLQueryItem(name: "q", value: t)]
        return c.url!
    }
}

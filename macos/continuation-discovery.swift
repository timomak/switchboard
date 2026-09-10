import Foundation
import SQLite3
import Darwin

/// Read-only discovery. Catalogs stay in memory; only the chosen transcript is read.
struct ContinuationLocalChat {
    enum Location { case session(URL), history(URL, String), unavailable }
    var chat: ContinuationChat
    var location: Location
    var workspace: URL? = nil
}

private final class ContinuationDatabase {
    private var db: OpaquePointer?
    init(_ url: URL) throws {
        let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
        guard attrs[.type] as? FileAttributeType == .typeRegular,
              sqlite3_open_v2(url.path, &db, SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX, nil) == SQLITE_OK else {
            if let db { sqlite3_close(db) }; db = nil
            throw ContinuationError.invalid
        }
        sqlite3_busy_timeout(db, 1000)
    }
    deinit { sqlite3_close(db) }
    func rows(_ sql: String, argument: String? = nil, maxBytes: Int = ContinuationLimits.input) throws -> [[String]] {
        var statement: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &statement, nil) == SQLITE_OK else { throw ContinuationError.unsupported }
        defer { sqlite3_finalize(statement) }
        if let argument {
            let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
            guard sqlite3_bind_text(statement, 1, argument, -1, transient) == SQLITE_OK else { throw ContinuationError.invalid }
        }
        var result: [[String]] = []; var bytes = 0
        while true {
            try Task.checkCancellation()
            let status = sqlite3_step(statement)
            if status == SQLITE_DONE { return result }
            guard status == SQLITE_ROW else { throw ContinuationError.invalid }
            var row: [String] = []
            for column in 0..<sqlite3_column_count(statement) {
                bytes += Int(sqlite3_column_bytes(statement, column))
                guard bytes <= maxBytes else { throw ContinuationError.tooLarge }
                row.append(sqlite3_column_text(statement, column).map { String(cString: $0) } ?? "")
            }
            result.append(row)
            guard result.count <= 20_000 else { throw ContinuationError.tooLarge }
        }
    }
}

enum ContinuationDiscovery {
    static func catalog(surface: ContinuationSurface, home: URL = FileManager.default.homeDirectoryForCurrentUser,
                        environment: [String: String] = ProcessInfo.processInfo.environment) throws -> [ContinuationLocalChat] {
        switch surface {
        case .codexDesktop, .codexCLI:
            let root = environment["CODEX_HOME"].flatMap { $0.isEmpty ? nil : URL(fileURLWithPath: $0) } ?? home.appendingPathComponent(".codex")
            return try codex(root: root, surface: surface)
        case .claudeCode:
            let root = environment["CLAUDE_CONFIG_DIR"].flatMap { $0.isEmpty ? nil : URL(fileURLWithPath: $0) } ?? home.appendingPathComponent(".claude")
            return try sessions(root: root.appendingPathComponent("projects"), surface: surface)
        case .claudeChat, .claudeDesktopCode: return [] // These surfaces have separate stores.
        }
    }
    private static func codex(root: URL, surface: ContinuationSurface) throws -> [ContinuationLocalChat] {
        let fm = FileManager.default
        guard fm.fileExists(atPath: root.path) else { return [] }
        let databases = try fm.contentsOfDirectory(at: root, includingPropertiesForKeys: nil)
            .filter { $0.lastPathComponent.hasPrefix("state_") && $0.pathExtension == "sqlite" }
            .sorted { $0.lastPathComponent.compare($1.lastPathComponent, options: .numeric) == .orderedDescending }
        guard let database = databases.first else {
            return try sessions(root: root.appendingPathComponent("sessions"), surface: surface)
        }
        let db = try ContinuationDatabase(database)
        let columns = try db.rows("PRAGMA table_info(threads)").map { $0[1] }
        let title = columns.contains("name") ? "COALESCE(NULLIF(name, ''), title)" : "title"
        let mode = columns.contains("history_mode") ? "history_mode" : "'legacy'"
        let cwd = columns.contains("cwd") ? "cwd" : "''"
        let origin = columns.contains("thread_source") ? "thread_source" : "''"
        let rows = try db.rows("SELECT id, \(title), updated_at, rollout_path, source, \(mode), \(cwd), \(origin) FROM threads ORDER BY updated_at DESC")
        let history = root.appendingPathComponent("thread_history_1.sqlite")
        return rows.compactMap { row in
            guard !row[4].contains("subagent"), row[4] != "exec" else { return nil }
            guard (surface == .codexCLI) == (row[4] == "cli" || row[7] == "switchboard-cli") else { return nil }
            let path = URL(fileURLWithPath: row[3], relativeTo: root).standardizedFileURL
            let location: ContinuationLocalChat.Location
            if row[5] == "paginated", fm.fileExists(atPath: history.path) { location = .history(history, row[0]) }
            else if fm.fileExists(atPath: path.path) { location = .session(path) }
            else { location = .unavailable }
            return .init(chat: .init(id: "local:\(surface.rawValue):\(row[0])", surface: surface,
                title: row[1].isEmpty ? "Untitled conversation" : String(row[1].prefix(120)), messages: [], omissions: [],
                importedAt: Date(timeIntervalSince1970: Double(row[2]) ?? 0)), location: location,
                workspace: row[6].hasPrefix("/") ? URL(fileURLWithPath: row[6]) : nil)
        }
    }
    private static func sessions(root: URL, surface: ContinuationSurface) throws -> [ContinuationLocalChat] {
        guard FileManager.default.fileExists(atPath: root.path) else { return [] }
        var failed = false
        guard let enumerator = FileManager.default.enumerator(at: root, includingPropertiesForKeys: [.isRegularFileKey, .isSymbolicLinkKey, .contentModificationDateKey], options: [.skipsHiddenFiles], errorHandler: { _, _ in failed = true; return false }) else { throw ContinuationError.invalid }
        var result: [ContinuationLocalChat] = []
        for case let url as URL in enumerator {
            try Task.checkCancellation()
            if url.lastPathComponent == "subagents" { enumerator.skipDescendants(); continue }
            guard url.pathExtension == "jsonl" else { continue }
            let values = try url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey, .contentModificationDateKey])
            guard values.isRegularFile == true, values.isSymbolicLink != true else { continue }
            let data = try prefix(url)
            var title: String?
            var workspace: URL?
            for line in data.split(separator: 10) {
                guard let row = try? JSONSerialization.jsonObject(with: Data(line)) as? [String: Any] else { continue }
                if let path = (row["cwd"] ?? (row["payload"] as? [String: Any])?["cwd"]) as? String, path.hasPrefix("/") {
                    workspace = URL(fileURLWithPath: path)
                }
                if row["isSidechain"] as? Bool == true { break }
                if row["type"] as? String == "user", row["isMeta"] as? Bool != true,
                   let message = row["message"] as? [String: Any] {
                    if let text = message["content"] as? String { title = text }
                    else if let blocks = message["content"] as? [[String: Any]] { title = blocks.first(where: { $0["type"] as? String == "text" })?["text"] as? String }
                }
                if title != nil { break }
            }
            // Codex fallback session names remain selectable even without an index.
            if title == nil && surface == .claudeCode { continue }
            let name = String((title ?? url.deletingPathExtension().lastPathComponent).components(separatedBy: .newlines).first?.prefix(120) ?? "Conversation")
            result.append(.init(chat: .init(id: "local:\(surface.rawValue):\(ContinuationFiles.digest(Data(url.path.utf8)))", surface: surface,
                title: name.isEmpty ? "Conversation" : name, messages: [], omissions: [], importedAt: values.contentModificationDate ?? .distantPast), location: .session(url), workspace: workspace))
            guard result.count <= 20_000 else { throw ContinuationError.tooLarge }
        }
        if failed { throw ContinuationError.invalid }
        return result
    }
    private static func prefix(_ url: URL) throws -> Data {
        let fd = open(url.path, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)
        guard fd >= 0 else { throw ContinuationError.invalid }; defer { close(fd) }
        var info = stat()
        guard fstat(fd, &info) == 0, (info.st_mode & S_IFMT) == S_IFREG else { throw ContinuationError.invalid }
        var buffer = [UInt8](repeating: 0, count: 256 * 1024)
        let count = Darwin.read(fd, &buffer, buffer.count)
        guard count >= 0 else { throw ContinuationError.invalid }
        return Data(buffer.prefix(count))
    }
    static func read(_ entry: ContinuationLocalChat) throws -> ContinuationChat {
        var chat: ContinuationChat
        switch entry.location {
        case .unavailable: throw ContinuationError.empty
        case .session(let url):
            let bytes = try ContinuationFiles.read(url, limit: ContinuationLimits.input)
            guard let parsed = try ContinuationParser.parse(bytes, extension: "jsonl", surface: entry.chat.surface, name: entry.chat.title).first else { throw ContinuationError.empty }
            chat = parsed
        case .history(let url, let id):
            let rows = try ContinuationDatabase(url).rows("SELECT item_json, created_at_ms FROM thread_items WHERE thread_id = ? AND item_type IN ('userMessage', 'agentMessage') ORDER BY rollout_ordinal, created_at_ms, item_id", argument: id, maxBytes: ContinuationLimits.input)
            var messages: [ContinuationMessage] = []; var omissions = ["Project instructions, tools and running work are not included."]
            for row in rows {
                guard let item = try JSONSerialization.jsonObject(with: Data(row[0].utf8)) as? [String: Any], let type = item["type"] as? String else { throw ContinuationError.invalid }
                let text: String
                if type == "agentMessage" {
                    guard let value = item["text"] as? String else { throw ContinuationError.invalid }; text = value
                } else {
                    guard let content = item["content"] as? [[String: Any]] else { throw ContinuationError.invalid }
                    text = try content.compactMap { block -> String? in
                        if block["type"] as? String == "text" {
                            guard let value = block["text"] as? String else { throw ContinuationError.invalid }
                            return value
                        }
                        omissions.append("Referenced attachments are not included. Add their original files separately."); return nil
                    }.joined(separator: "\n\n")
                }
                if !text.isEmpty { messages.append(.init(role: type == "userMessage" ? "User" : "Assistant", text: text,
                    timestamp: Double(row[1]).map { ISO8601DateFormatter().string(from: Date(timeIntervalSince1970: $0 / 1000)) })) }
            }
            chat = try .make(surface: entry.chat.surface, title: entry.chat.title, messages: messages, omissions: omissions)
        }
        chat.id = entry.chat.id; chat.title = entry.chat.title; chat.importedAt = entry.chat.importedAt
        return chat
    }
}

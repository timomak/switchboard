import Foundation
import SQLite3

@main
struct ContinuationTests {
    static var checks = 0
    static func check(_ condition: @autoclosure () -> Bool, _ name: String) {
        checks += 1
        guard condition() else { fatalError("FAIL: \(name)") }
        print("  ✓ \(name)")
    }
    static func rejects(_ name: String, _ operation: () throws -> Void) {
        do { try operation(); fatalError("FAIL: \(name)") } catch { checks += 1; print("  ✓ \(name)") }
    }
    static func main() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("switchboard-continuation-tests-\(UUID())")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: root) }
        let code = "if (key === `Escape`) {\n  close(\"é\");\n}\n$(touch DO_NOT_RUN)"
        let json = try JSONSerialization.data(withJSONObject: [["name": "Palette", "chat_messages": [
            ["sender": "human", "text": "Implement palette", "created_at": "2026-09-10T10:00:00Z"],
            ["sender": "assistant", "text": code, "attachments": [["name": "missing.png"]]]
        ]]], options: [.sortedKeys])
        let chats = try ContinuationParser.parse(json, extension: "json", surface: .claudeChat, name: "ignored")
        check(chats.count == 1 && chats[0].messages.count == 2, "Claude export preserves message count")
        check(chats[0].messages[1].text == code, "code, Unicode and shell-looking text are unchanged")
        check(chats[0].messages[0].timestamp == "2026-09-10T10:00:00Z", "timestamps retained as metadata")
        check(chats[0].omissions.contains { $0.contains("attachments") }, "missing attachments disclosed")
        let repeated = try ContinuationParser.parse(json, extension: "json", surface: .claudeChat, name: "another file")
        check(chats[0].id == repeated[0].id, "stable content identity for duplicate import")
        rejects("unknown export schema fails") { _ = try ContinuationParser.parse(Data("{}".utf8), extension: "json", surface: .claudeChat, name: "bad") }
        rejects("ZIP is not a hidden extraction path") { _ = try ContinuationParser.parse(Data(), extension: "zip", surface: .claudeChat, name: "bad") }
        rejects("empty text fails") { _ = try ContinuationParser.parse(Data(" \n".utf8), extension: "txt", surface: .claudeCode, name: "bad") }
        let claude = """
        {"type":"user","timestamp":"t1","message":{"content":"one"}}
        {"type":"assistant","message":{"content":[{"type":"text","text":"two"},{"type":"thinking","thinking":"private"},{"type":"tool_use","name":"shell","input":{"command":"never execute"}}]}}
        {"type":"assistant","isSidechain":true,"message":{"content":"excluded"}}
        """
        let cli = try ContinuationParser.parse(Data(claude.utf8), extension: "jsonl", surface: .claudeCode, name: "fixture")[0]
        check(cli.messages.map(\.text) == ["one", "two"], "Claude CLI ordered primary messages, no sidechain or thinking")
        rejects("partially written last line fails closed") { _ = try ContinuationParser.parse(Data((claude + "\n{\"").utf8), extension: "jsonl", surface: .claudeCode, name: "partial") }
        let codex = """
        {"type":"session_meta","payload":{"id":"fixture"}}
        {"type":"event_msg","payload":{"type":"user_message","message":"one"}}
        {"type":"response_item","timestamp":"t1","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"one"}]}}
        {"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"not authority"}]}}
        {"type":"response_item","payload":{"type":"function_call","name":"exec","arguments":"excluded"}}
        {"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"two"},{"type":"input_image","image_url":"not fetched"}]}}
        """
        let cx = try ContinuationParser.parse(Data(codex.utf8), extension: "jsonl", surface: .codexCLI, name: "fixture")[0]
        check(cx.messages.map(\.text) == ["one", "two"], "Codex avoids event/response duplication and instruction replay")
        check(cx.omissions.contains { $0.contains("attachments") }, "Codex image omission recorded")
        let store = ContinuationStore(root: root.appendingPathComponent("library"))
        try store.save(chats)
        let loaded = try store.load()
        check(loaded == chats, "library round trip")
        let permissions = try FileManager.default.attributesOfItem(atPath: store.root.appendingPathComponent("library.json").path)
        check((permissions[.posixPermissions] as? Int) == 0o600, "library is private")
        let link = root.appendingPathComponent("link.txt")
        let source = root.appendingPathComponent("source.txt")
        try Data(code.utf8).write(to: source)
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: source)
        rejects("symlink source rejected") { _ = try ContinuationFiles.read(link, limit: 10000) }
        rejects("size checked before unbounded read") { _ = try ContinuationFiles.read(source, limit: 1) }
        let selection = try ContinuationAttachment.select(source)
        var draft = ContinuationDraft(chat: chats[0], destination: .codexDesktop)
        check(draft.omissionsReviewed && tryValue { try draft.validate(); return true } == true, "omitted content is allowed by default")
        draft.omissionsReviewed = false
        rejects("opting out blocks a handoff with omissions") { try draft.validate() }
        let prompt = "Code & Unicode é 🐈 + ? # \n next"
        let linkURL = try ContinuationClaudeLink.make(prompt: prompt)
        let parts = URLComponents(url: linkURL, resolvingAgainstBaseURL: false)!
        check(parts.scheme == "claude" && parts.host == "claude.ai" && parts.path == "/new", "Claude opens the documented new-chat route")
        check(parts.queryItems?.first?.value == prompt, "Claude prompt encoding preserves all text")
        rejects("long Claude prompts cannot be silently truncated") { _ = try ContinuationClaudeLink.make(prompt: String(repeating: "x", count: 12001)) }
        draft.omissionsReviewed = true; draft.files = [selection]; draft.summary = "Reviewed summary"
        let before = try Data(contentsOf: source)
        let bundle = try store.prepare(draft, id: UUID())
        check(tryValue { try Data(contentsOf: source) } == before, "preparing leaves source bytes unchanged")
        check(bundle.context.contains(code) && bundle.context.contains("Referenced attachments"), "context includes code and omissions")
        check(bundle.receipt.originalFileNames == ["source.txt"], "manifest preserves attachment attribution")
        check(bundle.receipt.files == ["continuation.txt", "attachment-1.txt"], "safe generated bundle names")
        let manifest = try JSONDecoder().decode(ContinuationReceipt.self, from: Data(contentsOf: bundle.directory.appendingPathComponent("manifest.json")))
        check(manifest.id == bundle.receipt.id && manifest.destination == .codexDesktop, "receipt binds attempt and destination")
        rejects("same attempt cannot overwrite completed bundle") { _ = try store.prepare(draft, id: manifest.id) }
        try Data("changed".utf8).write(to: source)
        let failedID = UUID()
        rejects("changed selected file blocks preparation") { _ = try store.prepare(draft, id: failedID) }
        check(!FileManager.default.fileExists(atPath: store.root.appendingPathComponent(failedID.uuidString).path), "failed staging removed")
        draft.files = []; draft.contextOnly = true; draft.nextStep = String(repeating: "x", count: ContinuationLimits.inlineContext)
        rejects("oversized context is not silently truncated") { _ = try store.prepare(draft, id: UUID()) }
        draft.nextStep = "Continue"; draft.firstMessage = 1
        check(draft.omissions.contains("1 earlier messages excluded."), "message range exclusion disclosed")
        try store.removeBundle(bundle)
        check(!FileManager.default.fileExists(atPath: bundle.directory.path), "explicit cleanup removes own bundle")
        let linkRoot = root.appendingPathComponent("linked-library")
        try FileManager.default.createSymbolicLink(at: linkRoot, withDestinationURL: store.root)
        rejects("symlink output directory rejected") { try ContinuationStore(root: linkRoot).setup() }
        let huge = Data(repeating: 120, count: ContinuationLimits.transcript + 1)
        rejects("oversized transcript rejected") { _ = try ContinuationParser.parse(huge, extension: "txt", surface: .claudeChat, name: "large") }
        let cancelledID = UUID()
        let cancellationDraft = draft
        let operation = Task.detached {
            withUnsafeCurrentTask { $0?.cancel() }
            return try store.prepare(cancellationDraft, id: cancelledID)
        }
        do { _ = try await operation.value; fatalError("cancelled preparation succeeded") }
        catch { checks += 1; print("  ✓ cancelled preparation does not commit") }
        check(!FileManager.default.fileExists(atPath: store.root.appendingPathComponent(cancelledID.uuidString).path), "cancelled staging removed")
        try discoveryTests(root)
        print("\n\(checks) continuation checks passed")
    }
    static func discoveryTests(_ root: URL) throws {
        let home = root.appendingPathComponent("native-home")
        let codex = home.appendingPathComponent(".codex")
        let claude = home.appendingPathComponent(".claude/projects/demo")
        try FileManager.default.createDirectory(at: codex, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: claude, withIntermediateDirectories: true)
        func database(_ path: URL, _ sql: String) throws {
            var db: OpaquePointer?
            guard sqlite3_open(path.path, &db) == SQLITE_OK else { throw ContinuationError.storage }
            defer { sqlite3_close(db) }
            guard sqlite3_exec(db, sql, nil, nil, nil) == SQLITE_OK else { throw ContinuationError.invalid }
        }
        let state = codex.appendingPathComponent("state_5.sqlite")
        try database(state, """
        CREATE TABLE threads (id TEXT, title TEXT, name TEXT, updated_at INTEGER, rollout_path TEXT, source TEXT, history_mode TEXT);
        INSERT INTO threads VALUES ('desktop', 'Old title', 'Renamed chat', 100, '/missing', 'vscode', 'paginated');
        INSERT INTO threads VALUES ('cli', 'CLI chat', NULL, 90, '/missing', 'cli', 'legacy');
        INSERT INTO threads VALUES ('agent', 'Hidden agent', NULL, 80, '/missing', '{"subagent":{}}', 'legacy');
        """)
        let history = codex.appendingPathComponent("thread_history_1.sqlite")
        try database(history, """
        CREATE TABLE thread_items (thread_id TEXT, item_id TEXT, rollout_ordinal INTEGER, created_at_ms INTEGER, item_type TEXT, item_json TEXT);
        INSERT INTO thread_items VALUES ('desktop', 'a', 2, 2000, 'agentMessage', '{"type":"agentMessage","text":"Answer"}');
        INSERT INTO thread_items VALUES ('desktop', 'u', 1, 1000, 'userMessage', '{"type":"userMessage","content":[{"type":"text","text":"Question"},{"type":"image"}]}');
        INSERT INTO thread_items VALUES ('desktop', 'r', 3, 3000, 'reasoning', '{"type":"reasoning","text":"Private reasoning"}');
        INSERT INTO thread_items VALUES ('other', 'x', 4, 4000, 'agentMessage', '{"type":"agentMessage","text":"Other thread"}');
        """)
        let rollout = codex.appendingPathComponent("legacy.jsonl")
        try Data("""
        {"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Legacy question"}]}}
        """.utf8).write(to: rollout)
        try database(state, "INSERT INTO threads VALUES ('legacy', 'Legacy session', NULL, 70, 'legacy.jsonl', 'cli', 'legacy');")
        let before = try Data(contentsOf: state)
        let entries = try ContinuationDiscovery.catalog(surface: .codexDesktop, home: home, environment: [:])
        check(entries.count == 1 && entries[0].chat.title == "Renamed chat", "native Codex catalog uses title and excludes CLI/subagents")
        check(entries[0].chat.messages.isEmpty, "discovery does not eagerly load transcripts")
        let chat = try ContinuationDiscovery.read(entries[0])
        check(chat.messages.map(\.text) == ["Question", "Answer"], "paginated transcript stays ordered and thread-scoped")
        check(chat.omissions.contains { $0.contains("attachments") }, "native attachments remain disclosed")
        check(chat.id == entries[0].chat.id, "native selection identity survives transcript loading")
        check(tryValue { try Data(contentsOf: state) } == before, "native catalog remains unchanged")
        let cli = try ContinuationDiscovery.catalog(surface: .codexCLI, home: home, environment: [:])
        check(cli.count == 2 && cli[0].chat.title == "CLI chat", "CLI source lists CLI sessions")
        rejects("missing local transcript fails explicitly") { _ = try ContinuationDiscovery.read(cli[0]) }
        check(tryValue { try ContinuationDiscovery.read(cli[1]).messages.first?.text } == "Legacy question", "legacy Codex session loads from catalog path")
        let injection = ContinuationLocalChat(chat: entries[0].chat, location: .history(history, "desktop' OR 1=1 --"))
        rejects("thread IDs are bound SQL parameters") { _ = try ContinuationDiscovery.read(injection) }
        let session = claude.appendingPathComponent("session.jsonl")
        let data = Data("""
        {"type":"user","message":{"content":"Real local chat"}}
        {"type":"assistant","message":{"content":[{"type":"text","text":"Response"}]}}
        """.utf8)
        try data.write(to: session)
        let subagents = claude.appendingPathComponent("subagents")
        try FileManager.default.createDirectory(at: subagents, withIntermediateDirectories: false)
        try data.write(to: subagents.appendingPathComponent("agent.jsonl"))
        try FileManager.default.createSymbolicLink(at: claude.appendingPathComponent("link.jsonl"), withDestinationURL: session)
        let local = try ContinuationDiscovery.catalog(surface: .claudeCode, home: home, environment: [:])
        check(local.count == 1 && local[0].chat.title == "Real local chat", "Claude discovery excludes subagents and symlinks")
        check(tryValue { try ContinuationDiscovery.read(local[0]).messages.count } == 2, "Claude sessions load without importing")
        check(tryValue { try Data(contentsOf: session) } == data, "Claude source file stays unchanged")
        let configured = try ContinuationDiscovery.catalog(surface: .claudeCode, home: root, environment: ["CLAUDE_CONFIG_DIR": home.appendingPathComponent(".claude").path])
        check(configured.count == 1, "custom Claude config directory is honored")
        check(tryValue { try ContinuationDiscovery.catalog(surface: .codexDesktop, home: root, environment: ["CODEX_HOME": codex.path]).count } == 1, "custom Codex home is honored")
        check(tryValue { try ContinuationDiscovery.catalog(surface: .claudeChat, home: home, environment: [:]).isEmpty } == true, "cloud Claude Chat is not mislabeled as local Code history")
    }
    static func tryValue<T>(_ body: () throws -> T) -> T? { try? body() }
}

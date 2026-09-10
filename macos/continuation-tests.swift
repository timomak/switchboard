import Foundation

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
        rejects("attachment omission requires explicit decision") { try draft.validate() }
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
        draft.files = []; draft.nextStep = String(repeating: "x", count: ContinuationLimits.inlineContext)
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
        print("\n\(checks) continuation checks passed")
    }
    static func tryValue<T>(_ body: () throws -> T) -> T? { try? body() }
}

import Foundation
import Darwin

@main
struct ContinuationNativeTests {
    static var checks = 0
    static func check(_ condition: Bool, _ name: String) {
        guard condition else { fatalError("FAIL: " + name) }; checks += 1; print("  ✓ " + name)
    }
    static func main() async throws {
        let keep = ProcessInfo.processInfo.environment["SWITCHBOARD_NATIVE_FIXTURE_DIR"]
        let root = keep.map { URL(fileURLWithPath: $0) } ?? FileManager.default.temporaryDirectory.appendingPathComponent("switchboard-native-\(UUID())")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { if keep == nil { try? FileManager.default.removeItem(at: root) } }
        try ContinuationDesktopHandoff.run(script: "#!/bin/bash\nset -e\n[[ -t 0 && -t 1 && -t 2 ]]\n[[ $(/bin/stty size) == '40 120' ]]\n/usr/bin/yes synthetic | /usr/bin/head -c 300000\n", directory: root)
        check(true, "hidden handoff provides a real terminal and drains large output")
        do { try ContinuationDesktopHandoff.run(script: "exit 37\n", directory: root); fatalError("Must propagate CLI failure") }
        catch ContinuationHandoffError.failed { check(true, "nonzero handoff status becomes an actionable error") }
        let childFile = root.appendingPathComponent("child.pid")
        do {
            try ContinuationDesktopHandoff.run(script: "echo $$ > \(ContinuationNative.quote(childFile.path))\nexec /bin/sleep 30\n", directory: root, timeout: 0.25)
            fatalError("Must time out")
        } catch ContinuationHandoffError.timedOut { check(true, "interactive stalls time out without opening Terminal") }
        let pid = Int32(try String(contentsOf: childFile, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines))!
        for _ in 0..<50 where kill(pid, 0) == 0 { try await Task.sleep(nanoseconds: 10_000_000) }
        check(kill(pid, 0) != 0, "timeout hangs up and reaps the owned CLI child")
        let cancelled = Task.detached { try ContinuationDesktopHandoff.run(script: "exec /bin/sleep 30\n", directory: root) }
        try await Task.sleep(nanoseconds: 100_000_000); cancelled.cancel()
        do { try await cancelled.value; fatalError("Must cancel") }
        catch is CancellationError { check(true, "cancellation shuts down the hidden handoff") }
        let remnants = try FileManager.default.contentsOfDirectory(atPath: root.path).filter { $0.hasPrefix(".open-desktop-") }
        check(remnants.isEmpty, "success, failure and cancellation remove temporary launch scripts")
        do {
            try ContinuationDesktopHandoff.run(script: "echo 'Do you trust this folder?'\nsleep 5\n", directory: root)
            fatalError("Trust prompt must surface")
        } catch ContinuationHandoffError.trustRequired { check(true, "hidden trust prompt surfaces without waiting for timeout") }
        check(ContinuationDesktopHandoff.promptError("Please log in") != nil, "sign-in prompt is actionable")
        let project = root.appendingPathComponent("project space ' é"); try ContinuationFiles.directory(project)
        let source = root.appendingPathComponent("source.json")
        let messages = (0..<40).map { ContinuationMessage(role: $0 % 2 == 0 ? "User" : "Assistant",
            text: "Message \($0): é 🐈 `code` $(do-not-execute)\n" + String(repeating: "history ", count: 200), timestamp: "2026-09-10T09:00:00Z") }
        let chat = try ContinuationChat.make(surface: .claudeChat, title: "Synthetic clone", messages: messages)
        let sourceBytes = try JSONEncoder().encode(chat); try ContinuationFiles.write(sourceBytes, to: source)
        let store = ContinuationStore(root: root.appendingPathComponent("bundles"))
        var draft = ContinuationDraft(chat: chat, destination: .claudeCode); draft.workspace = project
        try draft.validate()
        check(draft.context.utf8.count > ContinuationLimits.inlineContext, "native history exceeds the old paste limit")
        let bundle = try store.prepare(draft, id: UUID())
        let claudeRoot = root.appendingPathComponent("claude")
        let result = try ContinuationNative.create(draft, bundle: bundle, rootOverride: claudeRoot, binaryOverride: URL(fileURLWithPath: "/usr/bin/true"))
        check(result.verified && result.messageCount == 40, "Claude transcript preserves 40 separate turns")
        let retry = try ContinuationNative.create(draft, bundle: bundle, rootOverride: claudeRoot)
        check(retry.id == result.id, "retry verifies the existing destination without duplicating it")
        let data = try ContinuationFiles.read(result.transcript!, limit: ContinuationLimits.input)
        let rows = try data.split(separator: 10).map { try JSONSerialization.jsonObject(with: Data($0)) as! [String: Any] }
        var previous: String?
        var linked = true
        for row in rows where row["type"] as? String != "custom-title" {
            linked = linked && (row["parentUuid"] as? String) == previous
            previous = row["uuid"] as? String
        }
        check(linked, "Claude parent chain is continuous across the full history")
        let secondBundle = try store.prepare(draft, id: UUID())
        let second = try ContinuationNative.create(draft, bundle: secondBundle, rootOverride: claudeRoot, binaryOverride: URL(fileURLWithPath: "/usr/bin/true"))
        check(second.id != result.id, "a new clone request gets an independent session ID")
        var desktopResult = result; desktopResult.destination = .claudeDesktopCode
        let script = try ContinuationNative.terminalScript(desktopResult, backend: nil, binaryOverride: URL(fileURLWithPath: "/usr/bin/true"))
        check(script.contains(ContinuationNative.quote(result.id)) && script.contains("'/desktop'") && !script.contains(messages[0].text), "Desktop Code handoff targets the clone ID, never sends transcript as a prompt")
        let shellFile = root.appendingPathComponent("open.command")
        try ContinuationFiles.write(Data(script.utf8), to: shellFile)
        let shell = Process(); shell.executableURL = URL(fileURLWithPath: "/bin/bash"); shell.arguments = ["-n", shellFile.path]
        try shell.run(); shell.waitUntilExit()
        check(shell.terminationStatus == 0, "opening script safely quotes Unicode and apostrophes")
        do { try ContinuationNative.publish(Data("replacement".utf8), to: result.transcript!); fatalError("Must not overwrite a session") }
        catch { check(try Data(contentsOf: result.transcript!) == data, "atomic publication never replaces an existing destination") }
        let parsed = try ContinuationParser.parse(data, extension: "jsonl", surface: .claudeCode, name: "ignored")[0]
        check(parsed.messages == messages && parsed.title == chat.title, "roles, text, title, timestamps round-trip")
        check(try Data(contentsOf: source) == sourceBytes, "source snapshot remains byte-for-byte unchanged")
        try ContinuationFiles.write(try JSONEncoder().encode(result), to: root.appendingPathComponent("claude-result.json"))
        try ContinuationFiles.write(try JSONEncoder().encode(messages), to: root.appendingPathComponent("expected.json"))
        draft.destination = .claudeChat
        do { _ = try ContinuationNative.create(draft, bundle: bundle); fatalError("Chat must not be a fake clone") }
        catch ContinuationNativeError.chatUnsupported { check(true, "ordinary Chat never silently falls back to a prompt") }
        draft.destination = .codexDesktop
        if let binary = ContinuationNative.executable("codex") {
            let codexRoot = root.appendingPathComponent("codex"); try ContinuationFiles.directory(codexRoot)
            let cxBundle = try store.prepare(draft, id: UUID())
            let cx = try ContinuationNative.create(draft, bundle: cxBundle, rootOverride: codexRoot, binaryOverride: binary)
            check(cx.verified && cx.messageCount == 40, "real Codex app-server creates and reloads native history")
            check(try ContinuationNative.codexLink(cx).absoluteString == "codex://threads/" + cx.id, "Codex opens the exact created session")
            let again = try ContinuationNative.create(draft, bundle: cxBundle, rootOverride: codexRoot, binaryOverride: binary)
            check(again.id == cx.id, "Codex retry reuses the persisted receipt")
            try ContinuationNative.verify(cx, expected: messages, binaryOverride: binary)
            check(true, "history survives server shutdown and independent reopen")
            let client = try ContinuationRPC(binary: binary, home: codexRoot, cwd: project); defer { client.close() }
            let view = try client.request("thread/read", ["threadId": cx.id, "includeTurns": true])
            let thread = view["thread"] as! [String: Any]
            check(thread["name"] as? String == chat.title, "native destination keeps the selected title")
            let turns = thread["turns"] as! [[String: Any]]
            check(turns.allSatisfy { $0["status"] as? String == "completed" }, "imported history has no unfinished execution")
            draft.destination = .codexCLI
            let cliBundle = try store.prepare(draft, id: UUID())
            let cliResult = try ContinuationNative.create(draft, bundle: cliBundle, rootOverride: codexRoot, binaryOverride: binary)
            check(cliResult.id != cx.id && cliResult.verified, "CLI destination creates an independent native conversation")
            let catalog = try ContinuationDiscovery.catalog(surface: .codexCLI, home: root, environment: ["CODEX_HOME": codexRoot.path])
            check(catalog.contains { $0.chat.id.hasSuffix(cliResult.id) }, "CLI clone remains discoverable under the selected CLI source")
            let pendingDraft = try store.prepare(draft, id: UUID())
            try ContinuationFiles.write(Data(), to: pendingDraft.directory.appendingPathComponent("creation-pending"))
            do { _ = try ContinuationNative.create(draft, bundle: pendingDraft, rootOverride: codexRoot, binaryOverride: binary); fatalError("Must not repeat an uncertain fork") }
            catch ContinuationNativeError.uncertain { check(true, "uncertain creation never automatically duplicates a session") }
            try ContinuationFiles.write(try JSONEncoder().encode(cx), to: root.appendingPathComponent("codex-result.json"))
        } else { print("  SKIP: Codex CLI is not installed; native integration check unavailable") }
        print("✓ \(checks) native clone checks passed. No model turns or live stores used.")
    }
}

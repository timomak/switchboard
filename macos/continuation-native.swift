import Foundation
import Darwin

enum ContinuationNativeError: LocalizedError {
    case chatUnsupported, missingCLI(String), protocolFailure, verification, uncertain, workspace
    var errorDescription: String? {
        switch self {
        case .chatUnsupported: return "Claude Chat cannot import message history. Choose Claude Code, or enable context handoff under Advanced."
        case .missingCLI(let name): return "Install or update \(name) to clone this conversation."
        case .protocolFailure: return "The destination could not create this conversation. Check that its CLI is up to date."
        case .verification: return "The destination history could not be verified. Retry verification."
        case .uncertain: return "Creation was interrupted. Check destination history before starting another copy. Local details are under Advanced."
        case .workspace: return "Choose an existing project folder under Advanced."
        }
    }
}

enum ContinuationHandoffError: LocalizedError {
    case failed, timedOut, trustRequired, signInRequired
    var errorDescription: String? {
        switch self {
        case .trustRequired: return "Claude needs folder trust confirmation. Use Open in Terminal under Advanced, then Open this saved chat."
        case .signInRequired: return "Claude needs sign-in. Use Open in Terminal under Advanced, then Open this saved chat."
        case .failed: return "Claude could not finish opening. Use Open in Terminal under Advanced to resolve sign-in or folder access."
        case .timedOut: return "Claude needs attention. Use Open in Terminal under Advanced to finish opening this conversation."
        }
    }
}

/// macOS script allocates a controlling PTY without starting Terminal.app.
/// Transcript output is drained, never displayed, logged, or retained. The
/// temporary script contains launch arguments only, never conversation text.
enum ContinuationDesktopHandoff {
    static func promptError(_ text: String) -> ContinuationHandoffError? {
        let plain = text.replacingOccurrences(of: "\u{1B}\\[[0-9;?]*[A-Za-z]", with: "", options: .regularExpression).lowercased()
        if plain.contains("do you trust") || plain.contains("trust this folder") || plain.contains("trust the files") || plain.contains("yes, i trust") { return .trustRequired }
        if plain.contains("please log in") || plain.contains("please sign in") || plain.contains("not logged in") || plain.contains("login required") { return .signInRequired }
        return nil
    }
    static func run(script: String, directory: URL, timeout: TimeInterval = 45) throws {
        try Task.checkCancellation()
        let file = directory.appendingPathComponent(".open-desktop-\(UUID()).sh")
        // A PTY started from a GUI inherits a 0×0 window. Give terminal UI
        // libraries usable dimensions before Claude starts rendering.
        let sizedScript = "/bin/stty cols 120 rows 40 || exit 125\n" + script
        try ContinuationFiles.write(Data(sizedScript.utf8), to: file)
        defer { try? FileManager.default.removeItem(at: file) }
        let process = Process(), input = Pipe(), output = Pipe()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/script")
        process.arguments = ["-q", "-e", "/dev/null", "/bin/bash", file.path]
        var environment = ProcessInfo.processInfo.environment
        environment["TERM"] = "xterm-256color"
        process.environment = environment
        process.currentDirectoryURL = directory
        process.standardInput = input; process.standardOutput = output; process.standardError = output
        try process.run()
        defer {
            // Closing script's PTY master hangs up its child session. Do not
            // touch Terminal.app or any process not owned by this invocation.
            if process.isRunning {
                process.terminate()
                let deadline = Date().addingTimeInterval(2)
                while process.isRunning && Date() < deadline { Thread.sleep(forTimeInterval: 0.01) }
                if process.isRunning { kill(process.processIdentifier, SIGKILL) }
            }
            try? input.fileHandleForWriting.close()
            try? output.fileHandleForReading.close()
        }
        let deadline = Date().addingTimeInterval(timeout)
        var diagnosticTail = Data()
        while process.isRunning {
            try Task.checkCancellation()
            guard Date() < deadline else { throw ContinuationHandoffError.timedOut }
            var fd = pollfd(fd: output.fileHandleForReading.fileDescriptor, events: Int16(POLLIN), revents: 0)
            if poll(&fd, 1, 100) > 0 {
                var bytes = [UInt8](repeating: 0, count: 16_384)
                let count = Darwin.read(fd.fd, &bytes, bytes.count)
                if count > 0 {
                    diagnosticTail.append(contentsOf: bytes.prefix(count))
                    if diagnosticTail.count > 16_384 { diagnosticTail.removeFirst(diagnosticTail.count - 16_384) }
                    // Classify in memory only; never persist terminal output.
                    if let error = promptError(String(decoding: diagnosticTail, as: UTF8.self)) { throw error }
                }
            }
        }
        guard process.terminationReason == .exit, process.terminationStatus == 0 else { throw ContinuationHandoffError.failed }
    }
}

struct ContinuationNativeResult: Codable {
    var id: String
    var destination: ContinuationSurface
    var workspace: URL
    var storageRoot: URL
    var transcript: URL?
    var messageCount: Int
    var verified: Bool
    var method: String
}

/// A single-purpose, bounded JSON-lines client. Never sends a model turn,
/// executes transcript content, or services server tool/approval requests.
final class ContinuationRPC {
    private let process = Process()
    private let input = Pipe()
    private let output = Pipe()
    private var buffer = Data()
    private var sequence = 0
    private var closed = false

    init(binary: URL, home: URL, cwd: URL) throws {
        process.executableURL = binary
        process.arguments = ["app-server"]
        var environment = ProcessInfo.processInfo.environment
        environment["CODEX_HOME"] = home.path
        environment.removeValue(forKey: "CODEX_SQLITE_HOME")
        process.environment = environment
        process.currentDirectoryURL = cwd
        process.standardInput = input; process.standardOutput = output
        // Provider diagnostics may contain local paths. Never copy them into UI/logs.
        process.standardError = FileHandle.nullDevice
        try process.run()
        _ = fcntl(input.fileHandleForWriting.fileDescriptor, F_SETNOSIGPIPE, 1)
        do {
            let info = try request("initialize", ["clientInfo": ["name": "switchboard", "version": "1.12.0"],
                                                  "capabilities": ["experimentalApi": true]])
            guard let reported = info["codexHome"] as? String,
                  URL(fileURLWithPath: reported).resolvingSymlinksInPath().path == home.resolvingSymlinksInPath().path else {
                throw ContinuationNativeError.protocolFailure
            }
            try send(["method": "initialized"])
        } catch { close(); throw error }
    }
    deinit { close() }
    func close() {
        guard !closed else { return }; closed = true
        try? input.fileHandleForWriting.close()
        if process.isRunning {
            process.terminate()
            let until = Date().addingTimeInterval(2)
            while process.isRunning && Date() < until { Thread.sleep(forTimeInterval: 0.01) }
            if process.isRunning { kill(process.processIdentifier, SIGKILL) }
        }
        try? output.fileHandleForReading.close()
    }
    private func send(_ object: [String: Any]) throws {
        var data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        data.append(10)
        try input.fileHandleForWriting.write(contentsOf: data)
    }
    func request(_ method: String, _ params: [String: Any], timeout: TimeInterval = 45) throws -> [String: Any] {
        sequence += 1; let id = sequence
        try send(["id": id, "method": method, "params": params])
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let newline = buffer.firstIndex(of: 10) {
                let line = buffer.prefix(upTo: newline); buffer.removeSubrange(...newline)
                guard let message = try JSONSerialization.jsonObject(with: line) as? [String: Any] else {
                    throw ContinuationNativeError.protocolFailure
                }
                if message["id"] as? Int == id, message["method"] == nil {
                    guard message["error"] == nil, let result = message["result"] as? [String: Any] else {
                        throw ContinuationNativeError.protocolFailure
                    }
                    return result
                }
                if let requestID = message["id"], message["method"] != nil {
                    try send(["id": requestID, "error": ["code": -32601, "message": "Switchboard does not execute requests"]])
                }
                continue
            }
            var fd = pollfd(fd: output.fileHandleForReading.fileDescriptor, events: Int16(POLLIN), revents: 0)
            let status = poll(&fd, 1, 100)
            guard status >= 0 else { if errno == EINTR { continue }; throw ContinuationNativeError.protocolFailure }
            if status == 0 { continue }
            var bytes = [UInt8](repeating: 0, count: 65_536)
            let count = Darwin.read(fd.fd, &bytes, bytes.count)
            guard count > 0 else { throw ContinuationNativeError.protocolFailure }
            buffer.append(contentsOf: bytes.prefix(count))
            guard buffer.count <= ContinuationLimits.input else { throw ContinuationError.tooLarge }
        }
        throw ContinuationNativeError.uncertain
    }
}

enum ContinuationNative {
    static func executable(_ name: String, home: URL = FileManager.default.homeDirectoryForCurrentUser) -> URL? {
        var roots = [home.appendingPathComponent(".local/bin").path, "/opt/homebrew/bin", "/usr/local/bin"]
        roots += (ProcessInfo.processInfo.environment["PATH"] ?? "").components(separatedBy: ":")
        if name == "codex" { roots += ["/Applications/Codex.app/Contents/Resources"] }
        return roots.filter { !$0.isEmpty }.map { URL(fileURLWithPath: $0).appendingPathComponent(name) }
            .first { FileManager.default.isExecutableFile(atPath: $0.path) }
    }

    static func storageRoot(_ destination: ContinuationSurface, home: URL = FileManager.default.homeDirectoryForCurrentUser,
                            environment: [String: String] = ProcessInfo.processInfo.environment) -> URL {
        if destination == .claudeCode || destination == .claudeDesktopCode {
            return environment["CLAUDE_CONFIG_DIR"].map { URL(fileURLWithPath: $0) } ?? home.appendingPathComponent(".claude")
        }
        if destination == .codexCLI {
            let cli = home.appendingPathComponent(".claude-acc/codex-cli")
            let mode = try? ContinuationFiles.read(cli.appendingPathComponent("mode"), limit: 32)
            if mode.flatMap({ String(data: $0, encoding: .utf8) })?.trimmingCharacters(in: .whitespacesAndNewlines) == "separate" {
                return cli.appendingPathComponent("home")
            }
        }
        return environment["CODEX_HOME"].map { URL(fileURLWithPath: $0) } ?? home.appendingPathComponent(".codex")
    }

    static func messages(_ draft: ContinuationDraft, bundle: ContinuationBundle) -> [ContinuationMessage] {
        var values = Array(draft.chat.messages.dropFirst(draft.firstMessage))
        // Preserve the original turns. Add only explicitly requested supplemental
        // context, never an implicit prompt that would run on opening.
        var additions: [String] = []
        if !draft.summary.isEmpty { additions.append("Summary supplied during cloning:\n" + draft.summary) }
        if !draft.nextStep.isEmpty && draft.nextStep != "Continue from the last request." { additions.append("Next step:\n" + draft.nextStep) }
        if !draft.files.isEmpty {
            additions.append("Files copied with this conversation:\n" + zip(draft.files, bundle.receipt.files.dropFirst()).map {
                $0.0.url.lastPathComponent + ": " + bundle.directory.appendingPathComponent($0.1).path
            }.joined(separator: "\n"))
        }
        if !additions.isEmpty { values.append(.init(role: "User", text: additions.joined(separator: "\n\n"))) }
        return values
    }
    static func role(_ message: ContinuationMessage) -> String { message.role == "Assistant" ? "assistant" : "user" }
    static func stamp(_ message: ContinuationMessage, fallback: Date) -> String {
        if let stamp = message.timestamp, ISO8601DateFormatter().date(from: stamp) != nil { return stamp }
        let formatter = ISO8601DateFormatter(); formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let stamp = message.timestamp, formatter.date(from: stamp) != nil { return stamp }
        return formatter.string(from: fallback)
    }
    static func lines(_ rows: [[String: Any]]) throws -> Data {
        var result = Data()
        for row in rows { result.append(try JSONSerialization.data(withJSONObject: row, options: [.sortedKeys])); result.append(10) }
        return result
    }
    static func claudeTranscript(_ messages: [ContinuationMessage], id: String, cwd: URL, title: String, date: Date) throws -> Data {
        var parent: Any = NSNull(); var rows: [[String: Any]] = []
        for (index, message) in messages.enumerated() {
            let uuid = UUID().uuidString.lowercased(); let role = role(message)
            rows.append(["type": role, "uuid": uuid, "parentUuid": parent, "sessionId": id,
                         "cwd": cwd.path.precomposedStringWithCanonicalMapping, "version": "switchboard-1.12.0", "isSidechain": false,
                         "userType": "external", "timestamp": stamp(message, fallback: date.addingTimeInterval(Double(index))),
                         "message": ["role": role, "content": [["type": "text", "text": message.text]]]])
            parent = uuid
        }
        rows.append(["type": "custom-title", "sessionId": id, "customTitle": title])
        return try lines(rows)
    }
    static func codexTranscript(_ messages: [ContinuationMessage], id: String, cwd: URL, date: Date) throws -> Data {
        let timestamp = ISO8601DateFormatter().string(from: date)
        var rows: [[String: Any]] = [["timestamp": timestamp, "type": "session_meta",
                                    "payload": ["id": id, "timestamp": timestamp, "cwd": cwd.path,
                                                "originator": "switchboard", "cli_version": "switchboard-1.12.0", "source": "cli"]]]
        var turn: String?
        func event(_ payload: [String: Any], _ stamp: String) { rows.append(["timestamp": stamp, "type": "event_msg", "payload": payload]) }
        for (index, message) in messages.enumerated() {
            let role = role(message); let stamp = stamp(message, fallback: date.addingTimeInterval(Double(index)))
            if role == "user" || turn == nil {
                if let turn { event(["type": "task_complete", "turn_id": turn], stamp) }
                turn = UUID().uuidString.lowercased()
                event(["type": "task_started", "turn_id": turn!, "collaboration_mode_kind": "default"], stamp)
            }
            event(["type": role == "user" ? "user_message" : "agent_message", "message": message.text], stamp)
            rows.append(["timestamp": stamp, "type": "response_item", "payload": ["type": "message", "role": role,
                "content": [["type": role == "user" ? "input_text" : "output_text", "text": message.text]]]])
        }
        if let turn { event(["type": "task_complete", "turn_id": turn], timestamp) }
        return try lines(rows)
    }
    static func save(_ result: ContinuationNativeResult, bundle: ContinuationBundle) throws {
        let temp = bundle.directory.appendingPathComponent(".destination-\(UUID()).json")
        defer { try? FileManager.default.removeItem(at: temp) }
        try ContinuationFiles.write(try JSONEncoder().encode(result), to: temp)
        guard rename(temp.path, bundle.directory.appendingPathComponent("destination.json").path) == 0 else { throw ContinuationError.storage }
    }
    static func saved(_ bundle: ContinuationBundle) throws -> ContinuationNativeResult? {
        let path = bundle.directory.appendingPathComponent("destination.json")
        guard FileManager.default.fileExists(atPath: path.path) else { return nil }
        return try JSONDecoder().decode(ContinuationNativeResult.self, from: ContinuationFiles.read(path, limit: 64_000))
    }
    static func publish(_ bytes: Data, to path: URL) throws {
        let temp = path.deletingLastPathComponent().appendingPathComponent(".switchboard-\(UUID()).tmp")
        defer { try? FileManager.default.removeItem(at: temp) }
        try ContinuationFiles.write(bytes, to: temp)
        // Atomic visibility, no replacement: readers never see a partial chain.
        guard link(temp.path, path.path) == 0 else { throw ContinuationError.storage }
    }

    static func create(_ draft: ContinuationDraft, bundle: ContinuationBundle, rootOverride: URL? = nil,
                       binaryOverride: URL? = nil) throws -> ContinuationNativeResult {
        guard draft.destination != .claudeChat else { throw ContinuationNativeError.chatUnsupported }
        let values = messages(draft, bundle: bundle)
        let root = rootOverride ?? storageRoot(draft.destination)
        let cwd = draft.workspace ?? bundle.directory.appendingPathComponent("workspace")
        if draft.workspace == nil { try ContinuationFiles.directory(cwd) }
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: cwd.path, isDirectory: &isDirectory), isDirectory.boolValue else { throw ContinuationNativeError.workspace }
        if var previous = try saved(bundle) {
            guard previous.destination == draft.destination else { throw ContinuationError.invalid }
            if (previous.destination == .claudeCode || previous.destination == .claudeDesktopCode),
               let path = previous.transcript, !FileManager.default.fileExists(atPath: path.path) {
                try publish(claudeTranscript(values, id: previous.id, cwd: previous.workspace,
                    title: draft.chat.title, date: bundle.receipt.createdAt), to: path)
            }
            try verify(previous, expected: values, binaryOverride: binaryOverride)
            previous.verified = true; try save(previous, bundle: bundle); return previous
        }
        let pending = bundle.directory.appendingPathComponent("creation-pending")
        guard !FileManager.default.fileExists(atPath: pending.path) else { throw ContinuationNativeError.uncertain }
        var result: ContinuationNativeResult
        if draft.destination == .claudeCode || draft.destination == .claudeDesktopCode {
            guard binaryOverride != nil || executable("claude") != nil else { throw ContinuationNativeError.missingCLI("Claude Code") }
            let id = UUID().uuidString.lowercased()
            // Claude's project key replaces each non-ASCII-alphanumeric character.
            let key = cwd.path.precomposedStringWithCanonicalMapping.utf16.map { unit -> String in
                if (65...90).contains(unit) || (97...122).contains(unit) || (48...57).contains(unit) { return String(UnicodeScalar(unit)!) }
                return "-"
            }.joined()
            guard key.utf8.count <= 200 else { throw ContinuationNativeError.workspace }
            try ContinuationFiles.directory(root)
            let projects = root.appendingPathComponent("projects"); try ContinuationFiles.directory(projects)
            let project = projects.appendingPathComponent(key); try ContinuationFiles.directory(project)
            let path = project.appendingPathComponent(id + ".jsonl")
            let bytes = try claudeTranscript(values, id: id, cwd: cwd, title: draft.chat.title, date: bundle.receipt.createdAt)
            result = .init(id: id, destination: draft.destination, workspace: cwd, storageRoot: root, transcript: path,
                           messageCount: values.count, verified: false, method: "claude-transcript-v1")
            // Persist the target identity before publishing any destination file.
            try save(result, bundle: bundle)
            do { try publish(bytes, to: path) }
            catch { throw ContinuationNativeError.verification }
        } else {
            guard let binary = binaryOverride ?? executable("codex") else { throw ContinuationNativeError.missingCLI("Codex") }
            let id = UUID().uuidString.lowercased()
            let path = bundle.directory.appendingPathComponent("source-rollout.jsonl")
            if !FileManager.default.fileExists(atPath: path.path) {
                try ContinuationFiles.write(try codexTranscript(values, id: id, cwd: cwd, date: bundle.receipt.createdAt), to: path)
            }
            let client = try ContinuationRPC(binary: binary, home: root, cwd: cwd); defer { client.close() }
            // This versioned path adapter uses Codex's own persistence/indexing.
            // It is deliberately a transcript fork: no inherited goals or work.
            try ContinuationFiles.write(Data("Native fork requested. Do not automatically repeat.".utf8), to: pending)
            let response = try client.request("thread/fork", ["threadId": id, "path": path.path, "cwd": cwd.path,
                "excludeTurns": true, "deferGoalContinuation": true, "threadSource": draft.destination == .codexCLI ? "switchboard-cli" : "switchboard-desktop"])
            guard let thread = response["thread"] as? [String: Any], let target = thread["id"] as? String,
                  UUID(uuidString: target) != nil else { throw ContinuationNativeError.uncertain }
            result = .init(id: target, destination: draft.destination, workspace: cwd, storageRoot: root,
                           transcript: (thread["path"] as? String).map { URL(fileURLWithPath: $0) },
                           messageCount: values.count, verified: false, method: "codex-rollout-fork-v1")
            try save(result, bundle: bundle)
            _ = try client.request("thread/name/set", ["threadId": target, "name": draft.chat.title])
        }
        try verify(result, expected: values, binaryOverride: binaryOverride)
        result.verified = true; try save(result, bundle: bundle)
        return result
    }

    static func verify(_ result: ContinuationNativeResult, expected: [ContinuationMessage], binaryOverride: URL? = nil) throws {
        let actual: [ContinuationMessage]
        if result.destination == .claudeCode || result.destination == .claudeDesktopCode {
            guard let path = result.transcript else { throw ContinuationNativeError.verification }
            actual = try ContinuationParser.parse(ContinuationFiles.read(path, limit: ContinuationLimits.input),
                extension: "jsonl", surface: .claudeCode, name: "Clone")[0].messages
        } else {
            guard let binary = binaryOverride ?? executable("codex") else { throw ContinuationNativeError.missingCLI("Codex") }
            let client = try ContinuationRPC(binary: binary, home: result.storageRoot, cwd: result.workspace); defer { client.close() }
            let response = try client.request("thread/read", ["threadId": result.id, "includeTurns": true])
            guard let thread = response["thread"] as? [String: Any], thread["id"] as? String == result.id,
                  let turns = thread["turns"] as? [[String: Any]] else { throw ContinuationNativeError.verification }
            var messages: [ContinuationMessage] = []
            for turn in turns {
                for item in turn["items"] as? [[String: Any]] ?? [] {
                    if item["type"] as? String == "agentMessage", let text = item["text"] as? String {
                        messages.append(.init(role: "Assistant", text: text))
                    } else if item["type"] as? String == "userMessage", let content = item["content"] as? [[String: Any]] {
                        messages.append(.init(role: "User", text: content.compactMap { $0["text"] as? String }.joined(separator: "\n\n")))
                    }
                }
            }
            actual = messages
        }
        guard actual.count == expected.count, zip(actual, expected).allSatisfy({ role($0.0) == role($0.1) && $0.0.text == $0.1.text }) else {
            throw ContinuationNativeError.verification
        }
    }
    static func quote(_ value: String) -> String { "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'" }
    static func terminalScript(_ result: ContinuationNativeResult, backend: String?, binaryOverride: URL? = nil) throws -> String {
        guard result.verified, UUID(uuidString: result.id) != nil else { throw ContinuationNativeError.verification }
        let claude = result.destination == .claudeCode || result.destination == .claudeDesktopCode
        guard let binary = binaryOverride ?? executable(claude ? "claude" : "codex") else { throw ContinuationNativeError.missingCLI(claude ? "Claude Code" : "Codex") }
        let args = claude ? ["--resume", result.id] + (result.destination == .claudeDesktopCode ? ["/desktop"] : []) : ["resume", result.id]
        // The established launcher selects Claude's existing configured login.
        let defaultClaudeRoot = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".claude")
        let useBackend = claude && backend != nil && result.storageRoot.standardizedFileURL.path == defaultClaudeRoot.standardizedFileURL.path
        let command = useBackend ? [backend!, "cli", "launch", "claude", "--"] + args : [binary.path] + args
        let rootKey = claude ? "CLAUDE_CONFIG_DIR" : "CODEX_HOME"
        return "#!/bin/bash\nset -e\ncd -- \(quote(result.workspace.path))\nexport \(rootKey)=\(quote(result.storageRoot.path))\n" + command.map(quote).joined(separator: " ") + "\n"
    }
    static func codexLink(_ result: ContinuationNativeResult) throws -> URL {
        guard result.verified, result.destination == .codexDesktop, UUID(uuidString: result.id) != nil,
              let url = URL(string: "codex://threads/" + result.id) else { throw ContinuationNativeError.verification }
        return url
    }
}

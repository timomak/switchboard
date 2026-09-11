import Foundation
import Darwin

struct ContinuationProject: Identifiable {
    let id: String
    let workspace: URL
    let chats: [ContinuationLocalChat]
    var title: String { chats.first?.projectTitle ?? workspace.lastPathComponent }
    var membershipLabel: String { chats.first?.projectID == nil ? "Local folder" : "Project" }

    static func group(_ chats: [ContinuationLocalChat], includeArchived: Bool = false) -> [Self] {
        let groups = Dictionary(grouping: chats.filter { !$0.projectless && (includeArchived || !$0.archived) && ($0.projectWorkspace ?? $0.workspace) != nil }) {
            $0.chat.surface.rawValue + ":" + ($0.projectID ?? $0.workspace!.standardizedFileURL.path)
        }
        return groups.map { key, entries in
            Self(id: key, workspace: (entries[0].projectWorkspace ?? entries[0].workspace)!.standardizedFileURL,
                 chats: entries.sorted { $0.chat.importedAt > $1.chat.importedAt })
        }.sorted { ($0.chats.first?.chat.importedAt ?? .distantPast) > ($1.chats.first?.chat.importedAt ?? .distantPast) }
    }
}

enum ProjectFolderMode: String, Codable, CaseIterable {
    case shared, empty, copy
    static let defaultMode: Self = .shared
    var title: String {
        switch self {
        case .empty: return "New empty folder"
        case .copy: return "Copy source files"
        case .shared: return "Use source folder"
        }
    }
}

struct ProjectCloneItem: Codable, Identifiable {
    let id: UUID
    let title: String
    let chat: ContinuationChat?
    var result: ContinuationNativeResult?
    var issue: String?
    var desktopHandoff: String? = nil
}

struct ProjectCloneBatch: Codable, Identifiable {
    let id: UUID
    let name: String
    let destination: ContinuationSurface
    let storageRoot: URL
    let workspace: URL
    let folderMode: ProjectFolderMode
    let createdAt: Date
    var omissions: [String]
    var items: [ProjectCloneItem]
    var verifiedCount: Int { items.filter { $0.result?.verified == true }.count }
    var desktopReadyCount: Int { items.filter { $0.desktopHandoff == "opened" }.count }
    var resultTitle: String {
        destination == .claudeDesktopCode ? "\(desktopReadyCount) of \(items.count) chats ready in Claude" : "\(verifiedCount) of \(items.count) chats cloned"
    }
}

enum ProjectCloneEngine {
    static let manifest = "project.json"
    static func directory(_ batch: ProjectCloneBatch, store: ContinuationStore) -> URL {
        store.root.appendingPathComponent("projects").appendingPathComponent(batch.id.uuidString)
    }
    static func save(_ batch: ProjectCloneBatch, store: ContinuationStore) throws {
        let folder = directory(batch, store: store)
        let temp = folder.appendingPathComponent(".project-\(UUID()).json")
        defer { try? FileManager.default.removeItem(at: temp) }
        try ContinuationFiles.write(try JSONEncoder().encode(batch), to: temp)
        guard rename(temp.path, folder.appendingPathComponent(manifest).path) == 0 else { throw ContinuationError.storage }
    }
    static func load(_ directory: URL) throws -> ProjectCloneBatch {
        try JSONDecoder().decode(ProjectCloneBatch.self, from: ContinuationFiles.read(directory.appendingPathComponent(manifest), limit: ContinuationLimits.input))
    }
    static func recent(store: ContinuationStore) throws -> [ProjectCloneBatch] {
        let root = store.root.appendingPathComponent("projects")
        guard FileManager.default.fileExists(atPath: root.path) else { return [] }
        return try FileManager.default.contentsOfDirectory(at: root, includingPropertiesForKeys: nil)
            .filter { UUID(uuidString: $0.lastPathComponent) != nil }
            .compactMap { try? load($0) }.sorted { $0.createdAt > $1.createdAt }
    }
    static func prepare(project: ContinuationProject, selected: Set<String>, name: String,
                        destination: ContinuationSurface, mode: ProjectFolderMode,
                        store: ContinuationStore, read: (ContinuationLocalChat) throws -> ContinuationChat = ContinuationDiscovery.read) throws -> ProjectCloneBatch {
        guard destination != .claudeChat else { throw ContinuationNativeError.chatUnsupported }
        let cleanName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !cleanName.isEmpty, cleanName != ".", cleanName != "..", cleanName.utf8.count <= 100,
              !cleanName.contains("/"), !cleanName.contains(":"), !cleanName.contains("\0") else { throw ContinuationError.invalid }
        let entries = project.chats.filter { selected.contains($0.chat.id) }
        guard !entries.isEmpty, entries.count <= 200 else { throw ContinuationError.tooLarge }
        var size = 0
        var items: [ProjectCloneItem] = []
        for entry in entries {
            do {
                let chat = try read(entry)
                size += chat.messages.reduce(0) { $0 + $1.text.utf8.count }
                items.append(.init(id: UUID(), title: chat.title, chat: chat))
            } catch {
                items.append(.init(id: UUID(), title: entry.chat.title, chat: nil, issue: "Unavailable, changing or too large; omitted."))
            }
            guard size <= 40 * 1024 * 1024 else { throw ContinuationError.tooLarge }
        }
        guard items.contains(where: { $0.chat != nil }), size <= 40 * 1024 * 1024 else { throw ContinuationError.tooLarge }
        try store.setup()
        try ContinuationFiles.directory(store.root.appendingPathComponent("projects"))
        let id = UUID()
        let folder = store.root.appendingPathComponent("projects").appendingPathComponent(id.uuidString)
        try ContinuationFiles.directory(folder)
        var committed = false
        defer { if !committed { try? FileManager.default.removeItem(at: folder) } }
        let workspace = mode == .shared ? project.workspace : folder.appendingPathComponent(cleanName)
        var omissions = ["Folder-based membership; other worktrees, stores and cloud-only chats may be absent.",
                         "Project instructions, tools, running work and native attachment data are not transferred.",
                         "Desktop project registration and sidebar ordering are not preserved."]
        if mode == .shared {
            var isDirectory: ObjCBool = false
            guard FileManager.default.fileExists(atPath: workspace.path, isDirectory: &isDirectory), isDirectory.boolValue else { throw ContinuationNativeError.workspace }
            omissions.append("Working files are shared with the source; edits affect both.")
        } else {
            try ContinuationFiles.directory(workspace)
            if mode == .copy { omissions += try copyFiles(from: project.workspace, to: workspace) }
            else { omissions.append("Working files are not copied.") }
        }
        let batch = ProjectCloneBatch(id: id, name: cleanName, destination: destination,
            storageRoot: ContinuationNative.storageRoot(destination), workspace: workspace, folderMode: mode,
            createdAt: Date(), omissions: omissions, items: items)
        // Commit immutable source snapshots before any native destination writes.
        guard try JSONEncoder().encode(batch).count <= ContinuationLimits.input else { throw ContinuationError.tooLarge }
        try save(batch, store: store); committed = true
        return batch
    }

    static func copyFiles(from source: URL, to destination: URL) throws -> [String] {
        // No symlink traversal, Git metadata, dotfiles, executable agent configuration,
        // dependencies or generated output. Snapshot only bounded regular files.
        let excluded: Set<String> = ["node_modules", "vendor", "build", "dist", "target", "DerivedData", "AGENTS.md", "CLAUDE.md", "CLAUDE.local.md", "Secrets.plist"]
        let source = source.resolvingSymlinksInPath().standardizedFileURL
        let target = destination.resolvingSymlinksInPath().standardizedFileURL
        guard !target.path.hasPrefix(source.path + "/") else { throw ContinuationError.invalid }
        var enumerationFailed = false
        guard let walker = FileManager.default.enumerator(at: source, includingPropertiesForKeys: [.isRegularFileKey, .isDirectoryKey, .isSymbolicLinkKey], errorHandler: { _, _ in enumerationFailed = true; return false }) else { throw ContinuationError.invalid }
        var bytes = 0, count = 0, omitted = 0
        for case let url as URL in walker {
            try Task.checkCancellation()
            count += 1; guard count <= 10_000 else { throw ContinuationError.tooLarge }
            let values = try url.resourceValues(forKeys: [.isRegularFileKey, .isDirectoryKey, .isSymbolicLinkKey])
            let name = url.lastPathComponent
            if name.hasPrefix(".") || excluded.contains(name) || name.lowercased().contains("secret") || ["pem", "key", "p12", "env"].contains(url.pathExtension.lowercased()) || values.isSymbolicLink == true {
                walker.skipDescendants(); omitted += 1; continue
            }
            let normalizedPath = url.standardizedFileURL.path
            guard normalizedPath.hasPrefix(source.path + "/") else { throw ContinuationError.invalid }
            let relative = String(normalizedPath.dropFirst(source.path.count + 1))
            let output = target.appendingPathComponent(relative)
            if values.isDirectory == true { try ContinuationFiles.directory(output) }
            else if values.isRegularFile == true {
                let data = try ContinuationFiles.read(url, limit: 100 * 1024 * 1024 - bytes)
                bytes += data.count
                try ContinuationFiles.write(data, to: output)
            } else { omitted += 1 }
        }
        guard !enumerationFailed else { throw ContinuationError.invalid }
        return ["Copied regular files only; \(omitted) excluded entries. No Git history, dotfiles, dependencies or agent instructions. Historical absolute paths are unchanged."]
    }

    static func handoff(_ input: ProjectCloneBatch, store: ContinuationStore, backend: String?,
                        progress: (ProjectCloneBatch) -> Void = { _ in },
                        openChat: (String, URL) throws -> Void = { script, folder in
                            try ContinuationDesktopHandoff.run(script: script, directory: folder)
                        }) throws -> ProjectCloneBatch {
        guard input.destination == .claudeDesktopCode else { return input }
        let folder = directory(input, store: store)
        let fd = open(folder.appendingPathComponent(".lock").path, O_CREAT | O_RDWR | O_NOFOLLOW, S_IRUSR | S_IWUSR)
        guard fd >= 0 else { throw ContinuationError.storage }
        defer { close(fd) }
        guard flock(fd, LOCK_EX | LOCK_NB) == 0 else { throw ContinuationError.storage }
        defer { flock(fd, LOCK_UN) }
        var batch = try load(folder)
        for index in batch.items.indices {
            try Task.checkCancellation()
            guard let result = batch.items[index].result, result.verified,
                  batch.items[index].desktopHandoff == nil else { continue }
            batch.items[index].desktopHandoff = "opening"
            batch.items[index].issue = "Desktop opening was interrupted. Check Claude before opening again."
            try save(batch, store: store); progress(batch)
            do {
                let child = folder.appendingPathComponent("chats").appendingPathComponent(batch.items[index].id.uuidString)
                try openChat(ContinuationNative.terminalScript(result, backend: backend), child)
                batch.items[index].desktopHandoff = "opened"
                batch.items[index].issue = nil
            } catch {
                batch.items[index].desktopHandoff = "needs-attention"
                batch.items[index].issue = error is CancellationError ? "Opening stopped. The saved chat is kept." : (error as? ContinuationHandoffError)?.errorDescription ?? "Chat created, but desktop opening needs attention. Use Open in Terminal under Advanced."
                try save(batch, store: store); progress(batch)
                if error is CancellationError { throw CancellationError() }
                // Do not send the remaining chats into the same blocked launcher.
                return batch
            }
            try save(batch, store: store); progress(batch)
        }
        return batch
    }

    static func recordOpened(_ input: ProjectCloneBatch, itemID: UUID, store: ContinuationStore) throws -> ProjectCloneBatch {
        let folder = directory(input, store: store)
        let fd = open(folder.appendingPathComponent(".lock").path, O_CREAT | O_RDWR | O_NOFOLLOW, S_IRUSR | S_IWUSR)
        guard fd >= 0 else { throw ContinuationError.storage }
        defer { close(fd) }
        guard flock(fd, LOCK_EX | LOCK_NB) == 0 else { throw ContinuationError.storage }
        defer { flock(fd, LOCK_UN) }
        var batch = try load(folder)
        guard let index = batch.items.firstIndex(where: { $0.id == itemID }), batch.items[index].result?.verified == true else { throw ContinuationError.invalid }
        batch.items[index].desktopHandoff = "opened"; batch.items[index].issue = nil
        try save(batch, store: store)
        return batch
    }

    static func run(_ input: ProjectCloneBatch, store: ContinuationStore,
                    progress: (ProjectCloneBatch) -> Void = { _ in },
                    create: (ContinuationDraft, ContinuationBundle) throws -> ContinuationNativeResult = { try ContinuationNative.create($0, bundle: $1) }) throws -> ProjectCloneBatch {
        let folder = directory(input, store: store)
        let fd = open(folder.appendingPathComponent(".lock").path, O_CREAT | O_RDWR | O_NOFOLLOW, S_IRUSR | S_IWUSR)
        guard fd >= 0 else { throw ContinuationError.storage }
        defer { close(fd) }
        guard flock(fd, LOCK_EX | LOCK_NB) == 0 else { throw ContinuationError.storage }
        defer { flock(fd, LOCK_UN) }
        var batch = try load(folder)
        guard batch.storageRoot.standardizedFileURL == ContinuationNative.storageRoot(batch.destination).standardizedFileURL else { throw ContinuationError.changed }
        let children = ContinuationStore(root: folder.appendingPathComponent("chats"))
        for index in batch.items.indices {
            try Task.checkCancellation()
            guard batch.items[index].result?.verified != true, let chat = batch.items[index].chat else { continue }
            var draft = ContinuationDraft(chat: chat, destination: batch.destination)
            draft.workspace = batch.workspace
            do {
                let id = batch.items[index].id
                let path = children.root.appendingPathComponent(id.uuidString)
                let bundle: ContinuationBundle
                if FileManager.default.fileExists(atPath: path.path) {
                    let receipt = try JSONDecoder().decode(ContinuationReceipt.self, from: ContinuationFiles.read(path.appendingPathComponent("manifest.json"), limit: ContinuationLimits.input))
                    guard receipt.id == id, receipt.sourceID == chat.id, receipt.destination == batch.destination else { throw ContinuationError.invalid }
                    bundle = .init(directory: path, context: draft.context, receipt: receipt)
                } else { bundle = try children.prepare(draft, id: id) }
                batch.items[index].result = try create(draft, bundle)
                batch.items[index].issue = nil
            } catch {
                // The child engine owns identity and uncertain-creation protection.
                // Always reuse the child directory on retry, including after restart.
                batch.items[index].issue = (error as? LocalizedError)?.errorDescription ?? "Could not create this chat. Retry."
            }
            try save(batch, store: store); progress(batch)
        }
        return batch
    }
}

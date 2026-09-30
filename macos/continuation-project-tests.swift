import Foundation
import Darwin

@main
struct ProjectCloneTests {
    static var checks = 0
    static func check(_ value: Bool, _ name: String) {
        guard value else { fatalError(name) }; checks += 1; print("  ✓ " + name)
    }
    static func main() throws {
        setbuf(stdout, nil)
        let root = URL(fileURLWithPath: "/tmp").appendingPathComponent("sb-project-\(UUID())")
        try ContinuationFiles.directory(root)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("source")
        try ContinuationFiles.directory(source)
        try ContinuationFiles.write(Data("synthetic file".utf8), to: source.appendingPathComponent("readme.txt"))
        try ContinuationFiles.write(Data("synthetic excluded value".utf8), to: source.appendingPathComponent(".env"))
        try FileManager.default.createSymbolicLink(at: source.appendingPathComponent("outside"), withDestinationURL: root)
        let chats = try (0..<3).map { index in
            try ContinuationChat.make(surface: .claudeCode, title: "Chat \(index)", messages: [
                .init(role: "User", text: "Synthetic user \(index)"), .init(role: "Assistant", text: "Synthetic assistant \(index)")])
        }
        let entries = chats.map { ContinuationLocalChat(chat: $0, location: .unavailable, workspace: source) }
        let project = ContinuationProject.group(entries)[0]
        let other = ContinuationLocalChat(chat: chats[0], location: .unavailable, workspace: root.appendingPathComponent("different/source"))
        check(ContinuationProject.group(entries + [other]).count == 2, "Same folder names do not merge projects")
        var assigned = entries
        for index in assigned.indices {
            assigned[index].projectID = "brand-fixture"
            assigned[index].projectTitle = "Brand fixture"
            assigned[index].projectWorkspace = source
        }
        assigned[2].workspace = root.appendingPathComponent("older-location")
        var archived = assigned[0]; archived.archived = true
        let active = ContinuationProject.group(assigned + Array(repeating: archived, count: 17))
        check(active.count == 1 && active[0].chats.count == 3, "Explicit membership includes other folder and excludes 17 archived chats")
        check(ContinuationProject.group(assigned + [archived], includeArchived: true)[0].chats.count == 4, "Archives are an explicit option")
        var projectless = assigned[0]; projectless.projectless = true
        check(ContinuationProject.group([projectless]).isEmpty, "Explicit projectless chats do not fall back to a folder project")
        let store = ContinuationStore(root: root.appendingPathComponent("copies"))
        let selection = Set(chats.map(\.id))
        let batch = try ProjectCloneEngine.prepare(project: project, selected: selection, name: "Independent copy",
            destination: .claudeCode, mode: .copy, store: store, read: { entry in
                if entry.chat.id == chats[2].id { throw ContinuationError.empty }; return entry.chat
            })
        check(batch.items.count == 3 && batch.items.filter { $0.chat == nil }.count == 1, "Unavailable chat remains in batch total")
        check(batch.workspace != source, "Default copy has independent workspace")
        check(FileManager.default.fileExists(atPath: batch.workspace.appendingPathComponent("readme.txt").path), "Regular source file copied")
        check(!FileManager.default.fileExists(atPath: batch.workspace.appendingPathComponent(".env").path), "Dotfiles excluded")
        check(!FileManager.default.fileExists(atPath: batch.workspace.appendingPathComponent("outside").path), "Symlinks not followed")
        let nativeRoot = root.appendingPathComponent("isolated-claude")
        let creator: (ContinuationDraft, ContinuationBundle) throws -> ContinuationNativeResult = {
            try ContinuationNative.create($0, bundle: $1, rootOverride: nativeRoot, binaryOverride: URL(fileURLWithPath: "/usr/bin/true"))
        }
        let partial = try ProjectCloneEngine.run(batch, store: store, create: { draft, bundle in
            if draft.chat.id == chats[1].id { throw ContinuationError.storage }
            return try creator(draft, bundle)
        })
        check(partial.verifiedCount == 1, "Failure preserves successful native chat")
        let firstID = partial.items.compactMap { $0.result?.id }.first!
        let restarted = try ProjectCloneEngine.load(ProjectCloneEngine.directory(batch, store: store))
        let complete = try ProjectCloneEngine.run(restarted, store: store, create: creator)
        check(complete.verifiedCount == 2, "Restart retries only unfinished readable chat")
        check(complete.items.compactMap { $0.result?.id }.contains(firstID), "Successful chat identity remains stable")
        let native = complete.items.first { $0.result?.verified == true }!
        var catalogChat = native.chat!; catalogChat.title = "First prompt fallback"
        let loaded = try ContinuationDiscovery.read(.init(chat: catalogChat, location: .session(native.result!.transcript!), workspace: source))
        check(loaded.title == native.title, "Full transcript preserves renamed Claude title")
        let again = try ProjectCloneEngine.run(batch, store: store, create: { _, _ in fatalError("Must not recreate success") })
        check(again.verifiedCount == 2, "Stale caller reloads durable completion")
        let uncertain = try ProjectCloneEngine.prepare(project: project, selected: [chats[0].id], name: "Uncertain",
            destination: .claudeCode, mode: .empty, store: store, read: { $0.chat })
        let pending = try ProjectCloneEngine.run(uncertain, store: store, create: { _, bundle in
            try ContinuationFiles.write(Data("pending".utf8), to: bundle.directory.appendingPathComponent("creation-pending"))
            throw ContinuationNativeError.uncertain
        })
        let recheck = try ProjectCloneEngine.run(pending, store: store, create: creator)
        check(recheck.verifiedCount == 0 && recheck.items[0].issue == ContinuationNativeError.uncertain.errorDescription, "Unknown creation cannot duplicate on retry")
        check(try ProjectCloneEngine.recent(store: store).count == 2, "Saved batches available for recovery")
        do {
            _ = try ProjectCloneEngine.prepare(project: project, selected: selection, name: "../escape", destination: .claudeCode, mode: .empty, store: store, read: { $0.chat })
            fatalError("Invalid name accepted")
        } catch ContinuationError.invalid { check(true, "Project name cannot escape copy directory") }
        let desktop = try ProjectCloneEngine.prepare(project: project, selected: [chats[0].id], name: "Desktop",
            destination: .claudeDesktopCode, mode: .empty, store: store, read: { $0.chat })
        let persisted = try ProjectCloneEngine.run(desktop, store: store, create: { draft, bundle in
            var result = try creator(draft, bundle)
            result.storageRoot = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".claude")
            return result
        })
        var handoffs = 0
        let opened = try ProjectCloneEngine.handoff(persisted, store: store, backend: "/fixture/switchboard-backend", openChat: { script, _ in
            check(script.contains("'/fixture/switchboard-backend' 'cli' 'launch' 'claude' '--'"), "Automatic handoff uses the same configured launcher as Open")
            handoffs += 1
        })
        check(handoffs == 1 && opened.items[0].desktopHandoff == "opened", "Desktop destination performs handoff after persistence")
        _ = try ProjectCloneEngine.handoff(persisted, store: store, backend: "/fixture/switchboard-backend", openChat: { _, _ in fatalError("Repeated handoff") })
        check(true, "Desktop handoff receipt prevents automatic repetition")
        check(persisted.desktopReadyCount == 0 && opened.desktopReadyCount == 1, "Persisted history is not reported as desktop-ready until handoff completes")
        var failed = opened; failed.items[0].desktopHandoff = "needs-attention"; failed.items[0].issue = "Opening failed"
        try ProjectCloneEngine.save(failed, store: store)
        let recovered = try ProjectCloneEngine.recordOpened(failed, itemID: failed.items[0].id, store: store)
        check(recovered.desktopReadyCount == 1 && recovered.items[0].issue == nil, "Successful manual Open clears failure and persists desktop-ready status")
        check(ProjectFolderMode.defaultMode == .shared, "Project cloning defaults to the original folder")
        let sourceBefore = try Data(contentsOf: source.appendingPathComponent("readme.txt"))
        let shared = try ProjectCloneEngine.prepare(project: project, selected: selection, name: "Shared history",
            destination: .claudeCode, mode: .defaultMode, store: store, read: { $0.chat })
        let sharedResult = try ProjectCloneEngine.run(shared, store: store, create: creator)
        check(sharedResult.items.allSatisfy { $0.result?.workspace == source }, "Every copied chat uses the same original source folder")
        check(try Data(contentsOf: source.appendingPathComponent("readme.txt")) == sourceBefore, "Shared-folder preparation leaves working files unchanged")
        var queued = recovered
        queued.items[0].desktopHandoff = nil
        var second = queued.items[0]
        second = ProjectCloneItem(id: UUID(), title: second.title, chat: second.chat, result: second.result)
        queued.items.append(second)
        try ProjectCloneEngine.save(queued, store: store)
        var attempts = 0, progressStates: [String] = []
        let blocked = try ProjectCloneEngine.handoff(queued, store: store, backend: "/fixture/backend", progress: { batch in
            progressStates.append(batch.items[0].desktopHandoff ?? "pending")
        }, openChat: { _, _ in attempts += 1; throw ContinuationHandoffError.timedOut })
        check(attempts == 1 && blocked.items[1].desktopHandoff == nil, "First handoff failure stops the batch instead of timing out every chat")
        check(progressStates == ["opening", "needs-attention"], "Opening and failure progress are emitted immediately")
        let transferred = try ProjectCloneEngine.transferData(project: project, selected: [chats[0].id, chats[1].id], name: "Selected project chats", read: { $0.chat })
        let package = try ContinuationTransfer.decode(transferred)
        check(package.kind == .project && package.chats.count == 2 && package.title == "Selected project chats", "Project transfer contains only the named selected chat set")
        let transferText = String(decoding: transferred, as: UTF8.self)
        check(!transferText.contains(source.path) && !transferText.contains("readme.txt") && !transferText.contains("synthetic excluded value"), "Project transfer exports no workspace path, source files or excluded file contents")
        do {
            _ = try ProjectCloneEngine.transferData(project: project, selected: selection, name: "Unreadable", read: { entry in
                if entry.chat.id == chats[1].id { throw ContinuationError.changed }; return entry.chat
            })
            fatalError("Unreadable selected chat silently omitted")
        } catch ContinuationError.changed { check(true, "A changing selected chat fails the whole export") }
        let importStore = ContinuationStore(root: root.appendingPathComponent("transfer-imports"))
        do {
            _ = try ProjectCloneEngine.prepareTransfer(chats: package.chats, name: package.title, destination: .claudeCode,
                workspace: root.appendingPathComponent("missing-workspace"), store: importStore)
            fatalError("Missing receiver folder accepted")
        } catch ContinuationNativeError.workspace { check(true, "A missing receiver folder blocks project import before creating state") }
        check(!FileManager.default.fileExists(atPath: importStore.root.path), "Invalid workspace creates no batch or native chats")
        let destinationFolder = root.appendingPathComponent("different-user-different-folder")
        try ContinuationFiles.directory(destinationFolder)
        try ContinuationFiles.write(Data("existing destination file".utf8), to: destinationFolder.appendingPathComponent("keep.txt"))
        let imported = try ProjectCloneEngine.prepareTransfer(chats: package.chats, name: package.title, destination: .claudeDesktopCode,
            workspace: destinationFolder, store: importStore)
        check(imported.isTransferImport && imported.workspace == destinationFolder && imported.items.allSatisfy { $0.result == nil }, "Reviewed import prepares fresh local items against only the chosen receiver folder")
        check(try ProjectCloneEngine.load(ProjectCloneEngine.directory(imported, store: importStore)).isTransferImport, "Transfer no-auto-open policy survives restarting Switchboard")
        let importedPartial = try ProjectCloneEngine.run(imported, store: importStore, create: { draft, bundle in
            if draft.chat.id == package.chats[1].id { throw ContinuationError.storage }
            return try creator(draft, bundle)
        })
        check(importedPartial.verifiedCount == 1, "Partial transfer failure keeps the successfully verified chat")
        let importedID = importedPartial.items[0].result!.id
        let importedComplete = try ProjectCloneEngine.run(imported, store: importStore, create: creator)
        check(importedComplete.verifiedCount == 2 && importedComplete.items[0].result?.id == importedID, "Transfer retry loads durable completion without duplicating success")
        check(importedComplete.items.allSatisfy { $0.result?.workspace == destinationFolder && $0.result?.id != $0.chat?.id }, "Every native imported chat receives a fresh ID and the receiver's folder")
        _ = try ProjectCloneEngine.handoff(importedComplete, store: importStore, backend: nil, openChat: { _, _ in fatalError("Transfer must not open native chats automatically") })
        check(importedComplete.desktopReadyCount == 0 && importedComplete.resultTitle == "2 of 2 chats imported", "Transfer reports verified import without claiming or triggering desktop opening")
        check(try String(contentsOf: destinationFolder.appendingPathComponent("keep.txt"), encoding: .utf8) == "existing destination file", "Import leaves receiver working files unchanged")
        check(!FileManager.default.fileExists(atPath: destinationFolder.appendingPathComponent("readme.txt").path), "Project transfer never reconstructs source working files")
        print("✓ \(checks) project clone checks passed; isolated synthetic stores only.")
    }
}

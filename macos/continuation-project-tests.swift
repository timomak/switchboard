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
        let persisted = try ProjectCloneEngine.run(desktop, store: store, create: creator)
        var handoffs = 0
        let opened = try ProjectCloneEngine.handoff(persisted, store: store, openChat: { _, _ in handoffs += 1 })
        check(handoffs == 1 && opened.items[0].desktopHandoff == "opened", "Desktop destination performs handoff after persistence")
        _ = try ProjectCloneEngine.handoff(persisted, store: store, openChat: { _, _ in fatalError("Repeated handoff") })
        check(true, "Desktop handoff receipt prevents automatic repetition")
        print("✓ \(checks) project clone checks passed; isolated synthetic stores only.")
    }
}

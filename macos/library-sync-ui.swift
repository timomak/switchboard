import Cocoa
import SwiftUI

enum LibrarySyncTarget: String, CaseIterable, Identifiable, Codable {
    case codex
    case claudeCode = "claude_code"
    case cowork

    var id: String { rawValue }
    var commandName: String { self == .claudeCode ? "claude-code" : rawValue }
    var title: String {
        switch self {
        case .codex: return "Codex"
        case .claudeCode: return "Claude Code"
        case .cowork: return "Cowork"
        }
    }
}

struct LibraryInventoryItem: Decodable, Identifiable {
    let id: String
    let name: String
    let kind: String
    var origin: String?
    let classification: String
    var detail: String?
    var supportedTargets: [LibrarySyncTarget]?
    var requirements: [String]?

    enum CodingKeys: String, CodingKey {
        case id, name, kind, origin, classification, detail, requirements
        case supportedTargets = "supported_targets"
    }
    var adoptable: Bool { classification == "custom" && ["skill", "mcp"].contains(kind) && !(supportedTargets ?? []).isEmpty }
}

struct LibraryDestinationStatus: Decodable, Identifiable {
    let target: LibrarySyncTarget
    let state: String
    var detail: String?
    var id: String { target.rawValue }
}

struct LibraryConflictCandidate: Decodable, Identifiable {
    let revision: String
    let label: String
    var detail: String?
    var id: String { revision }
}

struct LibraryManagedItem: Decodable, Identifiable {
    let id: String
    let name: String
    let kind: String
    let enabled: Bool
    let targets: [LibrarySyncTarget]
    let state: String
    var detail: String?
    var destinations: [LibraryDestinationStatus]?
    var conflicts: [LibraryConflictCandidate]?
    var requirements: [String]?
    var deleted: Bool?
}

struct LibraryCoworkStatus: Decodable {
    let state: String
    var detail: String?
    var zipPath: String?
    var exportedVersion: String?
    var installedVersion: String?

    enum CodingKeys: String, CodingKey {
        case state, detail
        case zipPath = "zip_path", exportedVersion = "exported_version", installedVersion = "installed_version"
    }
    var markedInstalled: Bool {
        ["confirmed", "ready"].contains(state) && exportedVersion != nil && installedVersion == exportedVersion
    }
}

struct LibrarySyncStatus: Decodable {
    let available: Bool
    let syncSkills: Bool
    let syncMCP: Bool
    var message: String?
    var lastSyncAt: String?
    var inventory: [LibraryInventoryItem]?
    var items: [LibraryManagedItem]?
    var cowork: LibraryCoworkStatus?

    enum CodingKeys: String, CodingKey {
        case available, message, inventory, items, cowork
        case syncSkills = "sync_skills", syncMCP = "sync_mcp", lastSyncAt = "last_sync_at"
    }
    var managedItems: [LibraryManagedItem] { (items ?? []).filter { $0.deleted != true && $0.state != "removed" } }
    func enabled(for kind: String) -> Bool { kind == "skill" ? syncSkills : kind == "mcp" && syncMCP }
}

func parseLibrarySyncStatus(_ data: Data) -> LibrarySyncStatus? {
    try? JSONDecoder().decode(LibrarySyncStatus.self, from: data)
}

func libraryTargetArgument(_ targets: Set<LibrarySyncTarget>) -> String {
    LibrarySyncTarget.allCases.filter { targets.contains($0) }.map(\.commandName).joined(separator: ",")
}

func librarySyncCanRun(skills: Bool, mcp: Bool, available: Bool, preview: Bool, accountOperation: Bool) -> Bool {
    (skills || mcp) && available && !preview && !accountOperation
}

func librarySyncClassificationLabel(_ classification: String) -> String {
    switch classification {
    case "custom": return "Personal"
    case "plugin_managed": return "Managed by a plugin"
    case "account_managed": return "Managed by the account"
    default: return "Not supported"
    }
}

func librarySyncStateLabel(_ state: String) -> String {
    switch state {
    case "ready": return "Ready"
    case "waiting", "waiting_for_app": return "Waiting for app"
    case "needs_setup": return "Needs setup"
    case "sign_in_needed": return "Sign-in needed"
    case "conflict": return "Conflict"
    case "install_required": return "Install required"
    case "update_available": return "Update available"
    case "confirmed": return "Import confirmed"
    case "disabled": return "Disabled"
    case "removed": return "Removed"
    case "unsupported": return "Not supported"
    case "error": return "Needs attention"
    default: return "Not checked"
    }
}

enum LibrarySyncPage: Equatable {
    case overview, inventory(String), item(String), cowork
}

struct LibrarySyncCommandResult {
    let data: Data
    var succeeded = true
}

private func librarySyncCommand(_ arguments: [String]) -> LibrarySyncCommandResult? {
    guard let binary = resolveBinary("ai-usagebar") else { return nil }
    let process = Process()
    process.executableURL = URL(fileURLWithPath: binary)
    process.arguments = ["library-sync"] + arguments + ["--json"]
    let pipe = Pipe()
    process.standardOutput = pipe
    process.standardError = FileHandle.nullDevice
    let watchdog = DispatchWorkItem { if process.isRunning { process.terminate() } }
    DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + 300, execute: watchdog)
    defer { watchdog.cancel() }
    do {
        try process.run()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return LibrarySyncCommandResult(data: data, succeeded: process.terminationStatus == 0)
    } catch { return nil }
}

final class LibrarySyncModel: ObservableObject {
    @Published var status: LibrarySyncStatus?
    @Published var busy = false
    @Published var message: String?
    @Published var page: LibrarySyncPage = .overview
    @Published var tab = "managed"
    var protectPopover: (Bool) -> Void = { _ in }
    private var timer: Timer?
    private var shouldPause: () -> Bool = { false }
    private let execute: ([String]) -> LibrarySyncCommandResult?
    let preview: Bool

    init(status: LibrarySyncStatus? = nil, preview: Bool = SWITCHBOARD_PREVIEW,
         execute: @escaping ([String]) -> LibrarySyncCommandResult? = librarySyncCommand) {
        self.status = status; self.preview = preview; self.execute = execute
    }

    deinit { timer?.invalidate() }

    var canChange: Bool { !preview && !shouldPause() && !busy && status != nil }
    var canRun: Bool {
        guard let status else { return false }
        return librarySyncCanRun(skills: status.syncSkills, mcp: status.syncMCP,
            available: status.available, preview: preview, accountOperation: shouldPause())
    }
    var hasCoworkItems: Bool { status?.managedItems.contains { $0.enabled && $0.targets.contains(.cowork) } == true }
    var canExportCowork: Bool { hasCoworkItems || status?.cowork?.exportedVersion != nil }

    func start(shouldPause: @escaping () -> Bool) {
        guard timer == nil else { return }
        self.shouldPause = shouldPause
        refresh()
        timer = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
            guard let self, !self.shouldPause() else { return }
            self.refresh(thenSync: true)
        }
        // Avoid starting both independent sync features at the same instant.
        timer?.fireDate = Date().addingTimeInterval(15)
        timer?.tolerance = 5
    }

    /// Inventory and status are read-only, including when the page is opened.
    func refresh(inventory: Bool = false, thenSync: Bool = false) {
        perform([inventory ? "inventory" : "status"], thenSync: thenSync)
    }

    func setEnabled(kind: String, enabled: Bool) {
        guard canChange, ["skill", "mcp"].contains(kind),
              enabled != status?.enabled(for: kind), !enabled || status?.available == true else { return }
        perform([enabled ? "enable" : "disable", "--kind", kind == "skill" ? "skills" : "mcp"], thenSync: enabled)
    }

    func syncNow() {
        guard canChange, canRun else { return }
        perform(["run"])
    }

    func adopt(_ item: LibraryInventoryItem, targets: Set<LibrarySyncTarget>) {
        guard canChange, item.adoptable, status?.enabled(for: item.kind) == true,
              !targets.isEmpty, targets.isSubset(of: Set(item.supportedTargets ?? [])) else { return }
        perform(["adopt", item.id, "--targets", libraryTargetArgument(targets)], thenSync: true) { [weak self] in
            self?.page = .overview; self?.tab = "managed"
        }
    }

    func setTargets(_ item: LibraryManagedItem, targets: Set<LibrarySyncTarget>) {
        guard canChange, !targets.isEmpty, targets != Set(item.targets) else { return }
        perform(["targets", item.id, "--targets", libraryTargetArgument(targets)], thenSync: true)
    }

    func setItemEnabled(_ item: LibraryManagedItem, enabled: Bool) {
        guard canChange, enabled != item.enabled else { return }
        perform(["set-enabled", item.id, "--enabled", enabled ? "true" : "false"], thenSync: true)
    }

    func remove(_ item: LibraryManagedItem) {
        guard canChange else { return }
        perform(["remove", item.id], thenSync: true) { [weak self] in self?.page = .overview }
    }

    func confirmRemoval(_ item: LibraryManagedItem) {
        guard canChange else { return }
        protectPopover(true); defer { protectPopover(false) }
        let alert = NSAlert()
        alert.messageText = "Remove \(item.name)?"
        alert.informativeText = "This removes it from your shared library and its unchanged Switchboard installations. Local changes are kept for review."
        alert.addButton(withTitle: "Remove"); alert.addButton(withTitle: "Cancel")
        if alert.runModal() == .alertFirstButtonReturn { remove(item) }
    }

    func resolve(_ item: LibraryManagedItem, revision: String) {
        guard canChange, item.conflicts?.contains(where: { $0.revision == revision }) == true else { return }
        perform(["resolve", item.id, "--revision", revision], thenSync: true)
    }

    func exportCowork() {
        guard canChange, canExportCowork else { return }
        perform(["export-cowork"]) { [weak self] in self?.page = .cowork }
    }

    func confirmCowork() {
        guard canChange, let version = status?.cowork?.exportedVersion,
              !version.isEmpty, status?.cowork?.markedInstalled != true else { return }
        perform(["confirm-cowork", "--version", version])
    }

    func revealCowork() {
        guard !preview, let path = status?.cowork?.zipPath, path.hasPrefix("/"),
              URL(fileURLWithPath: path).pathExtension.lowercased() == "zip",
              FileManager.default.fileExists(atPath: path) else { return }
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }

    func openCowork() { if !preview { openApp("Claude") } }

    private func perform(_ arguments: [String], thenSync: Bool = false, completion: (() -> Void)? = nil) {
        guard !busy else { return }
        busy = true; message = nil
        let execute = self.execute
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let response = execute(arguments)
            let result = response.flatMap { parseLibrarySyncStatus($0.data) }
            let safeMessage = response.flatMap { try? JSONSerialization.jsonObject(with: $0.data) as? [String: Any] }?["message"] as? String
            DispatchQueue.main.async {
                guard let self else { return }
                self.busy = false
                guard response?.succeeded == true, var result else {
                    self.message = safeMessage ?? "Tools & skills could not finish. Refresh and try again."
                    return
                }
                if result.inventory == nil { result.inventory = self.status?.inventory }
                self.status = result
                completion?()
                if thenSync && self.canRun { self.perform(["run"]) }
            }
        }
    }
}

private struct LibraryTargetChoices: View {
    @Binding var selected: Set<LibrarySyncTarget>
    var available: Set<LibrarySyncTarget> = Set(LibrarySyncTarget.allCases)
    var disabled = false

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            ForEach(LibrarySyncTarget.allCases) { target in
                Toggle(target.title, isOn: Binding(
                    get: { selected.contains(target) },
                    set: { chosen in
                        if chosen { selected.insert(target) }
                        else { selected.remove(target) }
                    }))
                    .disabled(disabled || !available.contains(target))
            }
        }
    }
}

private func libraryKindLabel(_ kind: String) -> String { kind == "skill" ? "Skill" : "MCP setup" }

struct LibrarySyncView: View {
    @ObservedObject var model: LibrarySyncModel
    let height: CGFloat
    let accountOperation: Bool
    let close: () -> Void

    private var blocked: Bool { model.busy || model.preview || accountOperation || model.status == nil }
    private var title: String {
        switch model.page {
        case .overview: return "Tools & skills"
        case .inventory, .item: return "Library item"
        case .cowork: return "Install in Cowork"
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Button {
                    if model.page == .overview { close() } else { model.page = .overview }
                } label: { Image(systemName: "chevron.left") }
                    .buttonStyle(.plain).help("Back")
                Text(title).fontWeight(.medium)
                Spacer()
                if model.busy { ProgressView().controlSize(.small) }
                Button { model.refresh(inventory: true) } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.plain).disabled(model.busy).help("Refresh tools and skills")
            }.padding(.horizontal, 16).padding(.vertical, 13)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    switch model.page {
                    case .overview: overview
                    case .inventory(let id):
                        if let item = model.status?.inventory?.first(where: { $0.id == id }) {
                            LibraryAdoptView(model: model, item: item, blocked: blocked).id("inventory-\(id)")
                        } else { Text("This item is no longer in the inventory. Refresh to check again.").foregroundStyle(.secondary) }
                    case .item(let id):
                        if let item = model.status?.managedItems.first(where: { $0.id == id }) {
                            LibraryManagedView(model: model, item: item, blocked: blocked).id("managed-\(id)")
                        } else { Text("This item is no longer in your library.").foregroundStyle(.secondary) }
                    case .cowork: cowork
                    }
                    if accountOperation {
                        Text("Waiting for the account change to finish.").font(.caption).foregroundStyle(.secondary)
                    }
                    if let message = model.message, !message.isEmpty {
                        Text(message).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                }.frame(maxWidth: .infinity, alignment: .leading).padding(16)
            }
        }
        .font(.system(size: 13)).frame(height: height)
        .background(Color(nsColor: .windowBackgroundColor))
        .onAppear { model.refresh(inventory: true) }
    }

    private var overview: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Your tools on both Macs").font(.system(size: 17, weight: .semibold))
            Text("Choose personal skills and MCP setups to share through iCloud, then choose which apps use them.")
                .foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            VStack(alignment: .leading, spacing: 9) {
                Toggle("Sync skills", isOn: Binding(get: { model.status?.syncSkills == true }, set: { model.setEnabled(kind: "skill", enabled: $0) }))
                    .disabled(blocked || (model.status?.syncSkills != true && model.status?.available != true))
                Toggle("Sync MCP setups", isOn: Binding(get: { model.status?.syncMCP == true }, set: { model.setEnabled(kind: "mcp", enabled: $0) }))
                    .disabled(blocked || (model.status?.syncMCP != true && model.status?.available != true))
            }
            Text("Only items you add are shared. Sign-ins stay local. Turning sync off keeps current installations.")
                .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            if model.status?.available == false {
                Label("Turn on iCloud Drive in System Settings to connect this Mac.", systemImage: "icloud.slash")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Divider()
            Picker("Items", selection: $model.tab) {
                Text("Library (\(model.status?.managedItems.count ?? 0))").tag("managed")
                Text("Available (\(model.status?.inventory?.count ?? 0))").tag("available")
            }.pickerStyle(.segmented).labelsHidden()
            if model.tab == "available" {
                let inventory = model.status?.inventory ?? []
                if inventory.isEmpty {
                    Text(model.status == nil ? "Looking for personal items…" : "No supported personal items were found on this Mac.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                ForEach(inventory) { item in
                    Button { model.page = .inventory(item.id) } label: {
                        row(name: item.name, detail: [libraryKindLabel(item.kind), item.origin].compactMap { $0 }.joined(separator: " · "),
                            state: librarySyncClassificationLabel(item.classification))
                    }.buttonStyle(.plain)
                }
            } else {
                let items = model.status?.managedItems ?? []
                if items.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Your library is empty. Review Available and choose what to add.")
                            .font(.caption).foregroundStyle(.secondary)
                        Button("Browse available items") { model.tab = "available" }
                    }
                }
                ForEach(items) { item in
                    Button { model.page = .item(item.id) } label: {
                        row(name: item.name, detail: item.targets.map(\.title).joined(separator: " · "),
                            state: item.enabled ? librarySyncStateLabel(item.state) : "Disabled")
                    }.buttonStyle(.plain)
                }
            }
            Divider()
            HStack {
                Button("Sync now") { model.syncNow() }.disabled(blocked || !model.canRun)
                Spacer()
                Button("Install in Cowork…") { model.page = .cowork }
            }
            if let message = model.status?.message, !message.isEmpty {
                Text(message).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func row(name: String, detail: String, state: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(alignment: .leading, spacing: 4) {
                Text(name).fontWeight(.medium).multilineTextAlignment(.leading)
                Text(detail).font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.leading)
            }
            Spacer(minLength: 4)
            Text(state).font(.caption).foregroundStyle(.secondary)
            Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
        }.frame(maxWidth: .infinity, alignment: .leading).padding(.vertical, 4)
    }

    private var cowork: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Your library in Cowork").font(.system(size: 17, weight: .semibold))
            Text("Cowork uses a plugin installed through Claude. Repeat the installation for each Claude account where you want these items.")
                .foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            if let cowork = model.status?.cowork {
                Text(cowork.markedInstalled ? "Import confirmed by you" : librarySyncStateLabel(cowork.state)).fontWeight(.medium)
                if let detail = cowork.detail, !detail.isEmpty {
                    Text(detail).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
            }
            Text("1. Export your selected Cowork items.").fontWeight(.medium)
            Button("Export/update plugin") { model.exportCowork() }.disabled(blocked || !model.canExportCowork)
            if !model.canExportCowork {
                Text("First add an item to your library and select Cowork as a destination.").font(.caption).foregroundStyle(.secondary)
            } else if !model.hasCoworkItems {
                Text("Export and install an empty replacement to remove the items from your previous Cowork plugin.")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            Text("2. Open Claude’s plugin installation screen and select the exported ZIP.")
                .fontWeight(.medium).fixedSize(horizontal: false, vertical: true)
            HStack {
                Button("Show ZIP") { model.revealCowork() }.disabled(model.preview || model.status?.cowork?.zipPath == nil)
                Button("Open Claude") { model.openCowork() }.disabled(model.preview)
            }
            Text("3. Finish any setup in Claude, then record your installation here.")
                .fontWeight(.medium).fixedSize(horizontal: false, vertical: true)
            Button("I imported this version") { model.confirmCowork() }
                .disabled(blocked || model.status?.cowork?.exportedVersion == nil || model.status?.cowork?.markedInstalled == true)
            Text("This records your confirmation. Switchboard does not verify installation or sign-in across your Claude accounts.")
                .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
        }
    }
}

private struct LibraryAdoptView: View {
    @ObservedObject var model: LibrarySyncModel
    let item: LibraryInventoryItem
    let blocked: Bool
    @State private var targets: Set<LibrarySyncTarget> = []

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(item.name).font(.system(size: 17, weight: .semibold))
            Text([libraryKindLabel(item.kind), item.origin, librarySyncClassificationLabel(item.classification)].compactMap { $0 }.joined(separator: " · "))
                .font(.caption).foregroundStyle(.secondary)
            if let detail = item.detail, !detail.isEmpty { Text(detail).fixedSize(horizontal: false, vertical: true) }
            if item.adoptable {
                Text("Use in").fontWeight(.medium)
                LibraryTargetChoices(selected: $targets, available: Set(item.supportedTargets ?? []), disabled: blocked)
                if model.status?.enabled(for: item.kind) != true {
                    Text(item.kind == "skill" ? "Turn on Sync skills before adding this item." : "Turn on Sync MCP setups before adding this item.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Button("Add to shared library") { model.adopt(item, targets: targets) }
                    .disabled(blocked || targets.isEmpty || model.status?.enabled(for: item.kind) != true)
            } else {
                Text("Keep this item under its existing app or plugin’s management.")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            if let requirements = item.requirements, !requirements.isEmpty {
                Divider()
                Text("Setup needed").fontWeight(.medium)
                ForEach(Array(requirements.enumerated()), id: \.offset) { _, requirement in
                    Text(requirement).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }
}

private struct LibraryManagedView: View {
    @ObservedObject var model: LibrarySyncModel
    let item: LibraryManagedItem
    let blocked: Bool
    @State private var targets: Set<LibrarySyncTarget>

    init(model: LibrarySyncModel, item: LibraryManagedItem, blocked: Bool) {
        self.model = model; self.item = item; self.blocked = blocked
        _targets = State(initialValue: Set(item.targets))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(item.name).font(.system(size: 17, weight: .semibold))
            Text(libraryKindLabel(item.kind)).font(.caption).foregroundStyle(.secondary)
            Toggle("Enabled", isOn: Binding(get: { item.enabled }, set: { model.setItemEnabled(item, enabled: $0) }))
                .disabled(blocked)
            if let detail = item.detail, !detail.isEmpty { Text(detail).font(.caption).foregroundStyle(.secondary) }
            Text("Use in").fontWeight(.medium)
            LibraryTargetChoices(selected: $targets, disabled: blocked)
            Button("Save destinations") { model.setTargets(item, targets: targets) }
                .disabled(blocked || targets.isEmpty || targets == Set(item.targets))
            ForEach(item.destinations ?? []) { destination in
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text(destination.target.title).fontWeight(.medium)
                        Spacer()
                        Text(librarySyncStateLabel(destination.state)).font(.caption).foregroundStyle(.secondary)
                    }
                    if let detail = destination.detail, !detail.isEmpty {
                        Text(detail).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            if item.destinations?.contains(where: { ["needs_setup", "sign_in_needed"].contains($0.state) }) == true {
                Text("Set up missing credentials or paths locally, then sync again.")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            if let conflicts = item.conflicts, !conflicts.isEmpty {
                Divider()
                Text("Choose a version").fontWeight(.medium)
                Text("Your current installation is kept until you choose.").font(.caption).foregroundStyle(.secondary)
                ForEach(conflicts) { candidate in
                    VStack(alignment: .leading, spacing: 6) {
                        Text(candidate.label).fontWeight(.medium)
                        if let detail = candidate.detail, !detail.isEmpty {
                            Text(detail).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                        }
                        Button("Use this version") { model.resolve(item, revision: candidate.revision) }.disabled(blocked)
                    }
                }
            }
            Divider()
            Button("Remove from library…") { model.confirmRemoval(item) }.disabled(blocked)
        }
    }
}

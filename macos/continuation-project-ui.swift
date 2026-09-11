import Cocoa
import SwiftUI

@MainActor
final class ProjectCloneModel: ObservableObject {
    @Published var page = 1
    @Published var source: ContinuationSurface = .codexDesktop
    @Published var destination: ContinuationSurface = .claudeDesktopCode
    @Published var includeArchived = false
    private var catalog: [ContinuationLocalChat] = []
    func regroup() {
        projects = ContinuationProject.group(catalog, includeArchived: includeArchived)
        if let selected = project { select(selected) } else { selectedID = nil; selectedChats = [] }
    }
    @Published var projects: [ContinuationProject] = []
    @Published var selectedID: String?
    @Published var selectedChats = Set<String>()
    @Published var query = ""
    @Published var chatQuery = ""
    @Published var name = ""
    @Published var mode: ProjectFolderMode = .defaultMode
    @Published var batch: ProjectCloneBatch?
    @Published var recent: [ProjectCloneBatch] = []
    @Published var busy = false
    @Published var message: String?
    var protectPopover: (Bool) -> Void = { _ in }
    private var worker: Task<ProjectCloneBatch, Error>?
    let opener = ContinuationModel()
    func stop() { worker?.cancel() }
    func returnToProjects() {
        guard !busy, !opener.busy else { return }
        if let batch { recent = [batch] + recent.filter { $0.id != batch.id } }
        batch = nil; message = nil; page = 1; opener.finish()
    }
    func openTerminal(_ item: ProjectCloneItem) {
        guard !busy, !opener.busy, let result = item.result, result.verified else { return }
        do { runInTerminal(try ContinuationNative.terminalScript(result, backend: resolveBinary("ai-usagebar"))) }
        catch { message = (error as? LocalizedError)?.errorDescription ?? "Could not open Terminal." }
    }
    private func run(_ batch: ProjectCloneBatch) async throws -> ProjectCloneBatch {
        let backend = resolveBinary("ai-usagebar")
        let operation = Task.detached { [store] in
            let copied = try ProjectCloneEngine.run(batch, store: store)
            return try ProjectCloneEngine.handoff(copied, store: store, backend: backend)
        }
        worker = operation
        defer { worker = nil }
        do { return try await operation.value }
        catch is CancellationError {
            message = "Stopped. Completed copies are kept."
            return try ProjectCloneEngine.load(ProjectCloneEngine.directory(batch, store: store))
        }
    }
    let store = ContinuationStore(root: FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!.appendingPathComponent("Switchboard Continuations"))
    var project: ContinuationProject? { projects.first { $0.id == selectedID } }
    var filtered: [ContinuationProject] {
        projects.filter { query.isEmpty || $0.title.localizedCaseInsensitiveContains(query) || $0.workspace.path.localizedCaseInsensitiveContains(query) || $0.chats.contains { $0.chat.title.localizedCaseInsensitiveContains(query) } }
    }
    func select(_ project: ContinuationProject) {
        selectedID = project.id; selectedChats = Set(project.chats.map { $0.chat.id })
        name = project.title + " copy"; chatQuery = ""
    }
    func refresh() {
        guard !busy else { return }; busy = true; message = nil
        let surface = source
        var environment = ProcessInfo.processInfo.environment
        if surface == .codexCLI { environment["CODEX_HOME"] = ContinuationNative.storageRoot(.codexCLI).path }
        Task {
            defer { busy = false }
            do {
                let entries = try await Task.detached { try ContinuationDiscovery.catalog(surface: surface, environment: environment) }.value
                catalog = entries; selectedID = nil; selectedChats = []; regroup()
                recent = try await Task.detached { [store] in try ProjectCloneEngine.recent(store: store) }.value
            } catch { message = "Could not read local projects. Try refreshing." }
        }
    }
    func create() {
        guard !busy, let project, !selectedChats.isEmpty else { return }
        busy = true; message = nil; protectPopover(true)
        let selection = selectedChats, title = name, target = destination, folderMode = mode, store = store
        Task {
            defer { busy = false; protectPopover(false) }
            do {
                batch = try await Task.detached { try ProjectCloneEngine.prepare(project: project, selected: selection, name: title, destination: target, mode: folderMode, store: store) }.value
                page = 3
                let prepared = batch!
                batch = try await run(prepared)
            } catch { message = (error as? LocalizedError)?.errorDescription ?? "Could not clone these chats." }
        }
    }
    func retry() {
        guard !busy, let batch else { return }
        busy = true; message = nil; protectPopover(true)
        Task {
            defer { busy = false; protectPopover(false) }
            do { self.batch = try await run(batch) }
            catch { message = (error as? LocalizedError)?.errorDescription ?? "Could not resume cloning." }
        }
    }
    func open(_ item: ProjectCloneItem, openCLI: (String, String?) -> Void) {
        guard !busy, !opener.busy, let batch, let chat = item.chat, let result = item.result, result.verified else { return }
        let folder = ProjectCloneEngine.directory(batch, store: store).appendingPathComponent("chats").appendingPathComponent(item.id.uuidString)
        do {
            let receipt = try JSONDecoder().decode(ContinuationReceipt.self, from: ContinuationFiles.read(folder.appendingPathComponent("manifest.json"), limit: ContinuationLimits.input))
            var draft = ContinuationDraft(chat: chat, destination: batch.destination); draft.workspace = batch.workspace
            opener.draft = draft; opener.nativeResult = result
            opener.bundle = .init(directory: folder, context: draft.context, receipt: receipt)
            opener.openDestination(openCLI: openCLI, onOpened: { [weak self] in
                guard let self, batch.destination == .claudeDesktopCode else { return }
                do { self.batch = try ProjectCloneEngine.recordOpened(batch, itemID: item.id, store: self.store) }
                catch { self.message = "Chat opened, but its status could not be saved." }
            })
        } catch { message = "Could not open the saved chat. Its copy is still available in the destination." }
    }
}

struct ProjectCloneView: View {
    @ObservedObject var model: ProjectCloneModel
    let height: CGFloat
    let close: () -> Void
    let openCLI: (String, String?) -> Void
    @State private var advanced = false
    @State private var visibleCount = 100

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button { if model.page == 1 { close() } else { model.returnToProjects() } } label: { Image(systemName: "chevron.left") }.buttonStyle(.plain).help("Back").disabled(model.busy || model.opener.busy)
                Text("Clone project").fontWeight(.medium)
                Spacer()
                Image(systemName: "square.on.square").foregroundStyle(.secondary)
            }.padding(16)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    if model.page == 1 { choose }
                    else if model.page == 2 { review }
                    else { results }
                    if model.busy { ProgressView(model.page == 3 && model.destination == .claudeDesktopCode ? "Copying and opening in Claude…" : "Preparing…").controlSize(.small) }
                    if let message = model.message { Text(message).font(.caption) }
                    ProjectCloneOpeningStatus(model: model.opener)
                }.padding(16)
            }
            Divider()
            HStack {
                if model.busy && model.page == 3 {
                    Button("Stop after this chat") { model.stop() }
                } else {
                    Button(model.page == 1 ? "Cancel" : "Back") {
                        if model.page == 1 { close() } else { model.returnToProjects() }
                    }.disabled(model.busy || model.opener.busy)
                    if model.page == 3 {
                        Button("Done") { model.returnToProjects(); close() }.disabled(model.busy || model.opener.busy)
                    }
                }
                Spacer()
                if model.page == 1 {
                    Button("Next") { model.page = 2; advanced = false }.disabled(model.selectedChats.isEmpty || model.busy).keyboardShortcut(.defaultAction)
                } else if model.page == 2 {
                    Button("Clone \(model.selectedChats.count) chats") { model.create() }
                        .disabled(model.busy || model.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.destination == .claudeChat).keyboardShortcut(.defaultAction)
                } else if let batch = model.batch, batch.items.contains(where: { $0.chat != nil && ($0.result?.verified != true || (batch.destination == .claudeDesktopCode && $0.desktopHandoff == nil)) }) {
                    Button("Retry") { model.retry() }.disabled(model.busy || model.opener.busy)
                }
            }.padding(16)
        }.frame(height: min(440, height)).font(.system(size: 13))
            .onAppear { model.opener.protectPopover = model.protectPopover; if model.projects.isEmpty && model.page == 1 { model.refresh() } }
    }

    private var choose: some View {
        Group {
            Text("1 OF 2").font(.caption).foregroundStyle(.secondary)
            Text("Choose a project").font(.title3).fontWeight(.semibold)
            Picker("From", selection: $model.source) {
                ForEach(ContinuationSurface.sources) { Text($0.title).tag($0) }
            }.onChange(of: model.source) { _, _ in model.query = ""; model.refresh() }.disabled(model.busy)
            TextField("Search projects or chats…", text: $model.query).textFieldStyle(.roundedBorder)
            ForEach(Array(model.filtered.prefix(visibleCount))) { project in
                Button { model.select(project) } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(project.title).fontWeight(.medium)
                            Text("\(project.chats.count) chats · \(project.membershipLabel)").font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        if model.selectedID == project.id { Image(systemName: "checkmark.circle.fill").foregroundStyle(Color.accentColor) }
                    }.padding(10).frame(maxWidth: .infinity, alignment: .leading)
                        .background(model.selectedID == project.id ? Color.accentColor.opacity(0.1) : Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 7))
                }.buttonStyle(.plain).disabled(model.busy)
            }
            if model.filtered.count > visibleCount { Button("Show more") { visibleCount += 100 } }
            if model.filtered.isEmpty && !model.busy {
                Text(model.source == .claudeChat ? "Claude Chat projects aren’t available locally." : "No local projects found.").foregroundStyle(.secondary)
            }
            DisclosureGroup("Advanced") {
                VStack(alignment: .leading, spacing: 10) {
                    Button("Refresh projects") { model.refresh() }
                    Toggle("Include archived chats", isOn: $model.includeArchived)
                        .onChange(of: model.includeArchived) { _, _ in model.regroup() }
                    if let project = model.project {
                        Text(project.workspace.path).font(.caption).textSelection(.enabled)
                        TextField("Search chats…", text: $model.chatQuery).textFieldStyle(.roundedBorder)
                        ForEach(project.chats.filter { model.chatQuery.isEmpty || $0.chat.title.localizedCaseInsensitiveContains(model.chatQuery) }, id: \.chat.id) { entry in
                            Toggle(entry.chat.title, isOn: Binding(get: { model.selectedChats.contains(entry.chat.id) }, set: { value in
                                if value { model.selectedChats.insert(entry.chat.id) } else { model.selectedChats.remove(entry.chat.id) }
                            }))
                        }
                    }
                    Text("Uses app project assignments when available; otherwise groups by folder. Archived chats are excluded by default. Unavailable chats are omitted. Up to 200 chats per copy.").font(.caption).foregroundStyle(.secondary)
                    if !model.recent.isEmpty {
                        Text("Previous copies").fontWeight(.medium)
                        ForEach(model.recent) { batch in
                            Button("\(batch.name) · \(batch.verifiedCount)/\(batch.items.count)") { model.batch = batch; model.page = 3 }
                        }
                    }
                }.disabled(model.busy).padding(.top, 8)
            }
        }
    }
    private var review: some View {
        Group {
            Text("2 OF 2").font(.caption).foregroundStyle(.secondary)
            Text("Clone in…").font(.title3).fontWeight(.semibold)
            Picker("App", selection: $model.destination) {
                ForEach(ContinuationSurface.allCases.filter { $0 != .claudeChat }) { Text($0.title).tag($0) }
            }

            if model.destination == .codexDesktop || model.destination == .claudeDesktopCode {
                Text("Chats are copied separately; desktop project grouping isn’t preserved.").font(.caption).foregroundStyle(.secondary)
            }
            DisclosureGroup("Advanced", isExpanded: $advanced) {
                VStack(alignment: .leading, spacing: 10) {
                    TextField("Name", text: $model.name).textFieldStyle(.roundedBorder)
                    Text("Claude Desktop uses its Code tab. Ordinary Claude Chat cannot import native history.").font(.caption).foregroundStyle(.secondary)
                    Picker("Project folder", selection: $model.mode) {
                        ForEach(ProjectFolderMode.allCases, id: \.self) { Text($0.title).tag($0) }
                    }
                    if model.mode == .shared {
                        Text("Edits affect the source files too.").font(.caption)
                    } else if model.mode == .copy {
                        Text("Copies regular files, including untracked files. Excludes dotfiles, Git history, dependencies, agent instructions and common secret files. Check your source files before copying; filenames cannot identify every credential. Maximum 100 MB.").font(.caption)
                    }
                    if model.mode == .shared, let project = model.project {
                        Text(project.workspace.path).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                    } else {
                        Text("New folders are stored with the copy in Switchboard’s local files.").font(.caption).foregroundStyle(.secondary)
                    }
                    Text("Unavailable content is omitted. Instructions, native attachments, tool execution and running work do not transfer. Chat ordering may differ.").font(.caption).foregroundStyle(.secondary)
                }.padding(.top, 8)
            }
        }.disabled(model.busy)
    }
    private var results: some View {
        Group {
            if let batch = model.batch {
                Text(batch.resultTitle).font(.title3).fontWeight(.semibold)
                ForEach(batch.items) { item in
                    HStack {
                        VStack(alignment: .leading) {
                            Text(item.title).lineLimit(2)
                            if let issue = item.issue { Text(issue).font(.caption).foregroundStyle(.secondary) }
                        }
                        Spacer()
                        if item.result?.verified == true { Button("Open") { model.open(item, openCLI: openCLI) }.disabled(model.busy || model.opener.busy) }
                    }
                }
                DisclosureGroup("Advanced") {
                    VStack(alignment: .leading, spacing: 10) {
                        Text(batch.name)
                        Text(batch.workspace.path).font(.caption).textSelection(.enabled)
                        ForEach(batch.omissions, id: \.self) { Text($0).font(.caption).foregroundStyle(.secondary) }
                        if batch.destination == .claudeDesktopCode {
                            Menu("Open in Terminal") {
                                ForEach(batch.items.filter { $0.result?.verified == true }) { item in
                                    Button(item.title) { model.openTerminal(item) }
                                }
                            }.disabled(model.busy || model.opener.busy)
                        }
                        Menu("Copy context") {
                            ForEach(batch.items) { item in
                                if let chat = item.chat {
                                    Button(item.title) {
                                        let draft = ContinuationDraft(chat: chat, destination: batch.destination)
                                        NSPasteboard.general.clearContents()
                                        if !NSPasteboard.general.setString(draft.context, forType: .string) { model.message = "Could not copy context." }
                                    }
                                }
                            }
                        }
                        Button("Show local files") { NSWorkspace.shared.activateFileViewerSelecting([ProjectCloneEngine.directory(batch, store: model.store)]) }
                        Button("New copy") { model.returnToProjects() }.disabled(model.busy || model.opener.busy)
                    }.padding(.top, 8)
                }
            }
        }
    }
}

private struct ProjectCloneOpeningStatus: View {
    @ObservedObject var model: ContinuationModel
    var body: some View {
        if model.busy { ProgressView("Opening…").controlSize(.small) }
        if let message = model.message { Text(message).font(.caption) }
    }
}

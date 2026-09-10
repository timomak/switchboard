import Cocoa
import SwiftUI
import UniformTypeIdentifiers

@MainActor
final class ContinuationModel: ObservableObject {
    enum Page { case choose, review, preparing, ready }
    @Published var page: Page = .choose
    @Published var source: ContinuationSurface = .codexDesktop
    @Published var query = ""
    @Published var chats: [ContinuationChat] = []
    @Published var selectedID: String?
    @Published var draft: ContinuationDraft?
    @Published var bundle: ContinuationBundle?
    @Published var busy = false
    @Published var message: String?
    @Published var showPaste = false
    @Published var paste = ""
    @Published var preview = false
    @Published var copied = false
    var protectPopover: (Bool) -> Void = { _ in }
    private var work: Task<Void, Never>?
    private var generation = UUID()
    private var loaded = false
    private var libraryReadable = true
    private var imports: [ContinuationChat] = []
    private var localChats: [String: ContinuationLocalChat] = [:]
    private let simulateExternalActions: Bool
    private(set) var store: ContinuationStore?

    init(store: ContinuationStore? = nil, chats: [ContinuationChat] = [], simulateExternalActions: Bool = false) {
        self.simulateExternalActions = simulateExternalActions
        self.store = store; self.chats = chats; self.imports = chats; self.loaded = store != nil
    }

    var filtered: [ContinuationChat] {
        chats.filter { $0.surface == source && (query.isEmpty || $0.title.localizedCaseInsensitiveContains(query)) }
            .sorted { $0.importedAt > $1.importedAt }
    }
    var selected: ContinuationChat? { filtered.first { $0.id == selectedID } }

    func isAvailable(_ chat: ContinuationChat) -> Bool {
        if let local = localChats[chat.id], case .unavailable = local.location { return false }
        return true
    }
    func load() {
        guard !loaded else { refresh(); return }; loaded = true
        // Imported fallbacks are separate from the read-only native catalog.
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
        let root = support.appendingPathComponent("Switchboard Continuations", isDirectory: true)
        store = ContinuationStore(root: root)
        do { imports = try store!.load(); chats = imports } catch { libraryReadable = false; message = "Could not read saved imports. Your library was not changed. Use More options → Show local files to recover it." }
        refresh()
    }
    func refresh() {
        guard !simulateExternalActions else { return }
        cancel(); let token = UUID(); generation = token; busy = true; selectedID = nil; message = nil
        localChats = [:]; chats = imports
        let surface = source
        work = Task { [weak self] in
            let worker = Task.detached(priority: .userInitiated) { try ContinuationDiscovery.catalog(surface: surface) }
            do {
                let entries = try await withTaskCancellationHandler(operation: { try await worker.value }, onCancel: { worker.cancel() })
                guard let self, self.generation == token, !Task.isCancelled else { return }
                self.localChats = Dictionary(entries.map { ($0.chat.id, $0) }, uniquingKeysWith: { first, _ in first })
                self.chats = self.imports + entries.map(\.chat)
            } catch {
                guard let self, self.generation == token, !Task.isCancelled else { return }
                self.message = "Could not read local chats. Try refreshing."
            }
            guard let self, self.generation == token else { return }; self.busy = false
        }
    }
    func chooseFiles() {
        protectPopover(true); defer { protectPopover(false) }
        let panel = NSOpenPanel(); panel.title = "Import conversations"
        panel.allowedContentTypes = [.plainText, .json, UTType(filenameExtension: "jsonl") ?? .data, UTType(filenameExtension: "md") ?? .plainText]
        panel.allowsMultipleSelection = true; panel.canChooseDirectories = false
        panel.message = "Choose exported chats or local session files. Originals stay unchanged."
        guard panel.runModal() == .OK else { return }
        importFiles(panel.urls)
    }
    func importFiles(_ urls: [URL]) {
        guard !busy, !urls.isEmpty, urls.count <= 200 else { return }
        let surface = source; let token = UUID(); generation = token; busy = true; message = nil
        work = Task { [weak self] in
            // One malformed input fails the batch; never silently skip records.
            let worker = Task.detached(priority: .userInitiated) { () throws -> [ContinuationChat] in
                var result: [ContinuationChat] = []; var total = 0
                for url in urls {
                    try Task.checkCancellation()
                    let bytes = try ContinuationFiles.read(url, limit: ContinuationLimits.input - total)
                    total += bytes.count
                    result += try ContinuationParser.parse(bytes, extension: url.pathExtension, surface: surface, name: url.deletingPathExtension().lastPathComponent)
                }
                return result
            }
            do {
                let imported = try await withTaskCancellationHandler(operation: { try await worker.value }, onCancel: { worker.cancel() })
                guard let self, self.generation == token, !Task.isCancelled else { return }
                try self.add(imported)
            } catch {
                guard let self, self.generation == token, !Task.isCancelled else { return }
                self.message = (error as? ContinuationError)?.errorDescription ?? "Could not import these files. Choose a supported export."
            }
            guard let self, self.generation == token else { return }; self.busy = false
        }
    }
    func add(_ values: [ContinuationChat]) throws {
        guard libraryReadable, let store else { throw ContinuationError.storage }
        var merged = Dictionary(imports.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        for chat in values { merged[chat.id] = chat }
        let result = Array(merged.values)
        try store.save(result); imports = result; chats = imports + localChats.values.map(\.chat); selectedID = values.first?.id
    }
    func importPaste() {
        do {
            let result = try ContinuationParser.parse(Data(paste.utf8), extension: "txt", surface: source,
                name: String(paste.trimmingCharacters(in: .whitespacesAndNewlines).prefix(80)))
            try add(result); paste = ""; showPaste = false
        } catch { message = (error as? ContinuationError)?.errorDescription ?? "Could not save pasted text." }
    }
    func forgetSelected() {
        guard let id = selected?.id, let store else { return }
        do { let remaining = imports.filter { $0.id != id }; try store.save(remaining); imports = remaining; chats = imports + localChats.values.map(\.chat); selectedID = nil }
        catch { message = "Could not remove this import. Try again." }
    }
    func review() {
        guard let selected else { return }
        if let entry = localChats[selected.id] {
            let token = UUID(); generation = token; busy = true; message = nil
            work = Task { [weak self] in
                let worker = Task.detached(priority: .userInitiated) { try ContinuationDiscovery.read(entry) }
                do {
                    let chat = try await withTaskCancellationHandler(operation: { try await worker.value }, onCancel: { worker.cancel() })
                    guard let self, self.generation == token, !Task.isCancelled else { return }
                    self.beginReview(chat)
                } catch {
                    guard let self, self.generation == token, !Task.isCancelled else { return }
                    self.message = "Could not read this conversation. It may be unavailable locally, changing, or too large."
                }
                guard let self, self.generation == token else { return }; self.busy = false
            }
        } else { beginReview(selected) }
    }
    private func beginReview(_ selected: ContinuationChat) {
        draft = ContinuationDraft(chat: selected, destination: selected.surface == .claudeChat || selected.surface == .claudeCode ? .codexDesktop : .claudeChat)
        draft?.omissionsReviewed = !(selected.omissions.contains { $0.contains("attachments") || $0.contains("Unsupported") })
        message = nil; copied = false; page = .review
    }
    func addAttachments() {
        protectPopover(true); defer { protectPopover(false) }
        guard draft != nil else { return }
        let panel = NSOpenPanel(); panel.title = "Add files"; panel.allowsMultipleSelection = true; panel.canChooseDirectories = false
        guard panel.runModal() == .OK else { return }
        let urls = panel.urls; let token = UUID(); generation = token; busy = true; message = nil
        work = Task { [weak self] in
            let worker = Task.detached(priority: .userInitiated) { try urls.map { try ContinuationAttachment.select($0) } }
            do {
                let files = try await withTaskCancellationHandler(operation: { try await worker.value }, onCancel: { worker.cancel() })
                guard let self, self.generation == token, !Task.isCancelled else { return }
                var existing = self.draft?.files ?? []
                for file in files where !existing.contains(where: { $0.url == file.url }) { existing.append(file) }
                guard existing.count <= ContinuationLimits.files,
                      existing.reduce(0, { $0 + $1.size }) <= ContinuationLimits.bundle else { throw ContinuationError.tooLarge }
                self.draft?.files = existing
            } catch {
                guard let self, self.generation == token, !Task.isCancelled else { return }
                self.message = (error as? ContinuationError)?.errorDescription ?? "Could not add these files."
            }
            guard let self, self.generation == token else { return }; self.busy = false
        }
    }
    func prepare() {
        guard let draft, let store, !busy else { return }
        do { try draft.validate() } catch { message = (error as? ContinuationError)?.errorDescription; return }
        let token = UUID(); generation = token; busy = true; page = .preparing; message = nil
        work = Task { [weak self] in
            let worker = Task.detached(priority: .userInitiated) { try store.prepare(draft, id: token) }
            do {
                let bundle = try await withTaskCancellationHandler(operation: { try await worker.value }, onCancel: { worker.cancel() })
                guard let self, self.generation == token, !Task.isCancelled else { try? store.removeBundle(bundle); return }
                self.bundle = bundle; self.page = .ready
            } catch {
                guard let self, self.generation == token, !Task.isCancelled else { return }
                self.message = (error as? ContinuationError)?.errorDescription ?? "Could not prepare the handoff. Try again."
                self.page = .review
            }
            guard let self, self.generation == token else { return }; self.busy = false
        }
    }
    func cancel() {
        generation = UUID(); work?.cancel(); work = nil; busy = false
        if page == .preparing { page = .review }
    }
    func finish() {
        cancel()
        // Successful bundles remain usable after opening the destination. The
        // user can remove them explicitly; never expire a referenced file mid-chat.
        bundle = nil; draft = nil; copied = false; message = nil; page = .choose
    }
    func copyContext() {
        if simulateExternalActions { copied = true; return }
        guard let bundle else { return }
        NSPasteboard.general.clearContents()
        copied = NSPasteboard.general.setString(bundle.context, forType: .string)
        if !copied { message = "Could not copy. Try again." }
    }
    func openDestination(openCLI: (String, String?) -> Void) {
        if simulateExternalActions { message = "Fixture: destination opening not executed."; return }
        guard let bundle else { return }
        switch bundle.receipt.destination {
        case .claudeCode: openCLI("claude", draft?.workspace?.path)
        case .codexCLI: openCLI("codex", draft?.workspace?.path)
        case .claudeChat, .codexDesktop:
            let ids = bundle.receipt.destination == .claudeChat ? ["com.anthropic.claudefordesktop"] : ["com.openai.codex"]
            // Ask Launch Services for installed apps; never guess a private
            // create-thread URL, install an app, or change its account.
            let app = ids.compactMap { NSWorkspace.shared.urlForApplication(withBundleIdentifier: $0) }.first
            guard let app else { message = "Install \(bundle.receipt.destination.title) to open it."; return }
            NSWorkspace.shared.openApplication(at: app, configuration: NSWorkspace.OpenConfiguration()) { [weak self] _, error in
                Task { @MainActor in if error != nil { self?.message = "Could not open the app. Open it from Applications." } }
            }
        }
    }
}

struct ContinuationView: View {
    @ObservedObject var model: ContinuationModel
    let height: CGFloat
    let close: () -> Void
    let openCLI: (String, String?) -> Void
    @State private var advanced = false
    @State private var more = false
    @State private var showSummary = false
    @State private var visibleCount = 100

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button { back() } label: { Image(systemName: "chevron.left") }.buttonStyle(.plain).help("Back")
                Text("Continue in another app").fontWeight(.medium)
                Spacer()
                Image(systemName: "arrow.left.arrow.right").foregroundStyle(.secondary)
            }.padding(16)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    if model.page == .choose { choose }
                    if model.page == .review { review }
                    if model.page == .preparing {
                        ProgressView("Preparing…").frame(maxWidth: .infinity).padding(.top, 40)
                    }
                    if model.page == .ready { ready }
                    if let message = model.message {
                        HStack(alignment: .top) {
                            Text(message).font(.caption).textSelection(.enabled)
                            Spacer()
                            Button { model.message = nil } label: { Image(systemName: "xmark") }.help("Dismiss")
                        }
                    }
                }.padding(16)
            }
            Divider()
            HStack {
                if model.busy { Button("Cancel") { model.cancel() } }
                else { Button(model.page == .ready ? "Done" : "Cancel") { model.finish(); close() } }
                Spacer()
                if model.page == .choose {
                    Button("Next") { model.review(); advanced = false }.disabled(model.selected == nil || model.busy).keyboardShortcut(.defaultAction)
                } else if model.page == .review {
                    Button("Prepare") { model.prepare() }.disabled(!canPrepare || model.busy).keyboardShortcut(.defaultAction)
                } else if model.page == .ready, let bundle = model.bundle {
                    Button("Open \(bundle.receipt.destination.title)") { model.openDestination(openCLI: openCLI) }.keyboardShortcut(.defaultAction)
                }
            }.padding(12)
        }.font(.system(size: 13)).frame(height: min(440, height))
            .onAppear { model.load() }
            .sheet(isPresented: $model.showPaste) { pasteSheet }
            .sheet(isPresented: $model.preview) {
                VStack(alignment: .leading) {
                    Text("Transcript").font(.headline)
                    ScrollView { Text(model.draft?.context ?? "").font(.system(.body, design: .monospaced)).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading) }
                    Button("Done") { model.preview = false }.keyboardShortcut(.defaultAction)
                }.padding(20).frame(width: 600, height: 450)
            }
    }
    private var choose: some View {
        Group {
            Text("1 OF 2").font(.caption).foregroundStyle(.secondary)
            Text("Choose a conversation").font(.title3).fontWeight(.semibold)
            Picker("From", selection: $model.source) {
                ForEach(ContinuationSurface.allCases) { Text($0.title).tag($0) }
            }.onChange(of: model.source) { _, _ in model.selectedID = nil; model.query = ""; model.refresh() }
                .disabled(model.busy)
            TextField("Search chats…", text: $model.query).textFieldStyle(.roundedBorder).accessibilityLabel("Search chats")
            if model.busy { ProgressView().controlSize(.small) }
            else if model.filtered.isEmpty {
                Text(model.source == .claudeChat && model.query.isEmpty ? "Claude Chat isn’t available locally. Use More options." : "No chats found.").foregroundStyle(.secondary)
            }
            ForEach(Array(model.filtered.prefix(visibleCount))) { chat in
                Button { model.selectedID = chat.id } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(chat.title).fontWeight(.medium).lineLimit(1)
                            if model.isAvailable(chat) {
                                Text(chat.importedAt, style: .date).font(.caption).foregroundStyle(.secondary)
                            } else {
                                Text("Not stored locally").font(.caption).foregroundStyle(.secondary)
                            }
                        }
                        Spacer()
                        if model.selectedID == chat.id { Image(systemName: "checkmark.circle.fill").foregroundStyle(Color.accentColor) }
                    }.padding(10).frame(maxWidth: .infinity, alignment: .leading)
                        .background(model.selectedID == chat.id ? Color.accentColor.opacity(0.1) : Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 7))
                }.buttonStyle(.plain).disabled(model.busy || !model.isAvailable(chat))
            }
            if model.filtered.count > visibleCount {
                Button("Show more") { visibleCount += 100 }
            }
            DisclosureGroup("More options", isExpanded: $more) {
                VStack(alignment: .leading, spacing: 10) {
                    Button("Refresh chats") { model.refresh() }
                    HStack {
                        Button("Choose export…") { model.chooseFiles() }
                        Button("Paste text…") { model.showPaste = true }
                    }
                    Text(model.source == .claudeChat ? "Claude: Settings → Privacy → Export data. Choose conversations.json from the downloaded export." : "Choose local session JSONL or an exported text file.")
                        .font(.caption).foregroundStyle(.secondary)
                    if let selected = model.selected, !selected.id.hasPrefix("local:") { Button("Remove imported copy") { model.forgetSelected() } }
                    Button("Show local files…") {
                        if let root = model.store?.root { NSWorkspace.shared.activateFileViewerSelecting([root]) }
                    }
                }.padding(.top, 8).disabled(model.busy)
            }
        }
    }
    private var canPrepare: Bool {
        guard let draft = model.draft else { return false }
        return (try? draft.validate()) != nil
    }
    private func binding<Value>(_ key: WritableKeyPath<ContinuationDraft, Value>, fallback: Value) -> Binding<Value> {
        Binding(get: { model.draft?[keyPath: key] ?? fallback }, set: { model.draft?[keyPath: key] = $0 })
    }
    private var review: some View {
        Group {
            Text("2 OF 2").font(.caption).foregroundStyle(.secondary)
            Text("Continue in…").font(.title3).fontWeight(.semibold)
            if let draft = model.draft {
                VStack(alignment: .leading, spacing: 4) {
                    Text(draft.chat.title).fontWeight(.medium).lineLimit(2)
                    Text("\(draft.chat.messages.count - draft.firstMessage) messages · \(draft.files.count) files").font(.caption).foregroundStyle(.secondary)
                }
                Picker("App", selection: binding(\.destination, fallback: .codexDesktop)) {
                    ForEach(ContinuationSurface.allCases) { Text($0.title).tag($0) }
                }
                if draft.context.utf8.count > ContinuationLimits.inlineContext {
                    Text("Too much context. Choose a later starting message under Advanced.").font(.caption)
                }
                if draft.omissions.contains(where: { $0.contains("attachments") || $0.contains("Unsupported") }) {
                    Toggle("Continue with omitted content", isOn: binding(\.omissionsReviewed, fallback: false))
                    DisclosureGroup("\(draft.omissions.count) omissions") {
                        ForEach(draft.omissions, id: \.self) { Text($0).font(.caption).foregroundStyle(.secondary) }
                    }
                }
                DisclosureGroup("Advanced", isExpanded: $advanced) {
                    VStack(alignment: .leading, spacing: 10) {
                        HStack {
                            Text(draft.workspace?.lastPathComponent ?? "No project folder").lineLimit(1)
                            Spacer()
                            Button("Choose…") {
                                model.protectPopover(true); defer { model.protectPopover(false) }
                                let panel = NSOpenPanel(); panel.canChooseDirectories = true; panel.canChooseFiles = false
                                if panel.runModal() == .OK { model.draft?.workspace = panel.url }
                            }
                        }
                        Stepper("Start at message \(draft.firstMessage + 1)", value: binding(\.firstMessage, fallback: 0), in: 0...max(0, draft.chat.messages.count - 1))
                        Toggle("Add summary", isOn: $showSummary)
                            .onChange(of: showSummary) { _, value in if !value { model.draft?.summary = "" } }
                        if showSummary { TextField("Summary", text: binding(\.summary, fallback: ""), axis: .vertical).lineLimit(2...5) }
                        TextField("Next step", text: binding(\.nextStep, fallback: ""), axis: .vertical).lineLimit(2...5)
                        Button("Add files…") { model.addAttachments() }
                        ForEach(draft.files) { file in
                            HStack {
                                Text(file.url.lastPathComponent).lineLimit(1)
                                Spacer()
                                Button { model.draft?.files.removeAll { $0.id == file.id } } label: { Image(systemName: "minus.circle") }.help("Remove file")
                            }
                        }
                        Button("Preview transcript") { model.preview = true }
                        ForEach(draft.omissions, id: \.self) { Text($0).font(.caption).foregroundStyle(.secondary) }
                    }.padding(.top, 8)
                }
            }
        }.disabled(model.busy)
    }
    private var ready: some View {
        Group {
            Image(systemName: "checkmark.circle.fill").font(.largeTitle).foregroundStyle(.green).frame(maxWidth: .infinity)
            Text("Ready to continue").font(.title3).fontWeight(.semibold).frame(maxWidth: .infinity)
            Text(model.draft?.chat.title ?? "").foregroundStyle(.secondary).lineLimit(2)
            Button(model.copied ? "Copied" : "Copy context") { model.copyContext() }
            if let bundle = model.bundle {
                DisclosureGroup("Details") {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(bundle.receipt.files, id: \.self) { Text($0).font(.caption) }
                        Button("Show files") { NSWorkspace.shared.activateFileViewerSelecting([bundle.directory]) }
                        ForEach(bundle.receipt.omissions, id: \.self) { Text($0).font(.caption).foregroundStyle(.secondary) }
                        Button("Delete prepared files") {
                            do { try model.store?.removeBundle(bundle); model.finish() }
                            catch { model.message = "Could not delete prepared files." }
                        }
                    }
                }
            }
        }
    }
    private var pasteSheet: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Paste conversation").font(.headline)
            TextEditor(text: $model.paste).font(.system(.body, design: .monospaced)).accessibilityLabel("Conversation text")
            HStack {
                Button("Cancel") { model.showPaste = false }
                Spacer()
                Button("Import") { model.importPaste() }.disabled(model.paste.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty).keyboardShortcut(.defaultAction)
            }
            if let message = model.message { Text(message).font(.caption) }
        }.padding(20).frame(width: 560, height: 350)
    }
    private func back() {
        model.cancel()
        switch model.page {
        case .choose: close()
        case .review: model.page = .choose
        case .preparing: model.page = .review
        case .ready: model.finish()
        }
    }
}

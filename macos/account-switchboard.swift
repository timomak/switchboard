import Cocoa
import SwiftUI

private enum BoardProviderIcons {
    static let images: [String: NSImage] = {
        var images: [String: NSImage] = [:]
        for name in ["claude", "codex"] {
            let bundled = Bundle.main.resourceURL?.appendingPathComponent("provider-icons/\(name).png")
            let development = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
                .appendingPathComponent("assets/provider-icons/\(name).png")
            if let image = bundled.flatMap({ NSImage(contentsOf: $0) }) ?? NSImage(contentsOf: development) {
                images[name] = image
            }
        }
        return images
    }()
}

enum BoardAction {
    case claudeConnection(Bool, Bool)
    case claudeDesktop(String), claudeCLI(String), codexDesktop(String), codexCLI(String)
    case addClaude(Bool), saveClaude, addCodex(Bool), saveCodex, codexMode(Bool)
    case openContinuationCLI(String, String?)
    case openCLI(String), recoverCodex(Bool), verifyCloud(String), useConnection(String), recoverConnection, addConnection, refresh, preferences, quit
}

struct BoardMenuEntry {
    let title: String
    var enabled = true
    var action: BoardAction? = nil
}

/// AppKit's popup button preserves the native bordered control and trailing
/// disclosure arrow; SwiftUI's macOS Menu discards the custom label layout.
struct BoardAccountMenu: NSViewRepresentable {
    let title: String
    let entries: [BoardMenuEntry]
    let enabled: Bool
    let action: (BoardAction) -> Void
    func makeCoordinator() -> Coordinator { Coordinator() }
    func makeNSView(context: Context) -> NSPopUpButton {
        let button = NSPopUpButton(frame: .zero, pullsDown: true)
        button.bezelStyle = .rounded
        button.font = .systemFont(ofSize: 12)
        button.setContentHuggingPriority(.defaultLow, for: .horizontal)
        button.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return button
    }
    func updateNSView(_ button: NSPopUpButton, context: Context) {
        context.coordinator.entries = entries; context.coordinator.action = action
        button.removeAllItems(); button.addItem(withTitle: title)
        button.menu?.autoenablesItems = false
        for (index, entry) in entries.enumerated() {
            if entry.title.isEmpty { button.menu?.addItem(.separator()); continue }
            let item = NSMenuItem(title: entry.title, action: #selector(Coordinator.pick(_:)), keyEquivalent: "")
            item.target = context.coordinator; item.tag = index; item.isEnabled = entry.enabled
            button.menu?.addItem(item)
        }
        button.isEnabled = enabled
        button.setAccessibilityLabel(title)
    }
    final class Coordinator: NSObject {
        var entries: [BoardMenuEntry] = []
        var action: (BoardAction) -> Void = { _ in }
        @objc func pick(_ item: NSMenuItem) {
            guard entries.indices.contains(item.tag), let value = entries[item.tag].action else { return }
            action(value)
        }
    }
}

final class SwitchboardModel: ObservableObject {
    @Published var viewport = NSSize(width: 420, height: 600)
    @Published var status: AccountStatus?
    @Published var snapshots: [String: Snapshot] = [:]
    @Published var busy = false
    @Published var refreshing = false
    @Published var message: String?
    var action: (BoardAction) -> Void = { _ in }
    var protectPopover: (Bool) -> Void = { _ in }
    var measuredHeight: CGFloat = 600
    var resize: (NSSize) -> Void = { _ in }
}

struct BoardUsageRequest {
    let key: String
    let vendor: String
    let args: [String]
    var home: String? = nil
}

/// Identity-bound request keys prevent a late usage response from being shown
/// underneath a different selected account. Provider metrics still come from Rust.
func boardUsageRequests(_ s: AccountStatus) -> [BoardUsageRequest] {
    var requests: [BoardUsageRequest] = []
    if !s.claudeConnectionDesktop, let label = s.desktopActive {
        requests.append(BoardUsageRequest(key: "claude:desktop:\(label)", vendor: "anthropic",
            args: ["--vendor", "anthropic", "--account", label, "--desktop"]))
    }
    if !s.claudeConnectionCLI, let label = s.cliActive {
        requests.append(BoardUsageRequest(key: "claude:cli:\(label)", vendor: "anthropic",
            args: ["--vendor", "anthropic", "--account", label]))
    }
    if s.cloudActive == nil && !s.connectionRecovery, let label = s.codexActive {
        requests.append(BoardUsageRequest(key: "codex:desktop:\(label)", vendor: "openai", args: ["--vendor", "openai"]))
    }
    if s.codexCLIMode == "separate", let label = s.codexCLIActive, let home = s.codexCLIHome {
        requests.append(BoardUsageRequest(key: "codex:cli:\(label)", vendor: "openai", args: ["--vendor", "openai"], home: home))
    }
    return requests
}

func boardTerminalScript(binary: String, provider: String, directory: String? = nil) -> String {
    func quote(_ s: String) -> String { "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'" }
    return "#!/bin/bash\nset -e\n" + (directory.map { "cd -- " + quote($0) + "\n" } ?? "") + quote(binary) + " cli launch " + quote(provider) + "\n"
}

/// Reserve space for the popover arrow and screen edges. Size before showing:
/// allowing the hosting view to grow after anchoring can push its top off-screen.
func boardViewport(visibleFrame: NSRect) -> NSSize {
    NSSize(width: min(420, max(1, visibleFrame.width - 32)),
           height: min(600, max(1, visibleFrame.height - 40)))
}

struct AccountSwitchboard: View {
    @ObservedObject var model: SwitchboardModel
    @StateObject private var continuation = ContinuationModel()
    @StateObject private var projectClone = ProjectCloneModel()
    @State private var page = "accounts"
    @State private var contentHeight: CGFloat = 600
    @AppStorage("boardWeekly") private var weekly = true
    @AppStorage("boardIconOnly") private var iconOnly = true
    private var status: AccountStatus { model.status ?? AccountStatus() }

    var body: some View {
        if page == "continue" {
            ContinuationView(model: continuation, height: model.viewport.height,
                close: { page = "accounts" }, openCLI: { model.action(.openContinuationCLI($0, $1)) })
                .frame(width: model.viewport.width)
                .onAppear {
                    continuation.protectPopover = model.protectPopover
                    let height = min(440, model.viewport.height)
                    model.measuredHeight = height
                    model.resize(NSSize(width: model.viewport.width, height: height))
                }
        } else if page == "clone-project" {
            ProjectCloneView(model: projectClone, height: model.viewport.height,
                close: { page = "accounts" }, openCLI: { model.action(.openContinuationCLI($0, $1)) })
                .frame(width: model.viewport.width)
                .onAppear {
                    projectClone.protectPopover = model.protectPopover
                    let height = min(440, model.viewport.height)
                    model.measuredHeight = height
                    model.resize(NSSize(width: model.viewport.width, height: height))
                }
        } else { accountBody }
    }

    private var accountBody: some View {
        ScrollView(.vertical) {
        VStack(spacing: 0) {
            HStack {
                if page != "accounts" { Button { page = "accounts" } label: { Image(systemName: "chevron.left") }.buttonStyle(.plain).help("Back to accounts") }
                Text(page == "accounts" ? "Switchboard" : page == "manage" ? "Manage accounts" : "Settings").fontWeight(.medium)
                Spacer()
                if model.busy { ProgressView().controlSize(.small); Text("Working…").foregroundStyle(.secondary).font(.caption) }
                Button { model.action(.refresh) } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.plain).disabled(model.refreshing || model.busy).help("Refresh accounts and usage")
            }.padding(.horizontal, 16).padding(.vertical, 13)
            Divider()
            if page == "accounts" {
                provider("claude")
                Divider()
                provider("codex")
                Divider()
                Button { page = "continue" } label: {
                    Label("Continue in another app…", systemImage: "arrow.left.arrow.right")
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 16).padding(.vertical, 12)
                }.buttonStyle(.plain).foregroundStyle(Color.accentColor)
                    .disabled(model.busy || SWITCHBOARD_PREVIEW)
                Button { page = "clone-project" } label: {
                    Label("Clone project…", systemImage: "square.on.square")
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 16).padding(.vertical, 12)
                }.buttonStyle(.plain).foregroundStyle(Color.accentColor)
                    .disabled(model.busy || SWITCHBOARD_PREVIEW)
            } else if page == "manage" {
                management
            } else {
                VStack(alignment: .leading, spacing: 16) {
                    Button("Manage accounts…") { page = "manage" }
                    Divider()
                    Toggle("Compact menu-bar icon", isOn: $iconOnly)
                    Toggle("Show Claude weekly usage", isOn: $weekly)
                    Button("More preferences…") { model.action(.preferences) }
                }.padding(16)
            }
            if let message = model.message {
                Divider()
                HStack(alignment: .top) {
                    Text(message).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 4)
                    Button { model.message = nil } label: { Image(systemName: "xmark") }.buttonStyle(.plain).help("Dismiss")
                }.padding(12)
            }
            Divider()
            HStack {
                Button { page = page == "settings" ? "accounts" : "settings" } label: { Image(systemName: "slider.horizontal.3") }.help("Settings").accessibilityLabel("Settings")
                Spacer()
                Button { model.action(.quit) } label: { Image(systemName: "power") }.help("Quit Switchboard").accessibilityLabel("Quit Switchboard")
            }.buttonStyle(.plain).foregroundStyle(.secondary).padding(.horizontal, 16).padding(.vertical, 12)
        }
        .font(.system(size: 13)).frame(width: model.viewport.width)
        .fixedSize(horizontal: false, vertical: true)
        .onGeometryChange(for: CGFloat.self) { geometry in geometry.size.height } action: { height in
            guard height > 0 else { return }
            contentHeight = height
            model.measuredHeight = height
            model.resize(NSSize(width: model.viewport.width, height: min(height, model.viewport.height)))
        }
        }
        .frame(width: model.viewport.width, height: min(contentHeight, model.viewport.height))
        .background(Color(nsColor: .windowBackgroundColor))
    }

    private func provider(_ p: String) -> some View {
        let claude = p == "claude"
        let shared = !claude && status.codexCLIMode == "shared"
        return VStack(alignment: .leading, spacing: 10) {
            HStack {
                Image(nsImage: BoardProviderIcons.images[p] ?? NSImage())
                    .resizable().scaledToFit().frame(width: 25, height: 25)
                    .clipShape(RoundedRectangle(cornerRadius: 6)).accessibilityHidden(true)
                Text(claude ? "Claude" : "Codex").font(.system(size: 15, weight: .medium))
                Spacer()
            }
            target(p, cli: false)
            target(p, cli: true)
            if !claude, let issue = status.codexError ?? status.codexCLIError {
                Text(issue).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            if !claude && (status.codexRecoveryRequired || status.codexCLIRecovery) {
                Button("Recover interrupted \(status.codexRecoveryRequired ? "Desktop" : "CLI") switch…") {
                    model.action(.recoverCodex(!status.codexRecoveryRequired))
                }.disabled(model.busy)
            }
            let label = claude ? (status.claudeConnectionDesktop ? status.claudeConnectionLabel : status.desktopActive) : (status.cloudActive ?? status.codexActive)
            let key = "\(p):desktop:\(label ?? "")"
            Divider().padding(.top, 1)
            Text("\(shared ? "Desktop + CLI" : "Desktop") · \(label ?? "No saved account")")
                .font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
            if claude && status.claudeConnectionDesktop {
                Text("Bedrock billing · Separate local workspace").foregroundStyle(.secondary).font(.caption)
            } else if !claude && (status.cloudActive != nil || status.connectionRecovery) {
                Text(status.connectionRecovery ? "Connection recovery required" : "Usage billed by your connection").foregroundStyle(.secondary).font(.caption)
            } else if let snapshot = model.snapshots[key] {
                HStack(alignment: .top, spacing: 16) {
                    if claude { meter("5-hour", snapshot.session) }
                    if !claude || weekly { meter("Weekly", snapshot.weekly) }
                }
            } else {
                Text(model.refreshing ? "Loading usage…" : "Usage unavailable").foregroundStyle(.secondary).font(.caption)
            }
            let cliLabel = claude ? (status.claudeConnectionCLI ? status.claudeConnectionLabel : status.cliActive) : status.codexCLIActive
            if !shared, let cliLabel {
                HStack {
                    Text("CLI · \(cliLabel)").lineLimit(1).truncationMode(.middle)
                    Spacer()
                    let snapshot = model.snapshots["\(p):cli:\(cliLabel)"]
                    if let window = claude ? snapshot?.session : snapshot?.weekly { Text("\(window.pct)% used · \(claude ? "5h" : "weekly")").monospacedDigit() }
                    else { Text("Usage unavailable") }
                }.font(.system(size: 11)).foregroundStyle(.secondary)
            }
        }.padding(16)
    }

    private func meter(_ title: String, _ window: Window?) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack { Text(title); Spacer(); Text(window.map { "\($0.pct)% used" } ?? "—").monospacedDigit() }.font(.caption)
            GeometryReader { geometry in
                ZStack(alignment: .leading) {
                    Capsule().fill(Color.primary.opacity(0.07))
                    Capsule().fill(Color.accentColor).frame(width: geometry.size.width * Double(window?.pct ?? 0) / 100)
                }
            }.frame(height: 4).accessibilityLabel("\(title): \(window?.pct ?? 0) percent used")
            Text(window.map { "Resets \($0.reset)" } ?? "No reset available").font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
        }.frame(maxWidth: .infinity)
    }

    private func target(_ p: String, cli: Bool) -> some View {
        let claude = p == "claude"
        let shared = !claude && cli && status.codexCLIMode == "shared"
        let label = claude ? ((cli ? status.claudeConnectionCLI : status.claudeConnectionDesktop) ? status.claudeConnectionLabel : (cli ? status.cliActive : status.desktopActive)) : (cli ? status.codexCLIActive : (status.cloudActive ?? status.codexActive))
        let setup = claude && cli && label == nil
        let labels = claude ? (cli ? status.cliLabels : status.desktopLabels) : (cli ? status.codexCLILabels : status.codexLabels)
        let title = model.status == nil ? "Loading…" : shared ? "Shared with Desktop" : label ?? (setup && status.cliHasIdentity ? "Existing CLI login" : "Set up account")
        return HStack(spacing: 8) {
            Label(cli ? "CLI" : "Desktop", systemImage: cli ? "terminal" : "desktopcomputer")
                .font(.caption).foregroundStyle(.secondary).frame(width: 70, alignment: .leading)
            if setup && status.cliHasIdentity && status.claudeConnectionLabel == nil {
                Button { model.action(.saveClaude) } label: {
                    HStack { Text("Existing CLI login"); Spacer(); Text("Set up").foregroundStyle(.secondary) }
                        .font(.caption).padding(.horizontal, 9).padding(.vertical, 7)
                        .background(Color(nsColor: .controlBackgroundColor))
                        .overlay(RoundedRectangle(cornerRadius: 7).stroke(Color.primary.opacity(0.15), style: StrokeStyle(lineWidth: 0.5, dash: [3, 2])))
                }.buttonStyle(.plain).disabled(model.busy || model.refreshing)
            } else {
                BoardAccountMenu(title: title, entries: menuEntries(p, cli: cli, label: label, labels: labels, shared: shared),
                    enabled: !model.busy && !model.refreshing && model.status != nil && (claude || (!(cli ? status.codexCLIRecovery : status.codexRecoveryRequired) && (cli ? status.codexCLIError : status.codexError) == nil)),
                    action: model.action).frame(maxWidth: .infinity).frame(height: 32)
            }
            if cli {
                Button { model.action(.openCLI(p)) } label: { Image(systemName: "arrow.up.forward.square") }
                    .buttonStyle(.plain).foregroundStyle(Color.accentColor).help("Open \(claude ? "Claude" : "Codex") CLI")
                    .disabled(model.busy || model.status == nil)
            }
        }
    }

    private func menuEntries(_ p: String, cli: Bool, label: String?, labels: [String], shared: Bool) -> [BoardMenuEntry] {
        let claude = p == "claude"
        var entries: [BoardMenuEntry] = []
        if claude, let issue = status.claudeConnectionError { entries.append(BoardMenuEntry(title: issue, enabled: false)) }
        if !claude {
            entries += (cli ? status.codexCLIProfileProblems : status.codexProfileProblems).map { BoardMenuEntry(title: $0, enabled: false) }
        }
        if claude, let connection = status.claudeConnectionLabel {
            let selected = cli ? status.claudeConnectionCLI : status.claudeConnectionDesktop
            entries.append(BoardMenuEntry(title: selected ? "✓ \(connection)" : connection, enabled: !selected, action: .claudeConnection(!cli, false)))
            if selected { entries.append(BoardMenuEntry(title: "Subscription · \((cli ? status.cliActive : status.desktopActive) ?? "saved login")", action: .claudeConnection(!cli, true))) }
            entries.append(BoardMenuEntry(title: ""))
        }
        if !claude && cli {
            entries += [BoardMenuEntry(title: shared ? "✓ Shared with Desktop" : "Shared with Desktop", action: .codexMode(false)),
                BoardMenuEntry(title: !shared ? "✓ Separate CLI account" : "Separate CLI account…", action: .codexMode(true)), BoardMenuEntry(title: "")]
        }
        if !claude && !cli && !status.cloudProfiles.isEmpty {
            entries += status.cloudProfiles.map { BoardMenuEntry(title: $0.label == status.cloudActive ? "✓ \($0.label)" : $0.label, enabled: !status.connectionRecovery && $0.label != status.cloudActive, action: .useConnection($0.label)) }
            if status.connectionRecovery { entries.append(BoardMenuEntry(title: "Recover connection…", action: .recoverConnection)) }
            entries.append(BoardMenuEntry(title: ""))
        }
        entries += labels.map { item in BoardMenuEntry(title: item == label ? "✓ \(item)" : item,
            enabled: item != label && !shared && (claude ? !(cli ? status.claudeConnectionCLI : status.claudeConnectionDesktop) : (cli || !status.connectionRecovery)),
            action: claude ? (cli ? .claudeCLI(item) : .claudeDesktop(item)) : (cli ? .codexCLI(item) : .codexDesktop(item))) }
        if !claude && !cli && status.codexHasLogin && label == nil { entries.append(BoardMenuEntry(title: "Save current account…", action: .saveCodex)) }
        entries += [BoardMenuEntry(title: ""), BoardMenuEntry(title: "Add \(cli ? "CLI " : "")account…", action: claude ? .addClaude(!cli) : .addCodex(cli))]
        return entries
    }

    private var management: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Claude").fontWeight(.medium)
            if let issue = status.claudeConnectionError { Text(issue).font(.caption).foregroundStyle(.secondary) }
            Text("Desktop: \(status.desktopLabels.joined(separator: ", "))").font(.caption).foregroundStyle(.secondary)
            HStack { Button("Add Desktop account…") { model.action(.addClaude(true)) }; Button("Add CLI account…") { model.action(.addClaude(false)) } }
            if status.cliActive == nil && status.cliHasIdentity { Button("Save current CLI login…") { model.action(.saveClaude) } }
            Divider()
            Text("Codex").fontWeight(.medium)
            ForEach(Array((status.codexProfileProblems + status.codexCLIProfileProblems).enumerated()), id: \.offset) { _, issue in Text(issue).font(.caption).foregroundStyle(.secondary) }
            Text("Desktop: \(status.codexLabels.joined(separator: ", "))").font(.caption).foregroundStyle(.secondary)
            HStack { Button("Add Desktop account…") { model.action(.addCodex(false)) }; Button("Add CLI account…") { model.action(.addCodex(true)) } }
            if status.codexHasLogin && status.codexActive == nil { Button("Save current account…") { model.action(.saveCodex) } }
            if let issue = status.cloudError { Text(issue).font(.caption).foregroundStyle(.secondary) }
            if !status.connectionTemplates.isEmpty || !status.cloudProfiles.isEmpty {
                Divider()
                Text("Connections").fontWeight(.medium)
                Text("Choose a connection for Codex. Your local workspace stays in place.")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                Button("Add connection…") { model.action(.addConnection) }
                    .disabled(status.connectionTemplates.isEmpty)
                if status.connectionRecovery { Button("Recover connection…") { model.action(.recoverConnection) } }
                ForEach(status.cloudProfiles, id: \.label) { profile in
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Text(profile.label).fontWeight(.medium)
                            Spacer()
                            if profile.label == status.cloudActive { Text("Selected").font(.caption).foregroundStyle(.secondary) }
                        }
                        Text(profile.provider).font(.caption).foregroundStyle(.secondary)
                        HStack {
                            Button("Use connection…") { model.action(.useConnection(profile.label)) }.disabled(status.connectionRecovery || profile.label == status.cloudActive)
                            Button("Check configuration…") { model.action(.verifyCloud(profile.label)) }
                        }
                    }
                }
            }
        }.padding(16).disabled(model.busy || model.refreshing || model.status == nil)
    }
}

extension AppDelegate {
    func configureSwitchboard() {
        statusItem.menu = nil
        statusItem.button?.target = self
        statusItem.button?.action = #selector(toggleSwitchboard)
        board.action = { [weak self] in self?.performBoardAction($0) }
        board.resize = { [weak self] size in
            DispatchQueue.main.async {
                guard let self else { return }
                self.sizeSwitchboard(size)
            }
        }
        board.protectPopover = { [weak self] protected in
            self?.boardPopover.behavior = protected ? .applicationDefined : .transient
        }
        boardPopover.behavior = .transient
        boardPopover.animates = false
        let controller = NSHostingController(rootView: AccountSwitchboard(model: board))
        controller.sizingOptions = []
        boardPopover.contentViewController = controller
        sizeSwitchboard(board.viewport)
        NotificationCenter.default.addObserver(self, selector: #selector(boardScreenChanged),
            name: NSApplication.didChangeScreenParametersNotification, object: nil)
        updateBoardIcon()
    }

    private func sizeSwitchboard(_ size: NSSize) {
        // fittingSize is not meaningful when hosting sizingOptions is disabled.
        // Explicitly keep both the hosting view and popover at the same size,
        // including on subsequent openings. Never resize the private popover window.
        guard size.width > 0, size.height > 0 else { return }
        boardPopover.contentViewController?.view.setFrameSize(size)
        boardPopover.contentSize = size
        boardPopover.contentViewController?.view.layoutSubtreeIfNeeded()
    }

    @objc func boardScreenChanged() {
        // The menu-bar anchor can move when a display disconnects. Reopening
        // resolves the new screen rather than retaining the old coordinates.
        boardPopover.performClose(nil)
    }

    @objc func toggleSwitchboard() {
        if boardPopover.isShown { boardPopover.performClose(nil); return }
        guard let button = statusItem.button else { return }
        guard let screen = button.window?.screen ?? NSScreen.main else { return }
        let size = boardViewport(visibleFrame: screen.visibleFrame)
        board.viewport = size
        sizeSwitchboard(NSSize(width: size.width, height: min(board.measuredHeight, size.height)))
        boardPopover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        boardPopover.contentViewController?.view.window?.makeKey()
        refreshSwitchboard()
    }

    func updateBoardIcon() {
        guard let button = statusItem.button else { return }
        button.image = Bundle.main.url(forResource: "Switchboard-menubar", withExtension: "pdf").flatMap { NSImage(contentsOf: $0) }
        button.image?.size = NSSize(width: 22, height: 22)
        button.image?.isTemplate = true
        let compact = DEF.bool(forKey: "boardIconOnly")
        statusItem.length = compact ? NSStatusItem.squareLength : NSStatusItem.variableLength
        button.imagePosition = compact ? .imageOnly : .imageLeading
        button.imageScaling = .scaleProportionallyDown
        button.attributedTitle = NSAttributedString(string: compact ? "" : "  Switchboard")
        button.toolTip = SWITCHBOARD_PREVIEW ? "Switchboard preview · Claude and Codex accounts" : "Switchboard · Claude and Codex accounts"
    }

    func refreshSwitchboard() {
        guard !board.refreshing, !accountSwitchInFlight, let binary = resolveBinary("ai-usagebar") else { return }
        board.refreshing = true
        boardGeneration += 1
        let generation = boardGeneration
        DispatchQueue.global(qos: .utility).async { [weak self] in
            func read(_ args: [String], home: String? = nil) -> Data? {
                let p = Process(); p.executableURL = URL(fileURLWithPath: binary); p.arguments = args
                if let home {
                    var env = ProcessInfo.processInfo.environment; env["CODEX_HOME"] = home
                    p.environment = env
                }
                let pipe = Pipe(); p.standardOutput = pipe; p.standardError = FileHandle.nullDevice
                let watchdog = DispatchWorkItem { if p.isRunning { p.terminate() } }
                DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + REFRESH_TIMEOUT, execute: watchdog)
                defer { watchdog.cancel() }
                do { try p.run(); let data = pipe.fileHandleForReading.readDataToEndOfFile(); p.waitUntilExit(); return p.terminationStatus == 0 ? data : nil }
                catch { return nil }
            }
            guard let data = read(["account", "status", "--json"]), let status = parseAccountStatus(data) else {
                DispatchQueue.main.async { guard let me = self, me.boardGeneration == generation else { return }; me.board.refreshing = false; me.board.message = "Could not refresh account status. Try Refresh again." }
                return
            }
            DispatchQueue.main.async {
                guard let me = self, me.boardGeneration == generation else { return }
                me.lastAccountStatus = status; me.board.status = status
                let valid = Set(boardUsageRequests(status).map { $0.key })
                me.board.snapshots = me.board.snapshots.filter { valid.contains($0.key) }
            }
            var snapshots: [String: Snapshot] = [:]
            for request in boardUsageRequests(status) {
                if let data = read(request.args + ["--format", FORMAT_WITH_SENTINEL], home: request.home),
                   let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                   let text = obj["text"] as? String, let snap = parse(text, vendor: request.vendor) { snapshots[request.key] = snap }
            }
            DispatchQueue.main.async {
                guard let me = self, me.boardGeneration == generation else { return }
                me.board.snapshots = snapshots; me.board.refreshing = false; me.updateBoardIcon()
            }
        }
    }

    func performBoardAction(_ action: BoardAction) {
        if SWITCHBOARD_PREVIEW {
            switch action {
            case .refresh, .quit: break
            default:
                board.message = "Preview only. Keep using AI Usage Bar to change accounts until you approve the switch."
                return
            }
        }
        guard !accountSwitchInFlight else { return }
        func sender(_ value: Any) -> NSMenuItem { let item = NSMenuItem(); item.representedObject = value; return item }
        switch action {
        case .useConnection(let label):
            guard let profile = board.status?.cloudProfiles.first(where: { $0.label == label }) else { return }
            let model = profile.model ?? boardLabel("Model or deployment", "Enter the model ID or deployment name available to this connection.", "Continue")
            guard let model, !model.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
            if boardConfirm("Use \(label) in Codex?", "Codex will restart. Finish active tasks and CLI sessions first. New and scheduled tasks may use this provider and its billing; their models must be available there. Chats, pins, and workflow records stay in the same local workspace.", "Use connection") {
                runAccountOperation(args: ["codex-provider", "use", label, "--model", model, "--yes"])
            }
        case .recoverConnection:
            if boardConfirm("Recover connection settings?", "Codex will restart with the configuration from before the interrupted change.", "Recover") { runAccountOperation(args: ["codex-provider", "recover", "--yes"]) }
        case .addConnection: addCloudConnection()
        case .verifyCloud(let label):
            boardPopover.performClose(nil)
            runAccountOperation(args: ["codex-provider", "verify", label], success: "Local configuration checks passed. Model access and scheduled workflow compatibility require a provider-backed check. Codex was not switched or restarted, and its login, history, pins, and workflow definitions were not edited.", successTitle: "Configuration checked")
        case .claudeConnection(let desktop, let subscription):
            let detail = desktop ? "Claude will restart. Bedrock uses a separate local workspace; subscription chats, routines, and logins remain in the subscription workspace." : "Applies to new CLI sessions opened with the arrow button. Existing CLI sessions keep their current provider."
            if boardConfirm(subscription ? "Use saved Claude subscription?" : "Use \(board.status?.claudeConnectionLabel ?? "connection")?", detail, "Select") {
                var args = ["cli", "claude-connection"]
                if desktop { args.append("--desktop") }
                if subscription { args.append("--subscription") }
                runAccountOperation(args: args)
            }
        case .refresh: refreshSwitchboard()
        case .quit: quit()
        case .preferences: boardPopover.performClose(nil); openPrefs()
        case .claudeDesktop(let label): boardPopover.performClose(nil); switchDesktopAccount(sender(label))
        case .claudeCLI(let label):
            if boardConfirm("Use \(label) in Claude CLI?", "New CLI sessions will use this account. Finish active CLI work before switching.", "Set CLI default") { runAccountOperation(args: switchArgs(label: label, desktop: false)) }
        case .codexDesktop(let label):
            if board.status?.cloudActive != nil {
                if boardConfirm("Switch to \(label)?", "Codex will restart using this saved subscription. Your local chats, pins, and workflows stay in place. Finish active tasks first.", "Switch account") {
                    runAccountOperation(args: ["codex-provider", "subscription", "--account", label, "--yes"])
                }
            } else { boardPopover.performClose(nil); switchCodexAccount(sender(label)) }
        case .codexCLI(let label):
            if boardConfirm("Use \(label) in Codex CLI?", "Close CLI sessions opened by Switchboard first. Desktop keeps its current account.", "Set CLI default") { runAccountOperation(args: ["codex-account", "switch", label, "--cli", "--yes"]) }
        case .addClaude(let desktop): boardPopover.performClose(nil); addAccount(sender(desktop))
        case .saveClaude:
            if let label = boardLabel("Save current Claude CLI login", "Keep the existing CLI login and history, and give this account a name.", "Save CLI login") { runAccountOperation(args: ["account", "save", label]) }
        case .saveCodex: boardPopover.performClose(nil); saveCodexAccount()
        case .addCodex(let cli):
            if !cli { boardPopover.performClose(nil); addCodexAccount(); return }
            if let label = boardLabel("Add independent Codex CLI account", "Sign in for a dedicated CLI workspace. Desktop keeps its current login, chats, and routines. CLI history starts separately.", "Sign in") {
                runAccountOperation(args: ["codex-account", "add", label, "--cli"], success: "CLI account saved. Select it from the CLI row; use Open CLI to launch a terminal.")
            }
        case .codexMode(let separate):
            if separate && board.status?.codexCLIActive == nil { performBoardAction(.addCodex(true)); return }
            runAccountOperation(args: ["cli", "mode", separate ? "separate" : "shared"])
        case .openContinuationCLI(let provider, let directory):
            guard ["claude", "codex"].contains(provider), let binary = resolveBinary("ai-usagebar") else { return }
            boardPopover.performClose(nil)
            runInTerminal(boardTerminalScript(binary: binary, provider: provider, directory: directory))
        case .openCLI(let provider):
            guard let binary = resolveBinary("ai-usagebar") else { return }
            boardPopover.performClose(nil); runInTerminal(boardTerminalScript(binary: binary, provider: provider))
        case .recoverCodex(let cli):
            if cli {
                if boardConfirm("Recover Codex CLI login?", "Close its CLI sessions first. Desktop is unchanged.", "Recover") { runAccountOperation(args: ["codex-account", "recover", "--cli", "--yes"]) }
            } else { boardPopover.performClose(nil); recoverCodexAccount() }
        }
    }

    private func addCloudConnection() {
        boardPopover.performClose(nil)
        let templates = board.status?.connectionTemplates ?? []
        guard !templates.isEmpty else { return }
        let choose = NSAlert(); choose.messageText = "Add connection"
        choose.informativeText = "Choose a connection type. You can select the connection after saving it."
        let picker = NSPopUpButton(frame: NSRect(x: 0, y: 0, width: 320, height: 28))
        picker.addItems(withTitles: templates.map { $0.name }); choose.accessoryView = picker
        choose.addButton(withTitle: "Continue"); choose.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        guard choose.runModal() == .alertFirstButtonReturn, templates.indices.contains(picker.indexOfSelectedItem) else { return }
        let template = templates[picker.indexOfSelectedItem]
        let form = NSAlert(); form.messageText = "Connection details"
        form.informativeText = "Enter variable names from your credential file, not secret values. Leave the model blank if unknown."
        var inputs: [(String, NSTextField)] = []
        let rows: [(String, String, String)] = [("label", "Connection name", ""), ("model", "Model / deployment (optional)", "")] + template.fields.map { ($0.argument, $0.title, $0.value) }
        let stack = NSStackView(); stack.orientation = .vertical; stack.alignment = .leading; stack.spacing = 6
        for (key, title, initial) in rows {
            stack.addArrangedSubview(NSTextField(labelWithString: title))
            let field = NSTextField(string: initial); field.widthAnchor.constraint(equalToConstant: 340).isActive = true
            stack.addArrangedSubview(field); inputs.append((key, field))
        }
        stack.frame = NSRect(x: 0, y: 0, width: 340, height: CGFloat(rows.count * 54))
        form.accessoryView = stack; form.addButton(withTitle: "Choose credential file…"); form.addButton(withTitle: "Cancel")
        form.window.initialFirstResponder = inputs.first?.1
        guard form.runModal() == .alertFirstButtonReturn else { return }
        let label = inputs[0].1.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !label.isEmpty else { board.message = "A connection name is required."; return }
        let file = NSOpenPanel(); file.title = "Choose the existing credential file"; file.showsHiddenFiles = true
        file.canChooseDirectories = false; file.allowsMultipleSelection = false
        guard file.runModal() == .OK, let url = file.url else { return }
        var args = ["codex-provider", "add", label, "--provider", template.id, "--source", url.path]
        for (key, field) in inputs.dropFirst() {
            let value = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
            if !value.isEmpty {
                if key != "model" && value.range(of: "^[A-Za-z_][A-Za-z0-9_]*$", options: .regularExpression) == nil {
                    board.message = "Use environment-variable names in the mapping fields, not secret values."; return
                }
                args += ["--" + key, value]
            }
        }
        runAccountOperation(args: args, success: "Connection saved. Choose Use connection to select it for Codex.", successTitle: "Connection saved")
    }

    private func boardConfirm(_ title: String, _ detail: String, _ action: String) -> Bool {
        boardPopover.performClose(nil)
        let alert = NSAlert(); alert.messageText = title; alert.informativeText = detail
        alert.addButton(withTitle: action); alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        return alert.runModal() == .alertFirstButtonReturn
    }

    private func boardLabel(_ title: String, _ detail: String, _ action: String) -> String? {
        boardPopover.performClose(nil)
        let alert = NSAlert(); alert.messageText = title; alert.informativeText = detail
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 280, height: 25)); field.placeholderString = "Account name"
        alert.accessoryView = field; alert.addButton(withTitle: action); alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field; NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return nil }
        let label = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        return label.isEmpty ? nil : label
    }
}

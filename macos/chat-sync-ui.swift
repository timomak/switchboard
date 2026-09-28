import Cocoa
import SwiftUI

struct ChatSyncAccount: Decodable, Identifiable {
    let id: String
    let provider: String
    let label: String
    let state: String
    var detail: String?
    var exported: Int?
    var imported: Int?
    var conflicts: Int?
    var pending: Int?

    var statusLabel: String {
        switch state {
        case "ready": return "Ready"
        case "waiting": return "Waiting"
        case "error": return "Needs attention"
        default: return "Not checked"
        }
    }
}

struct ChatSyncStatus: Decodable {
    let enabled: Bool
    let available: Bool
    var deviceID: String?
    var syncRoot: String?
    var lastSyncAt: String?
    var accounts: [ChatSyncAccount]?
    var message: String?
    var pending: Int?
    var conflicts: Int?

    enum CodingKeys: String, CodingKey {
        case enabled, available, accounts, message, pending, conflicts
        case deviceID = "device_id", syncRoot = "sync_root", lastSyncAt = "last_sync_at"
    }
}

func parseChatSyncStatus(_ data: Data) -> ChatSyncStatus? {
    try? JSONDecoder().decode(ChatSyncStatus.self, from: data)
}

func chatSyncDate(_ value: String) -> Date? {
    let formatter = ISO8601DateFormatter()
    if let date = formatter.date(from: value) { return date }
    formatter.formatOptions.insert(.withFractionalSeconds)
    return formatter.date(from: value)
}

/// Every automatic pass checks the backend setting first. A disabled setting,
/// preview build, or local account operation must never start a sync run.
func chatSyncCanRun(_ status: ChatSyncStatus?, preview: Bool, accountOperation: Bool) -> Bool {
    !preview && !accountOperation && status?.enabled == true && status?.available == true
}

struct ChatSyncCommandResult {
    let data: Data
    var succeeded = true
}

private func chatSyncCommand(_ command: String) -> ChatSyncCommandResult? {
    guard let binary = resolveBinary("ai-usagebar") else { return nil }
    let process = Process()
    process.executableURL = URL(fileURLWithPath: binary)
    process.arguments = ["chat-sync", command, "--json"]
    let pipe = Pipe()
    process.standardOutput = pipe
    process.standardError = FileHandle.nullDevice
    let watchdog = DispatchWorkItem { if process.isRunning { process.terminate() } }
    // A first export can be larger than an ordinary account-status request.
    DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + 300, execute: watchdog)
    defer { watchdog.cancel() }
    do {
        try process.run()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return ChatSyncCommandResult(data: data, succeeded: process.terminationStatus == 0)
    } catch { return nil }
}

final class ChatSyncModel: ObservableObject {
    @Published var status: ChatSyncStatus?
    @Published var busy = false
    @Published var message: String?
    private var timer: Timer?
    private var shouldPause: () -> Bool = { false }
    private let execute: (String) -> ChatSyncCommandResult?
    private let preview: Bool

    init(status: ChatSyncStatus? = nil, preview: Bool = SWITCHBOARD_PREVIEW,
         execute: @escaping (String) -> ChatSyncCommandResult? = chatSyncCommand) {
        self.status = status
        self.preview = preview
        self.execute = execute
    }

    deinit { timer?.invalidate() }

    func start(shouldPause: @escaping () -> Bool) {
        guard timer == nil else { return }
        self.shouldPause = shouldPause
        refresh(thenSync: true)
        timer = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
            guard let self, !self.shouldPause() else { return }
            self.refresh(thenSync: true)
        }
        timer?.tolerance = 10
    }

    /// Opening this page only reads state; it never enables or starts syncing.
    func refresh(thenSync: Bool = false) {
        perform("status", thenSync: thenSync)
    }

    func setEnabled(_ enabled: Bool) {
        guard !preview, !shouldPause(), enabled != status?.enabled,
              !enabled || status?.available == true else { return }
        perform(enabled ? "enable" : "disable", thenSync: enabled)
    }

    func syncNow() {
        guard chatSyncCanRun(status, preview: preview, accountOperation: shouldPause()) else { return }
        perform("run")
    }

    private func perform(_ command: String, thenSync: Bool = false) {
        guard !busy else { return }
        busy = true
        message = nil
        let execute = self.execute
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let response = execute(command)
            let result = response.flatMap { parseChatSyncStatus($0.data) }
            DispatchQueue.main.async {
                guard let self else { return }
                self.busy = false
                guard response?.succeeded == true, let result else {
                    self.message = result?.message ?? (command == "status"
                        ? "Could not read chat sync status. Check that the Switchboard app and command-line tool are up to date."
                        : "Chat sync could not finish. Refresh status and try again.")
                    return
                }
                self.status = result
                if thenSync && chatSyncCanRun(result, preview: self.preview, accountOperation: self.shouldPause()) {
                    self.perform("run")
                }
            }
        }
    }
}

struct ChatSyncView: View {
    @ObservedObject var model: ChatSyncModel
    let height: CGFloat
    let accountOperation: Bool
    let close: () -> Void

    private var enabled: Bool { model.status?.enabled == true }
    private var accounts: [ChatSyncAccount] {
        (model.status?.accounts ?? []).sorted { $0.provider < $1.provider }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Button(action: close) { Image(systemName: "chevron.left") }
                    .buttonStyle(.plain).help("Back to accounts")
                Text("Chat sync").fontWeight(.medium)
                Spacer()
                if model.busy { ProgressView().controlSize(.small) }
                Button { model.refresh() } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.plain).disabled(model.busy).help("Refresh chat sync status")
            }.padding(.horizontal, 16).padding(.vertical, 13)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text("Your chats on both Macs")
                        .font(.system(size: 17, weight: .semibold))
                    Text("Shared across your accounts. Codex and Claude stay separate.")
                        .foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    Text("Syncs Codex local chats and Claude Cowork history.")
                        .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    Toggle("iCloud chat sync", isOn: Binding(get: { enabled }, set: model.setEnabled))
                        .disabled(model.busy || accountOperation || SWITCHBOARD_PREVIEW || model.status == nil || (!enabled && model.status?.available != true))
                    Text(enabled
                        ? "Checks every minute while Switchboard is running. Enable this on your other Mac using the same iCloud account."
                        : "Enable on both Macs to share this history through your iCloud Drive. Account sign-ins stay on each Mac.")
                        .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    if model.status?.available == false {
                        Label("Turn on iCloud Drive in System Settings to connect this Mac.", systemImage: "icloud.slash")
                            .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                    Divider()
                    if accounts.isEmpty {
                        Text(model.status == nil ? "Loading sync status…" : "Codex and Cowork history will appear here when available.")
                            .foregroundStyle(.secondary).font(.caption)
                    } else {
                        ForEach(accounts) { account in
                            accountRow(account)
                        }
                    }
                    if let conflicts = model.status?.conflicts, conflicts > 0 {
                        Label("Both versions are kept as separate chats.", systemImage: "square.on.square")
                            .font(.caption).fixedSize(horizontal: false, vertical: true)
                    }
                    if let pending = model.status?.pending, pending > 0 {
                        Text("Some history is waiting to sync. Check each app’s status above; Switchboard will retry.")
                            .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                    }
                    if let message = model.message ?? model.status?.message, !message.isEmpty {
                        Text(message).font(.caption).foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    if accountOperation {
                        Text("Waiting for the account change to finish.").font(.caption).foregroundStyle(.secondary)
                    }
                    HStack {
                        Button("Sync now") { model.syncNow() }
                            .disabled(model.busy || !chatSyncCanRun(model.status, preview: SWITCHBOARD_PREVIEW, accountOperation: accountOperation))
                        Spacer()
                        if let date = model.status?.lastSyncAt.flatMap(chatSyncDate) {
                            Text(date, style: .relative).font(.caption).foregroundStyle(.secondary)
                                .help("Last sync: \(date.formatted())")
                        }
                    }
                }.padding(16)
            }
        }
        .font(.system(size: 13))
        .frame(height: height)
        .background(Color(nsColor: .windowBackgroundColor))
        .onAppear { model.refresh() }
    }

    private func accountRow(_ account: ChatSyncAccount) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack {
                Text(account.label).fontWeight(.medium)
                Spacer()
                Text(account.statusLabel).font(.caption).foregroundStyle(.secondary)
            }
            if let detail = account.detail, !detail.isEmpty {
                Text(detail).font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if let exported = account.exported, let imported = account.imported, exported > 0 || imported > 0 {
                Text("\(exported) sent · \(imported) restored")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

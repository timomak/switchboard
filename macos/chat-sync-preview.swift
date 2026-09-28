// Synthetic native rendering harness: no real account, iCloud, or CLI access.
#if CHAT_SYNC_PREVIEW
import Cocoa
import SwiftUI

@main
struct ChatSyncPreview {
    static func main() throws {
        let app = NSApplication.shared
        app.setActivationPolicy(.prohibited)
        let data = Data(#"{"enabled":true,"available":true,"accounts":[{"id":"codex","provider":"codex","label":"Codex","state":"ready","detail":"Shared across your Codex accounts.","exported":12,"imported":4},{"id":"claude","provider":"claude","label":"Claude","state":"waiting","detail":"Quit Claude to restore Cowork chats."}],"pending":2,"conflicts":1}"#.utf8)
        let model = ChatSyncModel(status: parseChatSyncStatus(data), preview: true, execute: { _ in ChatSyncCommandResult(data: data) })
        let controller = NSHostingController(rootView: ChatSyncView(model: model, height: 520, accountOperation: false, close: {}))
        controller.sizingOptions = []
        let window = NSWindow(contentRect: NSRect(x: -2000, y: -2000, width: 420, height: 520), styleMask: [.borderless], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: CommandLine.arguments.contains("--dark") ? .darkAqua : .aqua)
        window.contentViewController = controller
        controller.view.setFrameSize(NSSize(width: 420, height: 520))
        window.orderFront(nil)
        RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        let view = controller.view
        view.layoutSubtreeIfNeeded()
        precondition(view.frame.width == 420 && view.frame.height == 520)
        let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds)!
        view.cacheDisplay(in: view.bounds, to: bitmap)
        try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
        print("Rendered synthetic chat sync: \(view.frame.size)")
        window.orderOut(nil)
    }
}
#endif

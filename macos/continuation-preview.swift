// Fixture-only UI harness. Never starts AppDelegate, account status refresh,
// real clipboard writes, a vendor CLI, or the production continuation library.
#if CONTINUATION_PREVIEW
import Cocoa
import SwiftUI

@main
@MainActor
struct ContinuationPreview {
    static func main() throws {
        let app = NSApplication.shared
        app.setActivationPolicy(.regular)
        let root = CommandLine.arguments.count > 1
            ? URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
            : Bundle.main.bundleURL.deletingLastPathComponent().appendingPathComponent("fixture-ui", isDirectory: true)
        let chat = try ContinuationChat.make(surface: .claudeCode, title: "Design the command palette", messages: [
            .init(role: "User", text: "Add keyboard navigation."),
            .init(role: "Assistant", text: "Use arrow keys to change selection.\nif key == .escape { close() }")
        ])
        let second = try ContinuationChat.make(surface: .claudeCode, title: "Plan a weekend reading list", messages: [.init(role: "User", text: "Choose two books.")])
        let model = ContinuationModel(store: ContinuationStore(root: root), chats: [chat, second], simulateExternalActions: true)
        model.source = .claudeCode
        let window = NSWindow(contentRect: NSRect(x: 200, y: 200, width: 420, height: 440),
                              styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Switchboard — synthetic continuation"
        window.contentView = NSHostingView(rootView: ContinuationView(model: model, height: 440,
            close: { model.finish() }, openCLI: { _, _ in model.message = "Fixture: CLI opening not executed." }))
        window.makeKeyAndOrderFront(nil)
        app.activate(ignoringOtherApps: true)
        app.run()
    }
}
#endif

// Synthetic native rendering harness: no account, iCloud, or CLI access.
#if LIBRARY_SYNC_PREVIEW
import Cocoa
import SwiftUI

@main
struct LibrarySyncPreview {
    static func main() throws {
        let app = NSApplication.shared
        app.setActivationPolicy(.prohibited)
        let data = Data(#"{"available":true,"sync_skills":true,"sync_mcp":true,"message":"Some items need setup. Check each destination.","inventory":[{"id":"review","name":"Review notes","kind":"skill","origin":"Codex","classification":"custom","supported_targets":["codex","claude_code","cowork"],"detail":"A personal skill with supporting templates."},{"id":"design","name":"Design helper","kind":"skill","origin":"Claude Code","classification":"plugin_managed","supported_targets":[],"detail":"This skill is updated by its plugin."}],"items":[{"id":"notes","name":"Review notes","kind":"skill","enabled":true,"targets":["codex","claude_code","cowork"],"state":"waiting_for_app","destinations":[{"target":"codex","state":"ready"},{"target":"claude_code","state":"waiting_for_app","detail":"Close Claude Code to install this update."},{"target":"cowork","state":"install_required"}]},{"id":"docs","name":"Team documentation","kind":"mcp","enabled":true,"targets":["codex","cowork"],"state":"needs_setup","detail":"Set up the local credential before connecting.","destinations":[{"target":"codex","state":"needs_setup","detail":"A local API key reference is required."},{"target":"cowork","state":"install_required","detail":"Install the exported plugin in each Claude account."}]},{"id":"conflict","name":"Project checklist","kind":"skill","enabled":true,"targets":["codex"],"state":"conflict","conflicts":[{"revision":"fixture-a","label":"MacBook Air · 28 Sep, 10:40","detail":"Edited on this Mac"},{"revision":"fixture-b","label":"MacBook Pro · 28 Sep, 10:42","detail":"Arrived from iCloud"}]}],"cowork":{"state":"update_available","detail":"An updated plugin is ready to export and install.","zip_path":"/fixture/Switchboard-Library.zip","exported_version":"1.2.0","installed_version":"1.1.0"}}"#.utf8)
        let model = LibrarySyncModel(status: parseLibrarySyncStatus(data), preview: true,
            execute: { _ in LibrarySyncCommandResult(data: data) })
        if CommandLine.arguments.contains("--available") { model.tab = "available" }
        if CommandLine.arguments.contains("--adopt") { model.page = .inventory("review") }
        if CommandLine.arguments.contains("--item") { model.page = .item("docs") }
        if CommandLine.arguments.contains("--conflict") { model.page = .item("conflict") }
        if CommandLine.arguments.contains("--cowork") { model.page = .cowork }
        let controller = NSHostingController(rootView: LibrarySyncView(model: model, height: 580, accountOperation: false, close: {}))
        controller.sizingOptions = []
        let window = NSWindow(contentRect: NSRect(x: -2000, y: -2000, width: 420, height: 580), styleMask: [.borderless], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: CommandLine.arguments.contains("--dark") ? .darkAqua : .aqua)
        window.contentViewController = controller
        controller.view.setFrameSize(NSSize(width: 420, height: 580))
        window.orderFront(nil)
        RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        let view = controller.view
        view.layoutSubtreeIfNeeded()
        precondition(view.frame.width == 420 && view.frame.height == 580)
        let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds)!
        view.cacheDisplay(in: view.bounds, to: bitmap)
        try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
        print("Rendered synthetic tools & skills: \(view.frame.size)")
        window.orderOut(nil)
    }
}
#endif

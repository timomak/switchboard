// Offline native rendering harness. Build with SWIFT_TEST_HARNESS so the real
// app entry point, account polling, and credential operations never run.
import Cocoa
import SwiftUI

@main
struct SwitchboardPreview {
    static func main() {
        let app = NSApplication.shared
        app.setActivationPolicy(.prohibited)
        let delegate = AppDelegate()
        let model = delegate.board
        var status = AccountStatus()
        status.desktopAvailable = true
        status.desktopActive = "Personal"
        status.desktopLabels = ["Work", "Personal"]
        status.cliHasIdentity = true
        status.codexAvailable = true
        status.codexActive = "Personal"
        status.codexLabels = ["Personal"]
        status.codexHasLogin = true
        if CommandLine.arguments.contains("--separate") {
            status.cliActive = "Personal CLI"
            status.cliLabels = ["Personal CLI"]
            status.codexCLIMode = "separate"
            status.codexCLIActive = "Work"
            status.codexCLILabels = ["Work"]
        }
        if CommandLine.arguments.contains("--short-screen") {
            model.viewport = boardViewport(visibleFrame: NSRect(x: 0, y: 0, width: 800, height: 450))
        }
        model.status = status
        func snapshot(_ session: Int, _ weekly: Int) -> Snapshot {
            Snapshot(plan: "", hasUsageWindows: true, creditBalance: nil,
                session: Window(pct: session, reset: "in 2h 10m", elapsed: nil),
                weekly: Window(pct: weekly, reset: "in 3d", elapsed: nil),
                sonnet: nil, sonnetLabel: "", extra: nil)
        }
        model.snapshots = ["claude:desktop:Personal": snapshot(5,18), "codex:desktop:Personal": snapshot(38,62),
            "claude:cli:Personal CLI": snapshot(12,31), "codex:cli:Work": snapshot(8,22)]
        if CommandLine.arguments.contains("--popover") {
            // Exercise the real menu-bar container and reopen path with fixture
            // data; refreshing stays true so toggle never polls real accounts.
            model.refreshing = true
            delegate.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
            delegate.configureSwitchboard()
            for _ in 0..<3 {
                delegate.toggleSwitchboard()
                RunLoop.current.run(until: Date().addingTimeInterval(0.3))
                let view = delegate.boardPopover.contentViewController!.view
                precondition(view.frame.width == 420 && view.frame.height == model.measuredHeight)
                delegate.boardPopover.performClose(nil)
            }
            delegate.toggleSwitchboard()
            RunLoop.current.run(until: Date().addingTimeInterval(0.3))
            let view = delegate.boardPopover.contentViewController!.view
            let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds)!
            view.cacheDisplay(in: view.bounds, to: bitmap)
            try! bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
            print("Verified four popover openings: \(view.frame.size)")
            delegate.boardPopover.performClose(nil)
            NSStatusBar.system.removeStatusItem(delegate.statusItem)
            return
        }
        let controller = NSHostingController(rootView: AccountSwitchboard(model: model))
        controller.sizingOptions = []
        let window = NSWindow(contentRect: NSRect(x: -2000, y: -2000, width: 420, height: 640), styleMask: [.borderless], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: CommandLine.arguments.contains("--dark") ? .darkAqua : .aqua)
        model.resize = { size in print("Measured content: \(size)"); DispatchQueue.main.async { window.setContentSize(size) } }
        window.contentViewController = controller
        let view = controller.view
        view.layoutSubtreeIfNeeded()
        let size = view.fittingSize
        window.setContentSize(size)
        window.orderFront(nil)
        RunLoop.current.run(until: Date().addingTimeInterval(0.5))
        view.layoutSubtreeIfNeeded()
        guard let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { fatalError("No bitmap") }
        view.cacheDisplay(in: view.bounds, to: bitmap)
        guard let png = bitmap.representation(using: .png, properties: [:]) else { fatalError("No PNG") }
        let path = CommandLine.arguments[1]
        try! png.write(to: URL(fileURLWithPath: path))
        print("Rendered native switchboard: \(Int(view.bounds.width)) × \(Int(view.bounds.height))")
        window.orderOut(nil)
    }
}

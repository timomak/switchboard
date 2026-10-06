// GUI regression harness for the real Switchboard status-item presentation.
// Deliberately avoids the app launch hook, account polling, and sync startup.
import Cocoa
import Darwin

private struct StatusItemTestFailure: Error, CustomStringConvertible {
    let description: String
}

@main
struct StatusItemTests {
    private static var checks = 0

    private static func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
        checks += 1
        if !condition() { throw StatusItemTestFailure(description: message) }
    }

    private static func cpuSeconds() -> Double {
        var usage = rusage()
        getrusage(RUSAGE_SELF, &usage)
        return Double(usage.ru_utime.tv_sec + usage.ru_stime.tv_sec)
            + Double(usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) / 1_000_000
    }

    private static func runLoop(for seconds: TimeInterval) {
        RunLoop.main.run(until: Date().addingTimeInterval(seconds))
    }

    private static func run() throws {
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        // No production delegate is installed, so finishLaunching cannot call
        // applicationDidFinishLaunching or register production timers.
        app.finishLaunching()
        try expect(!NSScreen.screens.isEmpty, "A logged-in macOS GUI session is required")
        try expect(Bundle.main.bundleIdentifier == "io.github.timomak.switchboard.status-item-tests",
                   "Run this harness through macos/run-status-item-tests.sh")

        let originalArguments = DEF.volatileDomain(forName: UserDefaults.argumentDomain)
        func setCompact(_ compact: Bool) {
            var arguments = originalArguments
            arguments["boardIconOnly"] = compact
            DEF.setVolatileDomain(arguments, forName: UserDefaults.argumentDomain)
        }
        defer { DEF.setVolatileDomain(originalArguments, forName: UserDefaults.argumentDomain) }

        let delegate = AppDelegate()
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        delegate.statusItem = item
        defer {
            delegate.appearanceObservation?.invalidate()
            NSStatusBar.system.removeStatusItem(item)
        }
        guard let button = item.button else {
            throw StatusItemTestFailure(description: "Could not create a synthetic status button")
        }

        setCompact(true)
        delegate.updateBoardIcon()
        delegate.observeAppearanceChanges()
        runLoop(for: 0.2)
        try expect(delegate.appearanceObservation == nil,
                   "Template status item must not observe effectiveAppearance")
        guard let icon = button.image else {
            throw StatusItemTestFailure(description: "Bundled status PDF was not loaded")
        }
        try expect(icon.isTemplate, "Status icon must remain an AppKit template image")
        try expect(icon.size == NSSize(width: 22, height: 22), "Status icon must be 22 × 22 points")
        try expect(button.imageScaling == .scaleProportionallyDown, "Status icon scaling changed")
        try expect(button.toolTip == "Switchboard · Claude and Codex accounts", "Status tooltip changed")
        try expect(item.length == NSStatusItem.squareLength, "Compact status item must be square")
        try expect(button.imagePosition == .imageOnly, "Compact status item must show only its icon")
        try expect(button.attributedTitle.string.isEmpty, "Compact status title must be empty")

        var imageChanges = 0
        var titleChanges = 0
        var lengthChanges = 0
        let imageObservation = button.observe(\.image, options: [.new]) { _, _ in imageChanges += 1 }
        let titleObservation = button.observe(\.attributedTitle, options: [.new]) { _, _ in titleChanges += 1 }
        let lengthObservation = item.observe(\.length, options: [.new]) { _, _ in lengthChanges += 1 }
        defer { imageObservation.invalidate(); titleObservation.invalidate(); lengthObservation.invalidate() }

        for _ in 0..<100 { delegate.updateBoardIcon() }
        try expect(button.image === icon, "Repeated updates must preserve image identity")
        try expect(imageChanges == 0 && titleChanges == 0 && lengthChanges == 0,
                   "Repeated updates must not rewrite image, title, or length")

        setCompact(false)
        delegate.updateBoardIcon()
        runLoop(for: 0.2)
        try expect(item.length == NSStatusItem.variableLength, "Expanded status item must size to its content")
        try expect(button.imagePosition == .imageLeading, "Expanded status item must lead with its icon")
        try expect(button.attributedTitle.string == "  Switchboard", "Expanded status title changed")
        try expect(button.image === icon, "Changing layout must reuse the existing icon")
        let expandedTitleChanges = titleChanges
        let expandedLengthChanges = lengthChanges
        for _ in 0..<100 { delegate.updateBoardIcon() }
        try expect(titleChanges == expandedTitleChanges && imageChanges == 0 && lengthChanges == expandedLengthChanges,
                   "Unchanged expanded presentation must not be rewritten")

        // AppKit temporarily changes a status button's appearance while drawing
        // menu-bar replicants. Exercise both appearances and restoration without
        // touching the user's display, wallpaper, or system appearance settings.
        let originalAppearance = button.appearance
        for name: NSAppearance.Name in [.aqua, .darkAqua, .aqua, .darkAqua] {
            button.appearance = NSAppearance(named: name)
            runLoop(for: 0.1)
            try expect(button.image === icon && icon.isTemplate,
                       "Appearance changes must preserve the template icon")
        }
        button.appearance = originalAppearance
        runLoop(for: 0.2)
        try expect(titleChanges == expandedTitleChanges && imageChanges == 0,
                   "Appearance changes must not rewrite status-item content")

        setCompact(true)
        try expect(DEF.bool(forKey: "boardIconOnly"), "Volatile compact preference must be true")
        delegate.updateBoardIcon()
        try expect(button.attributedTitle.string.isEmpty,
                   "Returning to compact mode must clear the title (actual: \(button.attributedTitle.string))")
        try expect(button.imagePosition == .imageOnly,
                   "Returning to compact mode must restore icon-only presentation (actual: \(button.imagePosition.rawValue))")
        runLoop(for: 0.2)
        try expect(item.length == NSStatusItem.squareLength,
                   "Compact status length must remain square after layout")
        try expect(button.image === icon, "Compact mode must retain the original icon")
        try expect(delegate.timer == nil && delegate.configWatchSource == nil,
                   "Status-item harness must not start production background work")

        // Exclude launch and appearance-test drawing from the performance gate.
        // The generous 10% single-core ceiling catches the original continuous
        // redraw loop while allowing incidental WindowServer activity on CI.
        runLoop(for: 1)
        let idleImageChanges = imageChanges
        let idleTitleChanges = titleChanges
        let idleLengthChanges = lengthChanges
        let startCPU = cpuSeconds()
        let start = ProcessInfo.processInfo.systemUptime
        runLoop(for: 5)
        let elapsed = ProcessInfo.processInfo.systemUptime - start
        let cpuPercent = 100 * (cpuSeconds() - startCPU) / elapsed
        try expect(imageChanges == idleImageChanges && titleChanges == idleTitleChanges && lengthChanges == idleLengthChanges,
                   "Idle status item must not rewrite its image, title, or length")
        print(String(format: "Status item: %d checks, %.1fs idle, %.2f%% CPU, %d image changes, %d title changes, %d length changes",
                     checks + 1, elapsed, cpuPercent, imageChanges, titleChanges, lengthChanges))
        try expect(cpuPercent < 10, "Idle CPU exceeded 10% of one core")
    }

    static func main() {
        // This watchdog runs off the main queue, so a main-thread redraw loop
        // cannot leave the reproduction running. Process exit removes its item.
        DispatchQueue.global(qos: .userInitiated).asyncAfter(deadline: .now() + 14) {
            fputs("FAIL: status-item harness exceeded its 14-second bound\n", stderr)
            _exit(124)
        }
        do {
            try run()
            print("PASS: status-item GUI regression")
        } catch {
            fputs("FAIL: \(error)\n", stderr)
            exit(1)
        }
    }
}

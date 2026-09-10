import Cocoa
import ApplicationServices

@MainActor
enum ContinuationClaude {
    enum Failure: LocalizedError {
        case missing, accessibility, composer
        var errorDescription: String? {
            switch self {
            case .missing: return "Install Claude Desktop to continue."
            case .accessibility: return "For longer chats, allow Switchboard in System Settings → Privacy & Security → Accessibility, then try again."
            case .composer: return "Claude opened, but the context could not be inserted. Use Advanced → Copy context, then paste it in Claude."
            }
        }
    }
    static func open(context: String) async throws {
        guard let appURL = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.anthropic.claudefordesktop") else { throw Failure.missing }
        let long = context.utf16.count > ContinuationClaudeLink.promptLimit
        if long {
            let options = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
            guard AXIsProcessTrustedWithOptions(options) else { throw Failure.accessibility }
        }
        let marker = "Preparing conversation… \(UUID().uuidString)"
        let url = try ContinuationClaudeLink.make(prompt: long ? marker : context)
        let app: NSRunningApplication = try await withCheckedThrowingContinuation { continuation in
            NSWorkspace.shared.open([url], withApplicationAt: appURL, configuration: NSWorkspace.OpenConfiguration()) { app, error in
                if let error { continuation.resume(throwing: error) }
                else if let app { continuation.resume(returning: app) }
                else { continuation.resume(throwing: Failure.missing) }
            }
        }
        guard long else { return }
        let root = AXUIElementCreateApplication(app.processIdentifier)
        AXUIElementSetMessagingTimeout(root, 0.2)
        for _ in 0..<40 {
            try Task.checkCancellation()
            if let composer = findComposer(root, marker: marker) {
                // Only replace our unique marker in a new composer. Never focus,
                // select all, paste into an existing draft, or press Send.
                guard AXUIElementSetAttributeValue(composer, kAXValueAttribute as CFString, context as CFString) == .success else { throw Failure.composer }
                if value(composer, kAXValueAttribute) as? String == context { return }
                throw Failure.composer
            }
            try await Task.sleep(for: .milliseconds(250))
        }
        throw Failure.composer
    }
    private static func value(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
        var result: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, name as CFString, &result) == .success else { return nil }
        return result
    }
    private static func findComposer(_ root: AXUIElement, marker: String) -> AXUIElement? {
        var queue = [root]; var index = 0
        let deadline = Date().addingTimeInterval(0.5)
        while index < queue.count && index < 6000 && Date() < deadline {
            let element = queue[index]; index += 1
            let role = value(element, kAXRoleAttribute) as? String
            if [kAXTextAreaRole, kAXTextFieldRole].contains(role ?? ""), value(element, kAXValueAttribute) as? String == marker { return element }
            if let children = value(element, kAXChildrenAttribute) as? [AXUIElement] { queue.append(contentsOf: children.prefix(max(0, 6000 - queue.count))) }
        }
        return nil
    }
}

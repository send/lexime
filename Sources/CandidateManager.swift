import Cocoa
import InputMethodKit

class CandidateManager {

    private(set) var candidates: [String] = []
    private(set) var selectedIndex: Int = 0

    /// Monotonically increasing counter; invalidates stale async results.
    private(set) var generation: UInt64 = 0

    /// Set when commit_text moves the cursor; forces panel to recalculate position on next show.
    private var needsReposition = false

    static let maxDisplay = 9

    // MARK: - Generation

    func invalidate() {
        generation &+= 1
    }

    // MARK: - State

    func update(surfaces: [String], selected: Int) {
        candidates = surfaces
        selectedIndex = selected
    }

    func flagReposition() {
        needsReposition = true
    }

    func reset() {
        candidates = []
        selectedIndex = 0
    }

    func deactivate() {
        invalidate()
        hide()
    }

    // MARK: - Panel

    private static let modeNames = ["standard", "predictive", "ghost"]

    private var currentModeName: String? {
        let mode = UserDefaults.standard.integer(forKey: "conversionMode")
        guard mode > 0, mode < Self.modeNames.count else { return nil }
        return Self.modeNames[mode]
    }

    func show(client: IMKTextInput, currentDisplay: String?) {
        guard !candidates.isEmpty else { hide(); return }
        let clampedIndex = min(selectedIndex, candidates.count - 1)

        let pageSize = Self.maxDisplay
        let page = clampedIndex / pageSize
        let pageStart = page * pageSize
        let pageEnd = min(pageStart + pageSize, candidates.count)
        let pageCandidates = Array(candidates[pageStart..<pageEnd])
        let pageSelectedIndex = clampedIndex - pageStart

        let panel = AppContext.shared.candidatePanel
        let totalCount = candidates.count
        let modeName = currentModeName

        // Mozc style: don't recalculate position while panel is visible (prevents jitter)
        // But if cursor moved (auto-commit), force reposition.
        if panel.isVisible && !needsReposition {
            panel.show(candidates: pageCandidates, selectedIndex: pageSelectedIndex,
                       globalIndex: clampedIndex, totalCount: totalCount, cursorRect: nil,
                       modeName: modeName)
            return
        }
        // Reset early: if the async block below is cancelled (generation mismatch),
        // the panel stays hidden, so the next show() takes the full path anyway.
        needsReposition = false

        // Capture rect synchronously (client state is correct here),
        // then defer panel show to next run loop (workaround for Chrome etc.)
        let rect = cursorRect(client: client, currentDisplay: currentDisplay)
        let gen = generation
        DispatchQueue.main.async { [weak self] in
            guard let self, self.generation == gen else { return }
            panel.show(candidates: pageCandidates, selectedIndex: pageSelectedIndex,
                       globalIndex: clampedIndex, totalCount: totalCount, cursorRect: rect,
                       modeName: modeName)
        }
    }

    func hide() {
        AppContext.shared.candidatePanel.hide()
    }

    // MARK: - Cursor Rect

    func cursorRect(client: IMKTextInput, currentDisplay: String?) -> NSRect {
        var rect = NSRect.zero
        client.attributes(forCharacterIndex: 0, lineHeightRectangle: &rect)
        let index = currentDisplay?.utf16.count ?? 0
        if index > 0 {
            var endRect = NSRect.zero
            client.attributes(forCharacterIndex: index, lineHeightRectangle: &endRect)
            if endRect != .zero {
                rect.origin.x = endRect.origin.x
            }
        }
        return rect
    }
}

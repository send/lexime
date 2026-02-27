import Cocoa

// MARK: - CandidateListView

class CandidateListView: NSView {

    var candidates: [String] = []
    var selectedIndex: Int = 0

    override var isFlipped: Bool { true }

    // MARK: - Accessibility

    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .list }
    override func accessibilityLabel() -> String? { "変換候補" }

    private let font = NSFont.systemFont(ofSize: 14)
    private let rowHeight: CGFloat = 24
    private let horizontalPadding: CGFloat = 8
    private let verticalTextInset: CGFloat = 3

    var desiredSize: NSSize {
        var maxTextWidth: CGFloat = 0

        let attrs: [NSAttributedString.Key: Any] = [.font: font]
        for c in candidates {
            let w = (c as NSString).size(withAttributes: attrs).width
            if w > maxTextWidth { maxTextWidth = w }
        }

        guard maxTextWidth > 0 else { return .zero }
        let width = horizontalPadding + maxTextWidth + horizontalPadding
        let height = rowHeight * CGFloat(candidates.count)
        return NSSize(width: ceil(width), height: ceil(height))
    }

    override func draw(_ dirtyRect: NSRect) {
        let bg = NSColor.windowBackgroundColor
        bg.setFill()
        NSBezierPath(roundedRect: bounds, xRadius: 4, yRadius: 4).fill()

        for (i, candidate) in candidates.enumerated() {
            let y = CGFloat(i) * rowHeight
            let rowRect = NSRect(x: 0, y: y, width: bounds.width, height: rowHeight)

            if i == selectedIndex {
                NSColor.selectedContentBackgroundColor.setFill()
                NSBezierPath(roundedRect: rowRect.insetBy(dx: 2, dy: 1), xRadius: 3, yRadius: 3).fill()
            }

            let textColor: NSColor = i == selectedIndex
                ? .alternateSelectedControlTextColor
                : .labelColor

            let textAttrs: [NSAttributedString.Key: Any] = [
                .font: font,
                .foregroundColor: textColor,
            ]

            let textRect = NSRect(x: horizontalPadding, y: y + verticalTextInset,
                                  width: bounds.width - horizontalPadding * 2, height: rowHeight)
            (candidate as NSString).draw(in: textRect, withAttributes: textAttrs)
        }
    }
}

// MARK: - CandidatePanel

class CandidatePanel: NSPanel {

    private let listView = CandidateListView()

    init() {
        super.init(
            contentRect: NSRect(x: 0, y: 0, width: 200, height: 100),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: true
        )
        level = .popUpMenu
        hasShadow = true
        isOpaque = false
        backgroundColor = .clear

        let content = NSView()
        content.wantsLayer = true
        content.layer?.cornerRadius = 4
        content.layer?.masksToBounds = true
        contentView = content

        content.addSubview(listView)
    }

    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }

    func show(candidates: [String], selectedIndex: Int, globalIndex: Int, totalCount: Int,
              cursorRect: NSRect?) {
        guard !candidates.isEmpty else {
            hide()
            return
        }

        listView.candidates = candidates
        listView.selectedIndex = selectedIndex

        let size = listView.desiredSize
        listView.frame = NSRect(origin: .zero, size: size)

        let panelSize = size

        if let cursorRect {
            var origin = NSPoint(x: cursorRect.origin.x, y: cursorRect.origin.y - panelSize.height)

            // Clamp to screen
            if let screen = NSScreen.main ?? NSScreen.screens.first {
                let screenFrame = screen.visibleFrame

                // If below screen bottom, flip above cursor
                if origin.y < screenFrame.minY {
                    origin.y = cursorRect.maxY
                }
                // Clamp right edge
                if origin.x + panelSize.width > screenFrame.maxX {
                    origin.x = screenFrame.maxX - panelSize.width
                }
                // Clamp left edge
                if origin.x < screenFrame.minX {
                    origin.x = screenFrame.minX
                }
            }

            setFrame(NSRect(origin: origin, size: panelSize), display: false)
        } else {
            // Position freeze: anchor top edge (cursor position), grow downward
            setContentSize(panelSize)
        }

        listView.needsDisplay = true
        orderFront(nil)

        announceCandidate(candidates[selectedIndex], index: globalIndex, total: totalCount)
    }

    func hide() {
        orderOut(nil)
    }

    // MARK: - VoiceOver

    private func announceCandidate(_ candidate: String, index: Int, total: Int) {
        guard NSWorkspace.shared.isVoiceOverEnabled else { return }
        let message = "\(candidate) \(index + 1)/\(total)"
        let userInfo: [NSAccessibility.NotificationUserInfoKey: Any] = [
            .announcement: message,
            .priority: NSAccessibilityPriorityLevel.high.rawValue,
        ]
        NSAccessibility.post(
            element: self,
            notification: .announcementRequested,
            userInfo: userInfo
        )
    }
}

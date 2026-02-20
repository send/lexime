import Cocoa

// MARK: - CandidateListView

class CandidateListView: NSView {

    var candidates: [String] = []
    var selectedIndex: Int = 0
    var modeName: String?

    override var isFlipped: Bool { true }

    private let font = NSFont.systemFont(ofSize: 14)
    private let headerFont = NSFont.systemFont(ofSize: 10)
    private let rowHeight: CGFloat = 24
    private let headerHeight: CGFloat = 18
    private let horizontalPadding: CGFloat = 8
    private let verticalTextInset: CGFloat = 3

    private var headerOffset: CGFloat {
        modeName != nil ? headerHeight : 0
    }

    /// Whether the view is in notification-only mode (no candidates, just modeName).
    private var isNotificationMode: Bool {
        candidates.isEmpty && modeName != nil
    }

    private let notificationHeight: CGFloat = 28

    var desiredSize: NSSize {
        var maxTextWidth: CGFloat = 0

        if let modeName {
            let modeFont = isNotificationMode ? font : headerFont
            let headerAttrs: [NSAttributedString.Key: Any] = [.font: modeFont]
            let w = (modeName as NSString).size(withAttributes: headerAttrs).width
            if w > maxTextWidth { maxTextWidth = w }
        }

        let attrs: [NSAttributedString.Key: Any] = [.font: font]
        for c in candidates {
            let w = (c as NSString).size(withAttributes: attrs).width
            if w > maxTextWidth { maxTextWidth = w }
        }

        guard maxTextWidth > 0 else { return .zero }
        let width = horizontalPadding + maxTextWidth + horizontalPadding

        let height: CGFloat
        if isNotificationMode {
            height = notificationHeight
        } else {
            height = headerOffset + rowHeight * CGFloat(candidates.count)
        }
        return NSSize(width: ceil(width), height: ceil(height))
    }

    override func draw(_ dirtyRect: NSRect) {
        let bg = NSColor.windowBackgroundColor
        bg.setFill()
        NSBezierPath(roundedRect: bounds, xRadius: 4, yRadius: 4).fill()

        // Mode header
        if let modeName {
            if isNotificationMode {
                // Notification mode: larger font, vertically centered
                let attrs: [NSAttributedString.Key: Any] = [
                    .font: font,
                    .foregroundColor: NSColor.labelColor,
                ]
                let textSize = (modeName as NSString).size(withAttributes: attrs)
                let y = (bounds.height - textSize.height) / 2
                let rect = NSRect(x: horizontalPadding, y: y,
                                  width: bounds.width - horizontalPadding * 2, height: textSize.height)
                (modeName as NSString).draw(in: rect, withAttributes: attrs)
            } else {
                let headerAttrs: [NSAttributedString.Key: Any] = [
                    .font: headerFont,
                    .foregroundColor: NSColor.secondaryLabelColor,
                ]
                let headerRect = NSRect(x: horizontalPadding, y: 2,
                                        width: bounds.width - horizontalPadding * 2, height: headerHeight)
                (modeName as NSString).draw(in: headerRect, withAttributes: headerAttrs)
            }
        }

        for (i, candidate) in candidates.enumerated() {
            let y = headerOffset + CGFloat(i) * rowHeight
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
    private var notificationTimer: Timer?

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
              cursorRect: NSRect?, modeName: String? = nil) {
        notificationTimer?.invalidate()
        notificationTimer = nil
        guard !candidates.isEmpty else {
            hide()
            return
        }

        listView.candidates = candidates
        listView.selectedIndex = selectedIndex
        listView.modeName = modeName

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
        notificationTimer?.invalidate()
        notificationTimer = nil
        orderOut(nil)
    }

    /// Show a brief mode notification that auto-dismisses after 1 second.
    func showNotification(text: String, cursorRect: NSRect) {
        notificationTimer?.invalidate()

        listView.candidates = []
        listView.selectedIndex = 0
        listView.modeName = text

        let size = listView.desiredSize
        guard size != .zero else { return }
        listView.frame = NSRect(origin: .zero, size: size)

        var origin = NSPoint(x: cursorRect.origin.x, y: cursorRect.origin.y - size.height)
        if let screen = NSScreen.main ?? NSScreen.screens.first {
            let sf = screen.visibleFrame
            if origin.y < sf.minY { origin.y = cursorRect.maxY }
            if origin.x + size.width > sf.maxX { origin.x = sf.maxX - size.width }
            if origin.x < sf.minX { origin.x = sf.minX }
        }

        setFrame(NSRect(origin: origin, size: size), display: true)
        listView.needsDisplay = true
        orderFront(nil)

        notificationTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: false) { [weak self] _ in
            self?.hide()
        }
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

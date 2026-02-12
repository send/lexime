import Cocoa

// MARK: - CandidateListView

class CandidateListView: NSView {

    var candidates: [String] = []
    var selectedIndex: Int = 0

    override var isFlipped: Bool { true }

    private let font = NSFont.systemFont(ofSize: 14)
    private let rowHeight: CGFloat = 24
    private let horizontalPadding: CGFloat = 8

    var desiredSize: NSSize {
        guard !candidates.isEmpty else { return .zero }
        let attrs: [NSAttributedString.Key: Any] = [.font: font]
        var maxTextWidth: CGFloat = 0
        for c in candidates {
            let w = (c as NSString).size(withAttributes: attrs).width
            if w > maxTextWidth { maxTextWidth = w }
        }
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

            let textRect = NSRect(x: horizontalPadding, y: y + 3,
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

    func show(candidates: [String], selectedIndex: Int, cursorRect: NSRect) {
        guard !candidates.isEmpty else {
            hide()
            return
        }

        listView.candidates = candidates
        listView.selectedIndex = selectedIndex

        let size = listView.desiredSize
        listView.frame = NSRect(origin: .zero, size: size)

        let panelSize = size
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

        setContentSize(panelSize)
        setFrameOrigin(origin)
        listView.needsDisplay = true
        orderFront(nil)
    }

    func hide() {
        orderOut(nil)
    }
}

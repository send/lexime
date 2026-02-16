import Cocoa
import InputMethodKit

class GhostTextManager {

    private(set) var text: String?

    private let queue = DispatchQueue(label: "sh.send.lexime.ghost", qos: .utility)
    private var debounceItem: DispatchWorkItem?

    // MARK: - Display

    func showDisplay(_ text: String, client: IMKTextInput) {
        let attrs: [NSAttributedString.Key: Any] = [
            .foregroundColor: NSColor.placeholderTextColor,
            .markedClauseSegment: 0,
        ]
        let attrStr = NSAttributedString(string: text, attributes: attrs)
        client.setMarkedText(attrStr,
                             selectionRange: NSRange(location: 0, length: 0),
                             replacementRange: NSRange(location: NSNotFound, length: 0))
    }

    func clearDisplay(client: IMKTextInput) {
        client.setMarkedText("",
                             selectionRange: NSRange(location: 0, length: 0),
                             replacementRange: NSRange(location: NSNotFound, length: 0))
    }

    // MARK: - State

    func set(_ ghost: String, client: IMKTextInput) {
        text = ghost
        showDisplay(ghost, client: client)
    }

    func clear(client: IMKTextInput, updateDisplay: Bool) {
        text = nil
        debounceItem?.cancel()
        debounceItem = nil
        if updateDisplay {
            clearDisplay(client: client)
        }
    }

    func deactivate() {
        debounceItem?.cancel()
        debounceItem = nil
        text = nil
    }

    // MARK: - Async Neural Generation

    func requestGeneration(
        context: String,
        generation: UInt64,
        session: LexSession,
        completion: @escaping (LexKeyResult) -> Void
    ) {
        debounceItem?.cancel()
        let neural = AppContext.shared.neural
        guard let neural else { return }
        let item = DispatchWorkItem { [weak self] in
            guard let text = neural.generateGhost(context: context, maxTokens: 30) else { return }
            guard !text.isEmpty else { return }
            DispatchQueue.main.async { [weak self] in
                guard self != nil else { return }
                guard let resp = session.receiveGhostText(generation: generation, text: text)
                else { return }
                completion(resp)
            }
        }
        debounceItem = item
        queue.asyncAfter(deadline: .now() + 0.15, execute: item)
    }
}

import Carbon
import Cocoa
import Foundation
import InputMethodKit

/// Full stub of the `IMKTextInput` protocol. Records the interactions the
/// controller layer uses (`insertText`, `setMarkedText`) and returns inert
/// defaults for every other method so we satisfy protocol requirements.
final class FakeIMKClient: NSObject, IMKTextInput {
    struct InsertCall: Equatable {
        let text: String
        let replacementRange: NSRange
    }
    struct MarkedCall: Equatable {
        let text: String
        let selectionRange: NSRange
        let replacementRange: NSRange
    }

    var insertCalls: [InsertCall] = []
    var markedCalls: [MarkedCall] = []

    /// Optional rect returned from `attributes(forCharacterIndex:...)` so
    /// CandidateManager has a non-zero cursor position to work with.
    var attributesRect: NSRect = .zero

    // MARK: IMKTextInput

    func insertText(_ string: Any!, replacementRange: NSRange) {
        let text = (string as? String) ?? (string as? NSAttributedString)?.string ?? ""
        insertCalls.append(InsertCall(text: text, replacementRange: replacementRange))
    }

    func setMarkedText(_ string: Any!, selectionRange: NSRange, replacementRange: NSRange) {
        let text = (string as? String) ?? (string as? NSAttributedString)?.string ?? ""
        markedCalls.append(MarkedCall(
            text: text,
            selectionRange: selectionRange,
            replacementRange: replacementRange))
    }

    func selectedRange() -> NSRange { NSRange(location: NSNotFound, length: 0) }
    func markedRange() -> NSRange { NSRange(location: NSNotFound, length: 0) }
    func attributedSubstring(from range: NSRange) -> NSAttributedString! { NSAttributedString(string: "") }
    func length() -> Int { 0 }

    func characterIndex(for point: NSPoint,
                        tracking mappingMode: IMKLocationToOffsetMappingMode,
                        inMarkedRange: UnsafeMutablePointer<ObjCBool>!) -> Int { 0 }

    func attributes(forCharacterIndex index: Int,
                    lineHeightRectangle lineRect: UnsafeMutablePointer<NSRect>!) -> [AnyHashable: Any]! {
        lineRect?.pointee = attributesRect
        return [:]
    }

    func validAttributesForMarkedText() -> [Any]! { [] }
    func overrideKeyboard(withKeyboardNamed keyboardUniqueName: String!) {}
    func selectMode(_ modeIdentifier: String!) {}
    func supportsUnicode() -> Bool { true }
    func bundleIdentifier() -> String! { "test.client" }
    func windowLevel() -> CGWindowLevel { 0 }
    func supportsProperty(_ property: TSMDocumentPropertyTag) -> Bool { false }
    func uniqueClientIdentifierString() -> String! { "fake-client" }

    func string(from range: NSRange, actualRange: NSRangePointer!) -> String! { "" }
    func firstRect(forCharacterRange aRange: NSRange, actualRange: NSRangePointer!) -> NSRect { .zero }
}

/// Records the panel operations the tests care about. Default `visible = true`
/// routes `CandidateManager.show` through the synchronous fast path; individual
/// tests flip it to `false` to exercise the deferred path.
final class FakePanel: CandidatePanelDisplaying {
    struct ShowCall: Equatable {
        let candidates: [String]
        let selectedIndex: Int
        let globalIndex: Int
        let totalCount: Int
        let hasCursorRect: Bool
    }

    var visible: Bool = true
    var showCalls: [ShowCall] = []
    var hideCalls: Int = 0

    var showCount: Int { showCalls.count }
    var hideCount: Int { hideCalls }
    var lastCandidates: [String] { showCalls.last?.candidates ?? [] }
    var lastSelectedIndex: Int { showCalls.last?.selectedIndex ?? 0 }

    var isVisible: Bool { visible }

    func show(candidates: [String], selectedIndex: Int, globalIndex: Int, totalCount: Int,
              cursorRect: NSRect?) {
        showCalls.append(ShowCall(
            candidates: candidates,
            selectedIndex: selectedIndex,
            globalIndex: globalIndex,
            totalCount: totalCount,
            hasCursorRect: cursorRect != nil))
        visible = true
    }

    func hide() {
        hideCalls += 1
        visible = false
    }
}

/// In-memory fake of `LexSessionProtocol`. Tests queue responses to be returned
/// by `handleKey` / `commit` and inspect recorded calls afterwards.
final class FakeLexSession: LexSessionProtocol, @unchecked Sendable {
    var handleKeyResponses: [LexKeyResponse] = []
    var commitResponses: [LexKeyResponse] = []

    var handleKeyCalls: [LexKeyEvent] = []
    var commitCalls: Int = 0
    var shutdownCalls: Int = 0
    var isComposingValue: Bool = false

    var setSnippetStoreCalls: Int = 0
    var setAbcPassthroughCalls: [Bool] = []
    var setConversionModeCalls: [LexConversionMode] = []
    var setDeferCandidatesCalls: [Bool] = []

    func handleKey(event: LexKeyEvent) -> LexKeyResponse {
        handleKeyCalls.append(event)
        if handleKeyResponses.isEmpty {
            return LexKeyResponse(consumed: false, events: [])
        }
        return handleKeyResponses.removeFirst()
    }

    func commit() -> LexKeyResponse {
        commitCalls += 1
        if commitResponses.isEmpty {
            return LexKeyResponse(consumed: false, events: [])
        }
        return commitResponses.removeFirst()
    }

    func isComposing() -> Bool { isComposingValue }
    func setAbcPassthrough(enabled: Bool) { setAbcPassthroughCalls.append(enabled) }
    func setConversionMode(mode: LexConversionMode) { setConversionModeCalls.append(mode) }
    func setDeferCandidates(enabled: Bool) { setDeferCandidatesCalls.append(enabled) }
    func setSnippetStore(store: LexSnippetStore?) { setSnippetStoreCalls += 1 }
    func shutdown() { shutdownCalls += 1 }
}

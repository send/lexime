import Cocoa
import InputMethodKit

@objc(LeximeInputController)
class LeximeInputController: IMKInputController {

    private var composedKana: String = ""
    private var pendingRomaji: String = ""
    private var isComposing: Bool { !composedKana.isEmpty || !pendingRomaji.isEmpty }

    private let trie = RomajiTrie.shared
    private static let vowels: Set<Character> = ["a", "i", "u", "e", "o"]

    override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)
        let version = String(cString: lex_engine_version())
        NSLog("Lexime: InputController initialized (engine: %@)", version)
    }

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard let event = event, let client = sender as? IMKTextInput else {
            return false
        }

        guard event.type == .keyDown else {
            return false
        }

        let dominated = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            .subtracting([.capsLock, .numericPad, .function])

        // Modifier keys (Cmd, Ctrl, etc.) — commit first, then pass through
        if !dominated.isEmpty {
            if isComposing {
                flush(client: client)
                commitComposed(client: client)
            }
            return false
        }

        let keyCode = event.keyCode
        guard let text = event.characters, !text.isEmpty else {
            return false
        }

        NSLog("Lexime: handle keyCode=%d text=%@", keyCode, text)

        switch keyCode {
        case 36: // Enter
            if isComposing {
                flush(client: client)
                commitComposed(client: client)
                return true
            }
            return false

        case 49: // Space
            if isComposing {
                flush(client: client)
                commitComposed(client: client)
                return false
            }
            return false

        case 51: // Backspace
            if isComposing {
                if !pendingRomaji.isEmpty {
                    pendingRomaji.removeLast()
                } else if !composedKana.isEmpty {
                    composedKana.removeLast()
                }
                if isComposing {
                    updateMarkedText(client: client)
                } else {
                    // All cleared — remove marked text
                    client.setMarkedText("",
                                        selectionRange: NSRange(location: 0, length: 0),
                                        replacementRange: NSRange(location: NSNotFound, length: 0))
                }
                return true
            }
            return false

        case 53: // Escape
            if isComposing {
                composedKana = ""
                pendingRomaji = ""
                client.setMarkedText("",
                                     selectionRange: NSRange(location: 0, length: 0),
                                     replacementRange: NSRange(location: NSNotFound, length: 0))
                return true
            }
            return false

        default:
            break
        }

        let scalar = text.unicodeScalars.first!

        // Punctuation: period and comma
        if text == "." || text == "," {
            if isComposing {
                flush(client: client)
                commitComposed(client: client)
            }
            let punct = text == "." ? "。" : "、"
            client.insertText(punct, replacementRange: NSRange(location: NSNotFound, length: 0))
            return true
        }

        // Alphabetic characters (a-z)
        if CharacterSet.lowercaseLetters.contains(scalar) ||
           CharacterSet.uppercaseLetters.contains(scalar) {
            let ch = text.lowercased()
            appendAndConvert(ch, client: client)
            return true
        }

        // Hyphen for prolonged sound mark
        if text == "-" {
            appendAndConvert("-", client: client)
            return true
        }

        // Other characters — commit composing first, then pass through
        if isComposing {
            flush(client: client)
            commitComposed(client: client)
        }
        return false
    }

    // MARK: - Conversion

    private func appendAndConvert(_ input: String, client: IMKTextInput) {
        pendingRomaji += input
        drainPendingRomaji(force: false)
        updateMarkedText(client: client)
    }

    /// Consume pendingRomaji as much as possible.
    /// - force=false: stop at prefix/exactAndPrefix (wait for more input)
    /// - force=true: greedily convert everything (used before commit)
    private func drainPendingRomaji(force: Bool) {
        var changed = true
        while !pendingRomaji.isEmpty && changed {
            changed = false
            let result = trie.lookup(pendingRomaji)

            switch result {
            case .exact(let kana):
                composedKana += kana
                pendingRomaji = ""
                changed = true

            case .exactAndPrefix(let kana):
                if force {
                    composedKana += kana
                    pendingRomaji = ""
                    changed = true
                }
                // else: wait for more input — a longer match might exist

            case .prefix:
                if !force { break }
                // force mode: try shorter prefixes
                fallthrough

            case .none:
                // Try to find the longest prefix match
                var found = false
                for len in stride(from: pendingRomaji.count - 1, through: 1, by: -1) {
                    let subEnd = pendingRomaji.index(pendingRomaji.startIndex, offsetBy: len)
                    let sub = String(pendingRomaji[..<subEnd])
                    let subResult = trie.lookup(sub)

                    switch subResult {
                    case .exact(let kana), .exactAndPrefix(let kana):
                        composedKana += kana
                        pendingRomaji = String(pendingRomaji[subEnd...])
                        found = true
                        changed = true
                    default:
                        break
                    }
                    if found { break }
                }

                if !found {
                    if pendingRomaji.count >= 2 {
                        let chars = Array(pendingRomaji)
                        let first = chars[0]
                        let second = chars[1]
                        // Same consonant (not 'n', not vowel) → っ
                        if first == second && first != "n" && !Self.vowels.contains(first) {
                            composedKana += "っ"
                            pendingRomaji = String(pendingRomaji.dropFirst())
                            changed = true
                        } else if first == "n" && !Self.vowels.contains(second) &&
                                  second != "n" && second != "y" {
                            // "n" followed by non-vowel, non-n, non-y → ん
                            composedKana += "ん"
                            pendingRomaji = String(pendingRomaji.dropFirst())
                            changed = true
                        } else if force {
                            composedKana += String(pendingRomaji.removeFirst())
                            changed = true
                        } else {
                            pendingRomaji = String(pendingRomaji.dropFirst())
                            changed = true
                        }
                    } else {
                        // Single character remaining
                        if force {
                            if pendingRomaji == "n" {
                                composedKana += "ん"
                            } else {
                                composedKana += pendingRomaji
                            }
                        }
                        pendingRomaji = ""
                        changed = true
                    }
                }
            }
        }
    }

    // MARK: - Flush & Commit

    private func flush(client: IMKTextInput) {
        drainPendingRomaji(force: true)
    }

    private func commitComposed(client: IMKTextInput) {
        if !composedKana.isEmpty {
            NSLog("Lexime: commit %@", composedKana)
            client.insertText(composedKana, replacementRange: NSRange(location: NSNotFound, length: 0))
            composedKana = ""
        } else {
            // Clear marked text even if nothing to commit
            client.setMarkedText("",
                                 selectionRange: NSRange(location: 0, length: 0),
                                 replacementRange: NSRange(location: NSNotFound, length: 0))
        }
        pendingRomaji = ""
    }

    // MARK: - Marked Text

    private func updateMarkedText(client: IMKTextInput) {
        let display = composedKana + pendingRomaji
        let len = display.utf16.count
        client.setMarkedText(display,
                             selectionRange: NSRange(location: len, length: 0),
                             replacementRange: NSRange(location: NSNotFound, length: 0))
    }
}

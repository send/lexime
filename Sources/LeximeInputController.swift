import Cocoa
import InputMethodKit

@objc(LeximeInputController)
class LeximeInputController: IMKInputController {

    private var composedKana: String = ""
    private var pendingRomaji: String = ""
    private var isComposing: Bool { !composedKana.isEmpty || !pendingRomaji.isEmpty }

    private let trie = RomajiTrie.shared

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

        var changed = true
        while !pendingRomaji.isEmpty && changed {
            changed = false
            let result = trie.lookup(pendingRomaji)

            switch result {
            case .exact(let kana):
                composedKana += kana
                pendingRomaji = ""
                changed = true

            case .exactAndPrefix:
                // Wait for more input — a longer match might exist
                break

            case .prefix:
                // Wait for more input
                break

            case .none:
                // Try to find the longest prefix match
                var found = false
                for len in stride(from: pendingRomaji.count - 1, through: 1, by: -1) {
                    let subEnd = pendingRomaji.index(pendingRomaji.startIndex, offsetBy: len)
                    let sub = String(pendingRomaji[pendingRomaji.startIndex..<subEnd])
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
                    // Check for double consonant → っ
                    if pendingRomaji.count >= 2 {
                        let chars = Array(pendingRomaji)
                        let first = chars[0]
                        let second = chars[1]
                        // Same consonant and not 'n' (nn → ん is handled by trie)
                        if first == second && first != "n" && first != "a" && first != "i" &&
                           first != "u" && first != "e" && first != "o" {
                            composedKana += "っ"
                            pendingRomaji = String(pendingRomaji.dropFirst())
                            changed = true
                        } else {
                            // "n" followed by a non-vowel, non-n, non-y consonant → ん
                            if first == "n" && second != "a" && second != "i" &&
                               second != "u" && second != "e" && second != "o" &&
                               second != "n" && second != "y" {
                                composedKana += "ん"
                                pendingRomaji = String(pendingRomaji.dropFirst())
                                changed = true
                            } else {
                                // Discard the first character
                                pendingRomaji = String(pendingRomaji.dropFirst())
                                changed = true
                            }
                        }
                    } else {
                        // Single unrecognized character — discard
                        pendingRomaji = String(pendingRomaji.dropFirst())
                        changed = true
                    }
                }
            }
        }

        updateMarkedText(client: client)
    }

    // MARK: - Flush & Commit

    private func flush(client: IMKTextInput) {
        // Convert any remaining pending romaji before committing
        var changed = true
        while !pendingRomaji.isEmpty && changed {
            changed = false

            // Try full pending first (with exactAndPrefix also accepted)
            for len in stride(from: pendingRomaji.count, through: 1, by: -1) {
                let subEnd = pendingRomaji.index(pendingRomaji.startIndex, offsetBy: len)
                let sub = String(pendingRomaji[pendingRomaji.startIndex..<subEnd])
                let subResult = trie.lookup(sub)

                switch subResult {
                case .exact(let kana), .exactAndPrefix(let kana):
                    composedKana += kana
                    pendingRomaji = String(pendingRomaji[subEnd...])
                    changed = true
                default:
                    continue
                }
                break
            }

            if !changed && !pendingRomaji.isEmpty {
                // Check double consonant
                if pendingRomaji.count >= 2 {
                    let chars = Array(pendingRomaji)
                    if chars[0] == chars[1] && chars[0] != "n" &&
                       !["a","i","u","e","o"].contains(chars[0]) {
                        composedKana += "っ"
                        pendingRomaji = String(pendingRomaji.dropFirst())
                        changed = true
                        continue
                    }
                }
                // Discard unrecognizable character
                composedKana += String(pendingRomaji.removeFirst())
                changed = true
            }
        }
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

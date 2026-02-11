import Foundation

struct RomajiConvertResult {
    var composedKana: String
    var pendingRomaji: String
}

private let vowels: Set<Character> = ["a", "i", "u", "e", "o"]

func drainPendingRomaji(
    composedKana: String,
    pendingRomaji: String,
    trie: RomajiTrie,
    force: Bool
) -> RomajiConvertResult {
    var composedKana = composedKana
    var pendingRomaji = pendingRomaji

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

        case .prefix:
            if !force { break }
            fallthrough

        case .none:
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
                    if first == second && first != "n" && !vowels.contains(first) {
                        composedKana += "っ"
                        pendingRomaji = String(pendingRomaji.dropFirst())
                        changed = true
                    } else if first == "n" && !vowels.contains(second) &&
                              second != "n" && second != "y" {
                        composedKana += "ん"
                        pendingRomaji = String(pendingRomaji.dropFirst())
                        changed = true
                    } else if force {
                        composedKana += String(pendingRomaji.removeFirst())
                        changed = true
                    } else {
                        // R1 fix: preserve unrecognized characters in composedKana
                        // instead of silently discarding them
                        composedKana += String(pendingRomaji.removeFirst())
                        changed = true
                    }
                } else {
                    // Single character remaining
                    if pendingRomaji == "n" {
                        if force {
                            composedKana += "ん"
                        }
                        // When not forced, "n" stays as pending (could be prefix of "na", "ni", etc.)
                    } else {
                        // R1 fix: preserve unrecognized single chars in composedKana
                        composedKana += pendingRomaji
                    }
                    pendingRomaji = ""
                    changed = true
                }
            }
        }
    }

    // Collapse latin consonant + kana vowel sequences (e.g., "kあ" → "か")
    if composedKana.contains(where: { $0.isASCII && $0.isLetter && $0.isLowercase }) {
        composedKana = collapseLatinKana(composedKana, trie: trie)
    }

    return RomajiConvertResult(composedKana: composedKana, pendingRomaji: pendingRomaji)
}

private let kanaVowelToRomaji: [Character: Character] = [
    "あ": "a", "い": "i", "う": "u", "え": "e", "お": "o",
]

func collapseLatinKana(_ input: String, trie: RomajiTrie) -> String {
    let chars = Array(input)
    var result = ""
    var i = 0

    while i < chars.count {
        let ch = chars[i]

        if ch.isASCII && ch.isLetter && ch.isLowercase {
            // Collect consecutive latin lowercase chars
            var j = i + 1
            while j < chars.count && chars[j].isASCII && chars[j].isLetter && chars[j].isLowercase {
                j += 1
            }

            // Check if followed by a kana vowel
            if j < chars.count, let vowel = kanaVowelToRomaji[chars[j]] {
                let latin = String(chars[i..<j])
                let candidate = latin + String(vowel)
                switch trie.lookup(candidate) {
                case .exact(let kana), .exactAndPrefix(let kana):
                    result += kana
                    i = j + 1
                    continue
                default:
                    break
                }
            }

            result.append(ch)
            i += 1
        } else {
            result.append(ch)
            i += 1
        }
    }

    return result
}

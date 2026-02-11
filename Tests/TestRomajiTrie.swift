import Foundation

func testRomajiTrie() {
    print("--- RomajiTrie Tests ---")
    let trie = RomajiTrie.shared

    // T2: Vowel exact match
    if case .exact(let kana) = trie.lookup("a") {
        assertEqual(kana, "あ", "vowel 'a'")
    } else {
        testsFailed += 1
        print("FAIL (vowel 'a'): expected .exact")
    }

    // T2: Prefix match
    if case .prefix = trie.lookup("k") {
        testsPassed += 1
    } else {
        testsFailed += 1
        print("FAIL (prefix 'k'): expected .prefix")
    }

    // T2: No match
    if case .none = trie.lookup("q") {
        testsPassed += 1
    } else {
        testsFailed += 1
        print("FAIL (none 'q'): expected .none")
    }

    // T2: Symbol
    if case .exact(let kana) = trie.lookup("-") {
        assertEqual(kana, "ー", "symbol '-'")
    } else {
        testsFailed += 1
        print("FAIL (symbol '-'): expected .exact")
    }

    // T2: 拗音
    if case .exact(let kana) = trie.lookup("sha") {
        assertEqual(kana, "しゃ", "youon 'sha'")
    } else {
        testsFailed += 1
        print("FAIL (youon 'sha'): expected .exact")
    }

    // T2: exactAndPrefix — "chi" matches ち but is also prefix for "cho" etc.
    switch trie.lookup("chi") {
    case .exact(let kana):
        assertEqual(kana, "ち", "chi exact")
    case .exactAndPrefix(let kana):
        assertEqual(kana, "ち", "chi exactAndPrefix")
    default:
        testsFailed += 1
        print("FAIL (chi): expected .exact or .exactAndPrefix")
    }

    // T2: "ka" exact match
    if case .exact(let kana) = trie.lookup("ka") {
        assertEqual(kana, "か", "ka")
    } else {
        testsFailed += 1
        print("FAIL (ka): expected .exact")
    }

    // T2: "sh" should be prefix
    if case .prefix = trie.lookup("sh") {
        testsPassed += 1
    } else {
        testsFailed += 1
        print("FAIL (prefix 'sh'): expected .prefix")
    }

    // T2: "nn" exact match for ん
    if case .exact(let kana) = trie.lookup("nn") {
        assertEqual(kana, "ん", "nn")
    } else {
        testsFailed += 1
        print("FAIL (nn): expected .exact")
    }
}

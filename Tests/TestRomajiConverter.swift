import Foundation

func testRomajiConverter() {
    print("--- RomajiConverter Tests ---")
    let trie = RomajiTrie.shared

    // T1: Basic "ka" → か
    do {
        let r = drainPendingRomaji(composedKana: "", pendingRomaji: "ka", trie: trie, force: false)
        assertEqual(r.composedKana, "か", "ka → か")
        assertEqual(r.pendingRomaji, "", "ka pending empty")
    }

    // T1: Sokuon "kk" → っ + pending k
    do {
        let r = drainPendingRomaji(composedKana: "", pendingRomaji: "kk", trie: trie, force: false)
        assertEqual(r.composedKana, "っ", "kk → っ")
        assertEqual(r.pendingRomaji, "k", "kk pending k")
    }

    // T1: Hatsuon "nk" → ん + pending k
    do {
        let r = drainPendingRomaji(composedKana: "", pendingRomaji: "nk", trie: trie, force: false)
        assertEqual(r.composedKana, "ん", "nk → ん")
        assertEqual(r.pendingRomaji, "k", "nk pending k")
    }

    // T1: "n" force=true → ん
    do {
        let r = drainPendingRomaji(composedKana: "", pendingRomaji: "n", trie: trie, force: true)
        assertEqual(r.composedKana, "ん", "n force → ん")
        assertEqual(r.pendingRomaji, "", "n force pending empty")
    }

    // T1: "n" force=false → pending n
    do {
        let r = drainPendingRomaji(composedKana: "", pendingRomaji: "n", trie: trie, force: false)
        assertEqual(r.composedKana, "", "n no-force composedKana empty")
        assertEqual(r.pendingRomaji, "n", "n no-force pending n")
    }

    // T1: Consecutive conversion "kakiku"
    do {
        let r = drainPendingRomaji(composedKana: "", pendingRomaji: "kakiku", trie: trie, force: false)
        assertEqual(r.composedKana, "かきく", "kakiku → かきく")
        assertEqual(r.pendingRomaji, "", "kakiku pending empty")
    }

    // T1: R1 fix — unrecognized character "q" preserved in composedKana
    do {
        let r = drainPendingRomaji(composedKana: "", pendingRomaji: "q", trie: trie, force: false)
        assertEqual(r.composedKana, "q", "q → composedKana q (R1 fix)")
        assertEqual(r.pendingRomaji, "", "q pending empty")
    }

    // T1: 3-char match "shi" → し
    do {
        let r = drainPendingRomaji(composedKana: "", pendingRomaji: "shi", trie: trie, force: false)
        assertEqual(r.composedKana, "し", "shi → し")
        assertEqual(r.pendingRomaji, "", "shi pending empty")
    }

    // T1: Existing composedKana is preserved
    do {
        let r = drainPendingRomaji(composedKana: "あ", pendingRomaji: "ka", trie: trie, force: false)
        assertEqual(r.composedKana, "あか", "あ + ka → あか")
    }

    // T1: "sha" → しゃ (拗音)
    do {
        let r = drainPendingRomaji(composedKana: "", pendingRomaji: "sha", trie: trie, force: false)
        assertEqual(r.composedKana, "しゃ", "sha → しゃ")
        assertEqual(r.pendingRomaji, "", "sha pending empty")
    }

    // T1: Mixed "kyouhaii" — partial conversion
    do {
        let r = drainPendingRomaji(composedKana: "", pendingRomaji: "kyouha", trie: trie, force: false)
        assertEqual(r.composedKana, "きょうは", "kyouha → きょうは")
        assertEqual(r.pendingRomaji, "", "kyouha pending empty")
    }

    // T1: Sokuon with subsequent kana "kka" → っか
    do {
        let r = drainPendingRomaji(composedKana: "", pendingRomaji: "kka", trie: trie, force: false)
        assertEqual(r.composedKana, "っか", "kka → っか")
        assertEqual(r.pendingRomaji, "", "kka pending empty")
    }

    // T1: Collapse latin+kana vowel — "kあ" → "か"
    do {
        let r = drainPendingRomaji(composedKana: "kあ", pendingRomaji: "", trie: trie, force: false)
        assertEqual(r.composedKana, "か", "kあ → か (collapse)")
    }

    // T1: Collapse latin+kana vowel — "あkい" → "あき"
    do {
        let r = drainPendingRomaji(composedKana: "あkい", pendingRomaji: "", trie: trie, force: false)
        assertEqual(r.composedKana, "あき", "あkい → あき (collapse mid)")
    }

    // T1: Collapse multi-char latin — "shあ" → "しゃ"
    do {
        let r = drainPendingRomaji(composedKana: "shあ", pendingRomaji: "", trie: trie, force: false)
        assertEqual(r.composedKana, "しゃ", "shあ → しゃ (collapse multi-latin)")
    }

    // T1: No collapse when latin not followed by kana vowel
    do {
        let r = drainPendingRomaji(composedKana: "kが", pendingRomaji: "", trie: trie, force: false)
        assertEqual(r.composedKana, "kが", "kが → kが (no collapse)")
    }
}

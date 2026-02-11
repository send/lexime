import Foundation

enum TrieLookupResult {
    case none
    case prefix
    case exact(String)
    case exactAndPrefix(String)
}

class RomajiTrie {
    class Node {
        var children: [Character: Node] = [:]
        var kana: String? = nil
    }

    static let shared = RomajiTrie()
    private let root = Node()

    private init() {
        // 母音
        insert("a", "あ")
        insert("i", "い")
        insert("u", "う")
        insert("e", "え")
        insert("o", "お")

        // か行
        insert("ka", "か")
        insert("ki", "き")
        insert("ku", "く")
        insert("ke", "け")
        insert("ko", "こ")

        // さ行
        insert("sa", "さ")
        insert("si", "し")
        insert("shi", "し")
        insert("su", "す")
        insert("se", "せ")
        insert("so", "そ")

        // た行
        insert("ta", "た")
        insert("ti", "ち")
        insert("chi", "ち")
        insert("tu", "つ")
        insert("tsu", "つ")
        insert("te", "て")
        insert("to", "と")

        // な行
        insert("na", "な")
        insert("ni", "に")
        insert("nu", "ぬ")
        insert("ne", "ね")
        insert("no", "の")

        // は行
        insert("ha", "は")
        insert("hi", "ひ")
        insert("hu", "ふ")
        insert("fu", "ふ")
        insert("he", "へ")
        insert("ho", "ほ")

        // ま行
        insert("ma", "ま")
        insert("mi", "み")
        insert("mu", "む")
        insert("me", "め")
        insert("mo", "も")

        // や行
        insert("ya", "や")
        insert("yu", "ゆ")
        insert("yo", "よ")

        // ら行
        insert("ra", "ら")
        insert("ri", "り")
        insert("ru", "る")
        insert("re", "れ")
        insert("ro", "ろ")

        // わ行
        insert("wa", "わ")
        insert("wi", "ゐ")
        insert("we", "ゑ")
        insert("wo", "を")

        // ん
        insert("nn", "ん")
        insert("n'", "ん")
        insert("xn", "ん")

        // が行
        insert("ga", "が")
        insert("gi", "ぎ")
        insert("gu", "ぐ")
        insert("ge", "げ")
        insert("go", "ご")

        // ざ行
        insert("za", "ざ")
        insert("zi", "じ")
        insert("ji", "じ")
        insert("zu", "ず")
        insert("ze", "ぜ")
        insert("zo", "ぞ")

        // だ行
        insert("da", "だ")
        insert("di", "ぢ")
        insert("du", "づ")
        insert("de", "で")
        insert("do", "ど")

        // ば行
        insert("ba", "ば")
        insert("bi", "び")
        insert("bu", "ぶ")
        insert("be", "べ")
        insert("bo", "ぼ")

        // ぱ行
        insert("pa", "ぱ")
        insert("pi", "ぴ")
        insert("pu", "ぷ")
        insert("pe", "ぺ")
        insert("po", "ぽ")

        // きゃ行（拗音）
        insert("kya", "きゃ")
        insert("kyi", "きぃ")
        insert("kyu", "きゅ")
        insert("kye", "きぇ")
        insert("kyo", "きょ")

        // しゃ行
        insert("sya", "しゃ")
        insert("sha", "しゃ")
        insert("syi", "しぃ")
        insert("syu", "しゅ")
        insert("shu", "しゅ")
        insert("sye", "しぇ")
        insert("she", "しぇ")
        insert("syo", "しょ")
        insert("sho", "しょ")

        // ちゃ行
        insert("tya", "ちゃ")
        insert("cha", "ちゃ")
        insert("tyi", "ちぃ")
        insert("tyu", "ちゅ")
        insert("chu", "ちゅ")
        insert("tye", "ちぇ")
        insert("che", "ちぇ")
        insert("tyo", "ちょ")
        insert("cho", "ちょ")

        // にゃ行
        insert("nya", "にゃ")
        insert("nyi", "にぃ")
        insert("nyu", "にゅ")
        insert("nye", "にぇ")
        insert("nyo", "にょ")

        // ひゃ行
        insert("hya", "ひゃ")
        insert("hyi", "ひぃ")
        insert("hyu", "ひゅ")
        insert("hye", "ひぇ")
        insert("hyo", "ひょ")

        // みゃ行
        insert("mya", "みゃ")
        insert("myi", "みぃ")
        insert("myu", "みゅ")
        insert("mye", "みぇ")
        insert("myo", "みょ")

        // りゃ行
        insert("rya", "りゃ")
        insert("ryi", "りぃ")
        insert("ryu", "りゅ")
        insert("rye", "りぇ")
        insert("ryo", "りょ")

        // ぎゃ行
        insert("gya", "ぎゃ")
        insert("gyi", "ぎぃ")
        insert("gyu", "ぎゅ")
        insert("gye", "ぎぇ")
        insert("gyo", "ぎょ")

        // じゃ行
        insert("ja", "じゃ")
        insert("jya", "じゃ")
        insert("zya", "じゃ")
        insert("ji", "じ")
        insert("jyi", "じぃ")
        insert("zyi", "じぃ")
        insert("ju", "じゅ")
        insert("jyu", "じゅ")
        insert("zyu", "じゅ")
        insert("je", "じぇ")
        insert("jye", "じぇ")
        insert("zye", "じぇ")
        insert("jo", "じょ")
        insert("jyo", "じょ")
        insert("zyo", "じょ")

        // ぢゃ行
        insert("dya", "ぢゃ")
        insert("dyi", "ぢぃ")
        insert("dyu", "ぢゅ")
        insert("dye", "ぢぇ")
        insert("dyo", "ぢょ")

        // びゃ行
        insert("bya", "びゃ")
        insert("byi", "びぃ")
        insert("byu", "びゅ")
        insert("bye", "びぇ")
        insert("byo", "びょ")

        // ぴゃ行
        insert("pya", "ぴゃ")
        insert("pyi", "ぴぃ")
        insert("pyu", "ぴゅ")
        insert("pye", "ぴぇ")
        insert("pyo", "ぴょ")

        // ふぁ行
        insert("fa", "ふぁ")
        insert("fi", "ふぃ")
        insert("fe", "ふぇ")
        insert("fo", "ふぉ")

        // てぃ etc.
        insert("thi", "てぃ")
        insert("tha", "てゃ")
        insert("thu", "てゅ")
        insert("the", "てぇ")
        insert("tho", "てょ")

        // でぃ etc.
        insert("dhi", "でぃ")
        insert("dha", "でゃ")
        insert("dhu", "でゅ")
        insert("dhe", "でぇ")
        insert("dho", "でょ")

        // つぁ行
        insert("tsa", "つぁ")
        insert("tsi", "つぃ")
        insert("tse", "つぇ")
        insert("tso", "つぉ")

        // ヴ行
        insert("va", "ゔぁ")
        insert("vi", "ゔぃ")
        insert("vu", "ゔ")
        insert("ve", "ゔぇ")
        insert("vo", "ゔぉ")

        // 小書きかな
        insert("xa", "ぁ")
        insert("xi", "ぃ")
        insert("xu", "ぅ")
        insert("xe", "ぇ")
        insert("xo", "ぉ")
        insert("xya", "ゃ")
        insert("xyu", "ゅ")
        insert("xyo", "ょ")
        insert("xtu", "っ")
        insert("xtsu", "っ")
        insert("xwa", "ゎ")
        insert("xka", "ゕ")
        insert("xke", "ゖ")

        // 小書き代替
        insert("la", "ぁ")
        insert("li", "ぃ")
        insert("lu", "ぅ")
        insert("le", "ぇ")
        insert("lo", "ぉ")
        insert("lya", "ゃ")
        insert("lyu", "ゅ")
        insert("lyo", "ょ")
        insert("ltu", "っ")
        insert("ltsu", "っ")
        insert("lwa", "ゎ")
        insert("lka", "ゕ")
        insert("lke", "ゖ")

        // ウィ・ウェ・ウォ
        insert("whi", "うぃ")
        insert("whe", "うぇ")
        insert("who", "うぉ")

        // 記号
        insert("-", "ー")

        // z-sequences (Mozc 互換)
        insert("zh", "←")
        insert("zj", "↓")
        insert("zk", "↑")
        insert("zl", "→")
        insert("z.", "…")
        insert("z,", "‥")
        insert("z/", "・")
        insert("z-", "〜")
        insert("z[", "『")
        insert("z]", "』")
    }

    func lookup(_ romaji: String) -> TrieLookupResult {
        var node = root
        for ch in romaji {
            guard let child = node.children[ch] else {
                return .none
            }
            node = child
        }
        let hasChildren = !node.children.isEmpty
        if let kana = node.kana {
            return hasChildren ? .exactAndPrefix(kana) : .exact(kana)
        } else {
            return hasChildren ? .prefix : .none
        }
    }

    private func insert(_ romaji: String, _ kana: String) {
        var node = root
        for ch in romaji {
            if node.children[ch] == nil {
                node.children[ch] = Node()
            }
            node = node.children[ch]!
        }
        node.kana = kana
    }
}

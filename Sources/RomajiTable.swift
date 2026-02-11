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

    // Romaji-to-kana mapping table: (romaji, kana)
    private static let mappings: [(String, String)] = [
        // 母音
        ("a", "あ"), ("i", "い"), ("u", "う"), ("e", "え"), ("o", "お"),
        // か行
        ("ka", "か"), ("ki", "き"), ("ku", "く"), ("ke", "け"), ("ko", "こ"),
        // さ行
        ("sa", "さ"), ("si", "し"), ("shi", "し"), ("su", "す"), ("se", "せ"), ("so", "そ"),
        // た行
        ("ta", "た"), ("ti", "ち"), ("chi", "ち"), ("tu", "つ"), ("tsu", "つ"),
        ("te", "て"), ("to", "と"),
        // な行
        ("na", "な"), ("ni", "に"), ("nu", "ぬ"), ("ne", "ね"), ("no", "の"),
        // は行
        ("ha", "は"), ("hi", "ひ"), ("hu", "ふ"), ("fu", "ふ"), ("he", "へ"), ("ho", "ほ"),
        // ま行
        ("ma", "ま"), ("mi", "み"), ("mu", "む"), ("me", "め"), ("mo", "も"),
        // や行
        ("ya", "や"), ("yu", "ゆ"), ("yo", "よ"),
        // ら行
        ("ra", "ら"), ("ri", "り"), ("ru", "る"), ("re", "れ"), ("ro", "ろ"),
        // わ行
        ("wa", "わ"), ("wi", "ゐ"), ("we", "ゑ"), ("wo", "を"),
        // ん
        ("nn", "ん"), ("n'", "ん"), ("xn", "ん"),
        // が行
        ("ga", "が"), ("gi", "ぎ"), ("gu", "ぐ"), ("ge", "げ"), ("go", "ご"),
        // ざ行
        ("za", "ざ"), ("zi", "じ"), ("ji", "じ"), ("zu", "ず"), ("ze", "ぜ"), ("zo", "ぞ"),
        // だ行
        ("da", "だ"), ("di", "ぢ"), ("du", "づ"), ("de", "で"), ("do", "ど"),
        // ば行
        ("ba", "ば"), ("bi", "び"), ("bu", "ぶ"), ("be", "べ"), ("bo", "ぼ"),
        // ぱ行
        ("pa", "ぱ"), ("pi", "ぴ"), ("pu", "ぷ"), ("pe", "ぺ"), ("po", "ぽ"),
        // きゃ行（拗音）
        ("kya", "きゃ"), ("kyi", "きぃ"), ("kyu", "きゅ"), ("kye", "きぇ"), ("kyo", "きょ"),
        // しゃ行
        ("sya", "しゃ"), ("sha", "しゃ"), ("syi", "しぃ"), ("syu", "しゅ"), ("shu", "しゅ"),
        ("sye", "しぇ"), ("she", "しぇ"), ("syo", "しょ"), ("sho", "しょ"),
        // ちゃ行
        ("tya", "ちゃ"), ("cha", "ちゃ"), ("tyi", "ちぃ"), ("tyu", "ちゅ"), ("chu", "ちゅ"),
        ("tye", "ちぇ"), ("che", "ちぇ"), ("tyo", "ちょ"), ("cho", "ちょ"),
        // にゃ行
        ("nya", "にゃ"), ("nyi", "にぃ"), ("nyu", "にゅ"), ("nye", "にぇ"), ("nyo", "にょ"),
        // ひゃ行
        ("hya", "ひゃ"), ("hyi", "ひぃ"), ("hyu", "ひゅ"), ("hye", "ひぇ"), ("hyo", "ひょ"),
        // みゃ行
        ("mya", "みゃ"), ("myi", "みぃ"), ("myu", "みゅ"), ("mye", "みぇ"), ("myo", "みょ"),
        // りゃ行
        ("rya", "りゃ"), ("ryi", "りぃ"), ("ryu", "りゅ"), ("rye", "りぇ"), ("ryo", "りょ"),
        // ぎゃ行
        ("gya", "ぎゃ"), ("gyi", "ぎぃ"), ("gyu", "ぎゅ"), ("gye", "ぎぇ"), ("gyo", "ぎょ"),
        // じゃ行
        ("ja", "じゃ"), ("jya", "じゃ"), ("zya", "じゃ"),
        ("ji", "じ"), ("jyi", "じぃ"), ("zyi", "じぃ"),
        ("ju", "じゅ"), ("jyu", "じゅ"), ("zyu", "じゅ"),
        ("je", "じぇ"), ("jye", "じぇ"), ("zye", "じぇ"),
        ("jo", "じょ"), ("jyo", "じょ"), ("zyo", "じょ"),
        // ぢゃ行
        ("dya", "ぢゃ"), ("dyi", "ぢぃ"), ("dyu", "ぢゅ"), ("dye", "ぢぇ"), ("dyo", "ぢょ"),
        // びゃ行
        ("bya", "びゃ"), ("byi", "びぃ"), ("byu", "びゅ"), ("bye", "びぇ"), ("byo", "びょ"),
        // ぴゃ行
        ("pya", "ぴゃ"), ("pyi", "ぴぃ"), ("pyu", "ぴゅ"), ("pye", "ぴぇ"), ("pyo", "ぴょ"),
        // ふぁ行
        ("fa", "ふぁ"), ("fi", "ふぃ"), ("fe", "ふぇ"), ("fo", "ふぉ"),
        // てぃ etc.
        ("thi", "てぃ"), ("tha", "てゃ"), ("thu", "てゅ"), ("the", "てぇ"), ("tho", "てょ"),
        // でぃ etc.
        ("dhi", "でぃ"), ("dha", "でゃ"), ("dhu", "でゅ"), ("dhe", "でぇ"), ("dho", "でょ"),
        // つぁ行
        ("tsa", "つぁ"), ("tsi", "つぃ"), ("tse", "つぇ"), ("tso", "つぉ"),
        // ヴ行
        ("va", "ゔぁ"), ("vi", "ゔぃ"), ("vu", "ゔ"), ("ve", "ゔぇ"), ("vo", "ゔぉ"),
        // 小書きかな
        ("xa", "ぁ"), ("xi", "ぃ"), ("xu", "ぅ"), ("xe", "ぇ"), ("xo", "ぉ"),
        ("xya", "ゃ"), ("xyu", "ゅ"), ("xyo", "ょ"),
        ("xtu", "っ"), ("xtsu", "っ"), ("xwa", "ゎ"), ("xka", "ゕ"), ("xke", "ゖ"),
        // 小書き代替
        ("la", "ぁ"), ("li", "ぃ"), ("lu", "ぅ"), ("le", "ぇ"), ("lo", "ぉ"),
        ("lya", "ゃ"), ("lyu", "ゅ"), ("lyo", "ょ"),
        ("ltu", "っ"), ("ltsu", "っ"), ("lwa", "ゎ"), ("lka", "ゕ"), ("lke", "ゖ"),
        // ウィ・ウェ・ウォ
        ("whi", "うぃ"), ("whe", "うぇ"), ("who", "うぉ"),
        // 記号
        ("-", "ー"),
        // z-sequences (Mozc 互換)
        ("zh", "←"), ("zj", "↓"), ("zk", "↑"), ("zl", "→"),
        ("z.", "…"), ("z,", "‥"), ("z/", "・"), ("z-", "〜"),
        ("z[", "『"), ("z]", "』"),
    ]

    private init() {
        for (romaji, kana) in RomajiTrie.mappings {
            insert(romaji, kana)
        }
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

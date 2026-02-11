import Foundation

enum InputState {
    case idle
    case composing
    case converting
}

struct ConversionSegment {
    let reading: String
    var surface: String
    var candidates: [String]
    var selectedIndex: Int
}

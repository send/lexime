import Foundation

enum InputState {
    case idle
    case composing
}

enum InputSubmode {
    case japanese   // romaji → kana conversion
    case english    // characters added directly to composedKana
}

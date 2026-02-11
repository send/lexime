import Foundation

var testsPassed = 0
var testsFailed = 0

func assertEqual<T: Equatable>(_ actual: T, _ expected: T,
                                _ message: String = "",
                                file: String = #file, line: Int = #line) {
    if actual == expected {
        testsPassed += 1
    } else {
        testsFailed += 1
        let label = message.isEmpty ? "" : " (\(message))"
        print("FAIL\(label): expected \(expected), got \(actual) [\(file):\(line)]")
    }
}

func assertTrue(_ condition: Bool,
                _ message: String = "",
                file: String = #file, line: Int = #line) {
    if condition {
        testsPassed += 1
    } else {
        testsFailed += 1
        let label = message.isEmpty ? "" : " (\(message))"
        print("FAIL\(label): condition was false [\(file):\(line)]")
    }
}

@main
struct TestMain {
    static func main() {
        testRomajiTrie()
        testRomajiConverter()

        print("\nResults: \(testsPassed) passed, \(testsFailed) failed")
        exit(testsFailed > 0 ? 1 : 0)
    }
}

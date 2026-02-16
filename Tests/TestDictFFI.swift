import Foundation

func testDictFFI() {
    print("--- Dict FFI Tests ---")

    // Open nonexistent path throws
    do {
        let _ = try LexDictionary.open(path: "/nonexistent/path/dict.bin")
        testsFailed += 1
        print("FAIL (open nonexistent): expected throw, got success")
    } catch {
        testsPassed += 1
    }
}

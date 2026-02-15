import Foundation

func testDictFFI() {
    print("--- Dict FFI Tests ---")

    // Open / close round-trip (nonexistent path returns nil)
    do {
        let dict = lex_dict_open("/nonexistent/path/dict.bin")
        assertTrue(dict == nil, "open nonexistent returns nil")
    }

    // Close null is safe (_Nullable parameter)
    do {
        lex_dict_close(nil)
        testsPassed += 1
    }
}

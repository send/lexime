import Foundation

func testDictFFI() {
    print("--- Dict FFI Tests ---")

    // Open / close round-trip (nonexistent path returns nil)
    do {
        let dict = lex_dict_open("/nonexistent/path/dict.bin")
        assertTrue(dict == nil, "open nonexistent returns nil")
    }

    // Lookup null dict returns empty
    do {
        let list = lex_dict_lookup(nil, "かんじ")
        assertEqual(list.len, 0, "null dict lookup returns empty")
        lex_candidates_free(list)
    }

    // Predict null dict returns empty
    do {
        let list = lex_dict_predict(nil, "かん", 10)
        assertEqual(list.len, 0, "null dict predict returns empty")
        lex_candidates_free(list)
    }

    // Close null is safe
    do {
        lex_dict_close(nil)
        testsPassed += 1
    }
}

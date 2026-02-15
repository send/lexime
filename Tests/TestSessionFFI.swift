import Foundation

func testSessionFFI() {
    print("--- Session FFI Tests ---")

    // Free null is safe (_Nullable parameter)
    do {
        lex_session_free(nil)
        testsPassed += 1
    }
}

import Foundation

func testSessionFFI() {
    print("--- Session FFI Tests ---")

    // Null dict → null session
    do {
        let session = lex_session_new(nil, nil, nil)
        assertTrue(session == nil, "null dict → null session")
    }

    // Null session operations are safe
    do {
        let resp = lex_session_handle_key(nil, 0, "", 0)
        assertEqual(resp.consumed, 0, "null session handle_key consumed=0")
        lex_key_response_free(resp)

        let resp2 = lex_session_commit(nil)
        assertEqual(resp2.consumed, 0, "null session commit consumed=0")
        lex_key_response_free(resp2)

        assertEqual(lex_session_is_composing(nil), 0, "null session is_composing=0")

        lex_session_free(nil)
        testsPassed += 1
    }
}

import Foundation

func testSessionFFI() {
    print("--- Session FFI Tests ---")

    // Version string is non-empty
    do {
        let version = engineVersion()
        assertTrue(!version.isEmpty, "engine version non-empty")
    }
}

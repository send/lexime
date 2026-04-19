import Foundation

func testSnippetTOML() {
    print("--- Snippet TOML Tests ---")

    // Round-trip the real user's default layout
    do {
        let source = """
            gh = "https://github.com/"
            gmail = "user@example.com"
            now = "$datetime"
            today = "$date"
            wareki = "$wareki"

            """
        let entries = try SnippetTOML.parse(source)
        assertEqual(entries.count, 5, "5 entries")
        assertEqual(entries[0].key, "gh", "sorted: gh first")
        assertEqual(entries[0].body, "https://github.com/", "gh body")
        assertEqual(entries[2].key, "now", "now key")
        assertEqual(entries[2].body, "$datetime", "now body preserves $var")

        let serialized = SnippetTOML.serialize(entries)
        assertEqual(serialized, source, "round-trip exact match")
    } catch {
        testsFailed += 1
        print("FAIL (basic round-trip): \(error)")
    }

    // Comments and blank lines are tolerated on parse
    do {
        let source = """
            # header comment
            a = "alpha"

            # middle comment
            b = "beta"  # inline comment
            """
        let entries = try SnippetTOML.parse(source)
        assertEqual(entries.count, 2, "comments stripped")
        assertEqual(entries[0].key, "a", "a key")
        assertEqual(entries[0].body, "alpha", "a body")
        assertEqual(entries[1].body, "beta", "b body (inline comment ignored)")
    } catch {
        testsFailed += 1
        print("FAIL (comments): \(error)")
    }

    // Escape sequences: \n, \t, \", \\
    do {
        let source = #"""
            multiline = "line1\nline2"
            tabbed = "a\tb"
            quoted = "say \"hi\""
            slashes = "\\path"
            """#
        let entries = try SnippetTOML.parse(source)
        let map = Dictionary(uniqueKeysWithValues: entries.map { ($0.key, $0.body) })
        assertEqual(map["multiline"], "line1\nline2", "\\n escape")
        assertEqual(map["tabbed"], "a\tb", "\\t escape")
        assertEqual(map["quoted"], "say \"hi\"", "\\\" escape")
        assertEqual(map["slashes"], "\\path", "\\\\ escape")

        // Round-trip through serializer
        let serialized = SnippetTOML.serialize(entries)
        let reparsed = try SnippetTOML.parse(serialized)
        let remap = Dictionary(uniqueKeysWithValues: reparsed.map { ($0.key, $0.body) })
        assertEqual(remap["multiline"], "line1\nline2", "round-trip \\n")
        assertEqual(remap["quoted"], "say \"hi\"", "round-trip \\\"")
    } catch {
        testsFailed += 1
        print("FAIL (escapes): \(error)")
    }

    // Non-ASCII passes through without escaping
    do {
        let entries = [LexSnippetEntry(key: "hi", body: "こんにちは")]
        let serialized = SnippetTOML.serialize(entries)
        assertEqual(serialized, "hi = \"こんにちは\"\n", "japanese body preserved")
        let reparsed = try SnippetTOML.parse(serialized)
        assertEqual(reparsed[0].body, "こんにちは", "japanese round-trip")
    } catch {
        testsFailed += 1
        print("FAIL (unicode): \(error)")
    }

    // Literal strings (single-quoted) — hand-edits should work
    do {
        let source = "k = 'raw \\n value'\n"
        let entries = try SnippetTOML.parse(source)
        assertEqual(entries.count, 1, "literal string parsed")
        assertEqual(entries[0].body, "raw \\n value", "literal string preserves backslash")
    } catch {
        testsFailed += 1
        print("FAIL (literal string): \(error)")
    }

    // Quoted keys with special chars
    do {
        let source = "\"key with space\" = \"v\"\n"
        let entries = try SnippetTOML.parse(source)
        assertEqual(entries.count, 1, "quoted key parsed")
        assertEqual(entries[0].key, "key with space", "quoted key value")

        // Serializer quotes non-bare keys
        let serialized = SnippetTOML.serialize(entries)
        assertEqual(serialized, "\"key with space\" = \"v\"\n", "non-bare key serialized quoted")
    } catch {
        testsFailed += 1
        print("FAIL (quoted key): \(error)")
    }

    // Unicode escape \u00A9
    do {
        let source = "c = \"\\u00A9\"\n"
        let entries = try SnippetTOML.parse(source)
        assertEqual(entries[0].body, "©", "unicode escape decoded")
    } catch {
        testsFailed += 1
        print("FAIL (unicode escape): \(error)")
    }

    // Errors: unterminated string
    do {
        _ = try SnippetTOML.parse("k = \"unclosed\n")
        testsFailed += 1
        print("FAIL: expected parse error for unterminated string")
    } catch {
        testsPassed += 1
    }

    // Errors: missing equals
    do {
        _ = try SnippetTOML.parse("k \"value\"\n")
        testsFailed += 1
        print("FAIL: expected parse error for missing '='")
    } catch {
        testsPassed += 1
    }

    // Errors: duplicate key
    do {
        _ = try SnippetTOML.parse("a = \"1\"\na = \"2\"\n")
        testsFailed += 1
        print("FAIL: expected parse error for duplicate key")
    } catch {
        testsPassed += 1
    }

    // Empty input produces empty list
    do {
        let entries = try SnippetTOML.parse("")
        assertEqual(entries.count, 0, "empty input")
        assertEqual(SnippetTOML.serialize([]), "", "empty serialize")
    } catch {
        testsFailed += 1
        print("FAIL (empty): \(error)")
    }
}

import Foundation

/// Minimal TOML parser and serializer for the snippets file format.
///
/// Only the subset actually emitted by the Lexime engine is supported:
///   - Flat `key = "value"` pairs (no tables, no arrays)
///   - Bare keys `[A-Za-z0-9_-]+` or quoted basic/literal string keys
///   - Basic strings `"..."` with standard escapes (`\"`, `\\`, `\n`, `\t`,
///     `\r`, `\b`, `\f`, `\uXXXX`, `\UXXXXXXXX`)
///   - Literal strings `'...'` with no escape handling
///   - `#` line comments and blank lines
///
/// Multi-line strings, integers, floats, dates, arrays, and inline tables are
/// intentionally unsupported — snippets.toml never contains them. If a user
/// hand-edits their file with unsupported syntax, parsing will throw a clear
/// error and the UI will surface it.
enum SnippetTOML {

    struct ParseError: Error, CustomStringConvertible {
        let line: Int
        let message: String
        var description: String { "line \(line): \(message)" }
    }

    /// Parse flat `key = "value"` TOML into snippet entries sorted by key.
    static func parse(_ source: String) throws -> [LexSnippetEntry] {
        var entries: [(String, String)] = []
        var seenKeys: Set<String> = []

        let lines = source.split(omittingEmptySubsequences: false, whereSeparator: { $0 == "\n" })
        for (index, rawLine) in lines.enumerated() {
            let lineNumber = index + 1
            let line = stripTrailingCR(rawLine)
            let trimmed = trimLeadingWhitespace(line)
            if trimmed.isEmpty || trimmed.first == "#" {
                continue
            }

            var cursor = trimmed.startIndex
            let key = try parseKey(trimmed, cursor: &cursor, line: lineNumber)
            skipInlineWhitespace(trimmed, cursor: &cursor)
            guard cursor < trimmed.endIndex, trimmed[cursor] == "=" else {
                throw ParseError(line: lineNumber, message: "expected '=' after key")
            }
            cursor = trimmed.index(after: cursor)
            skipInlineWhitespace(trimmed, cursor: &cursor)
            let value = try parseString(trimmed, cursor: &cursor, line: lineNumber)
            skipInlineWhitespace(trimmed, cursor: &cursor)

            if cursor < trimmed.endIndex, trimmed[cursor] != "#" {
                let column = trimmed.distance(from: trimmed.startIndex, to: cursor) + 1
                throw ParseError(
                    line: lineNumber,
                    message: "unexpected trailing content after value at column \(column)")
            }

            if seenKeys.contains(key) {
                throw ParseError(line: lineNumber, message: "duplicate key '\(key)'")
            }
            seenKeys.insert(key)
            entries.append((key, value))
        }

        entries.sort { $0.0 < $1.0 }
        return entries.map { LexSnippetEntry(key: $0.0, body: $0.1) }
    }

    /// Serialize entries to `key = "value"` lines, sorted by key, trailing
    /// newline after each entry. Round-trips with `parse`.
    static func serialize(_ entries: [LexSnippetEntry]) -> String {
        let sorted = entries.sorted { $0.key < $1.key }
        var out = ""
        for entry in sorted {
            out += formatKey(entry.key)
            out += " = "
            out += formatBasicString(entry.body)
            out += "\n"
        }
        return out
    }

    // MARK: - Key parsing

    private static func parseKey(
        _ line: Substring, cursor: inout Substring.Index, line lineNumber: Int
    ) throws -> String {
        guard cursor < line.endIndex else {
            throw ParseError(line: lineNumber, message: "expected key")
        }
        let ch = line[cursor]
        if ch == "\"" {
            return try parseBasicString(line, cursor: &cursor, line: lineNumber)
        }
        if ch == "'" {
            return try parseLiteralString(line, cursor: &cursor, line: lineNumber)
        }
        return try parseBareKey(line, cursor: &cursor, line: lineNumber)
    }

    private static func parseBareKey(
        _ line: Substring, cursor: inout Substring.Index, line lineNumber: Int
    ) throws -> String {
        let start = cursor
        while cursor < line.endIndex, isBareKeyChar(line[cursor]) {
            cursor = line.index(after: cursor)
        }
        let key = String(line[start..<cursor])
        if key.isEmpty {
            throw ParseError(line: lineNumber, message: "invalid key")
        }
        return key
    }

    private static func isBareKeyChar(_ ch: Character) -> Bool {
        guard let scalar = ch.unicodeScalars.first, ch.unicodeScalars.count == 1 else {
            return false
        }
        return (scalar >= "A" && scalar <= "Z")
            || (scalar >= "a" && scalar <= "z")
            || (scalar >= "0" && scalar <= "9")
            || scalar == "_" || scalar == "-"
    }

    // MARK: - String parsing

    private static func parseString(
        _ line: Substring, cursor: inout Substring.Index, line lineNumber: Int
    ) throws -> String {
        guard cursor < line.endIndex else {
            throw ParseError(line: lineNumber, message: "expected string value")
        }
        let ch = line[cursor]
        if ch == "\"" {
            return try parseBasicString(line, cursor: &cursor, line: lineNumber)
        }
        if ch == "'" {
            return try parseLiteralString(line, cursor: &cursor, line: lineNumber)
        }
        throw ParseError(
            line: lineNumber,
            message: "value must be a quoted string (found '\(ch)')")
    }

    private static func parseBasicString(
        _ line: Substring, cursor: inout Substring.Index, line lineNumber: Int
    ) throws -> String {
        // cursor is at opening "
        cursor = line.index(after: cursor)
        var out = ""
        while cursor < line.endIndex {
            let ch = line[cursor]
            if ch == "\"" {
                cursor = line.index(after: cursor)
                return out
            }
            if ch == "\\" {
                cursor = line.index(after: cursor)
                guard cursor < line.endIndex else {
                    throw ParseError(line: lineNumber, message: "unterminated escape sequence")
                }
                let esc = line[cursor]
                switch esc {
                case "\"": out.append("\"")
                case "\\": out.append("\\")
                case "n": out.append("\n")
                case "t": out.append("\t")
                case "r": out.append("\r")
                case "b": out.append("\u{08}")
                case "f": out.append("\u{0C}")
                case "u":
                    cursor = line.index(after: cursor)
                    out.append(try readHexUnicode(line, cursor: &cursor, digits: 4, line: lineNumber))
                    continue
                case "U":
                    cursor = line.index(after: cursor)
                    out.append(try readHexUnicode(line, cursor: &cursor, digits: 8, line: lineNumber))
                    continue
                default:
                    throw ParseError(
                        line: lineNumber,
                        message: "invalid escape sequence '\\\(esc)'")
                }
                cursor = line.index(after: cursor)
                continue
            }
            out.append(ch)
            cursor = line.index(after: cursor)
        }
        throw ParseError(line: lineNumber, message: "unterminated basic string")
    }

    private static func parseLiteralString(
        _ line: Substring, cursor: inout Substring.Index, line lineNumber: Int
    ) throws -> String {
        // cursor is at opening '
        cursor = line.index(after: cursor)
        let start = cursor
        while cursor < line.endIndex {
            if line[cursor] == "'" {
                let value = String(line[start..<cursor])
                cursor = line.index(after: cursor)
                return value
            }
            cursor = line.index(after: cursor)
        }
        throw ParseError(line: lineNumber, message: "unterminated literal string")
    }

    private static func readHexUnicode(
        _ line: Substring, cursor: inout Substring.Index, digits: Int, line lineNumber: Int
    ) throws -> Character {
        var hex = ""
        for _ in 0..<digits {
            guard cursor < line.endIndex else {
                throw ParseError(
                    line: lineNumber,
                    message: "expected \(digits) hex digits in unicode escape")
            }
            let ch = line[cursor]
            if !ch.isHexDigit {
                throw ParseError(
                    line: lineNumber,
                    message: "invalid hex digit '\(ch)' in unicode escape")
            }
            hex.append(ch)
            cursor = line.index(after: cursor)
        }
        guard let code = UInt32(hex, radix: 16), let scalar = Unicode.Scalar(code) else {
            throw ParseError(
                line: lineNumber,
                message: "invalid unicode scalar \\u\(hex)")
        }
        return Character(scalar)
    }

    // MARK: - Whitespace helpers

    private static func skipInlineWhitespace(_ line: Substring, cursor: inout Substring.Index) {
        while cursor < line.endIndex {
            let ch = line[cursor]
            if ch == " " || ch == "\t" {
                cursor = line.index(after: cursor)
            } else {
                return
            }
        }
    }

    private static func trimLeadingWhitespace(_ line: Substring) -> Substring {
        var idx = line.startIndex
        while idx < line.endIndex, line[idx] == " " || line[idx] == "\t" {
            idx = line.index(after: idx)
        }
        return line[idx...]
    }

    private static func stripTrailingCR(_ line: Substring) -> Substring {
        if line.last == "\r" {
            return line.dropLast()
        }
        return line
    }

    // MARK: - Serialization helpers

    private static func isBareKey(_ key: String) -> Bool {
        guard !key.isEmpty else { return false }
        return key.unicodeScalars.allSatisfy { s in
            (s >= "A" && s <= "Z") || (s >= "a" && s <= "z")
                || (s >= "0" && s <= "9") || s == "_" || s == "-"
        }
    }

    private static func formatKey(_ key: String) -> String {
        if isBareKey(key) {
            return key
        }
        return formatBasicString(key)
    }

    /// Serialize as a TOML basic string. Matches Rust `toml::Value::String`
    /// output: escapes `"`, `\`, and control chars; leaves non-ASCII as-is.
    private static func formatBasicString(_ value: String) -> String {
        var out = "\""
        for scalar in value.unicodeScalars {
            switch scalar {
            case "\"": out += "\\\""
            case "\\": out += "\\\\"
            case "\n": out += "\\n"
            case "\r": out += "\\r"
            case "\t": out += "\\t"
            case "\u{08}": out += "\\b"
            case "\u{0C}": out += "\\f"
            default:
                if scalar.value < 0x20 || scalar.value == 0x7F {
                    out += String(format: "\\u%04X", scalar.value)
                } else {
                    out.unicodeScalars.append(scalar)
                }
            }
        }
        out += "\""
        return out
    }
}

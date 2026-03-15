/// Tiny helper to switch macOS input source by ID.
/// Usage: lexime-select-input <input-source-id>
/// Example: lexime-select-input sh.send.inputmethod.Lexime.Japanese
import Carbon

guard CommandLine.arguments.count == 2 else {
    fputs("Usage: lexime-select-input <input-source-id>\n", stderr)
    exit(1)
}
let sourceID = CommandLine.arguments[1]

let conditions = [kTISPropertyInputSourceID as String: sourceID] as CFDictionary
for includeAll in [false, true] {
    guard let list = TISCreateInputSourceList(conditions, includeAll)?.takeRetainedValue()
            as? [TISInputSource],
          let source = list.first else {
        continue
    }
    let status = TISSelectInputSource(source)
    if status != noErr {
        fputs("TISSelectInputSource failed with status \(status)\n", stderr)
        exit(1)
    }
    exit(0)
}
fputs("Input source '\(sourceID)' not found\n", stderr)
exit(1)

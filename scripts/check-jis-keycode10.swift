/// Check if macOS JIS keyCode 10 bug is present.
/// Exit 0 = bug present (workaround needed), Exit 1 = bug fixed (workaround removable).
import Carbon
import Foundation

guard KBGetLayoutType(Int16(LMGetKbdType())) == kKeyboardJIS else {
    // Not a JIS keyboard — workaround irrelevant
    exit(0)
}
guard let source = TISCopyCurrentKeyboardLayoutInputSource()?.takeRetainedValue(),
      let dataRef = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData)
else { exit(0) }

let data = Unmanaged<CFData>.fromOpaque(dataRef).takeUnretainedValue() as Data
let ch: String = data.withUnsafeBytes { buf in
    guard let ptr = buf.baseAddress?.assumingMemoryBound(to: UCKeyboardLayout.self) else { return "?" }
    var dead: UInt32 = 0, len: Int = 0, chars = [UniChar](repeating: 0, count: 4)
    let s = UCKeyTranslate(
        ptr, 10, UInt16(kUCKeyActionDown), 0,
        UInt32(LMGetKbdType()), UInt32(kUCKeyTranslateNoDeadKeysMask),
        &dead, 4, &len, &chars)
    guard s == noErr, len > 0 else { return "?" }
    return String(utf16CodeUnits: chars, count: len)
}

if ch == "]" {
    print("""
    ╔═══════════════════════════════════════════════════════════╗
    ║  JIS keyCode 10 bug is FIXED — workaround can be removed ║
    ║  See: Sources/LeximeInputController.swift (jisKeyCodeFix) ║
    ╚═══════════════════════════════════════════════════════════╝
    """)
    exit(1)
}

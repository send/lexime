import Cocoa
import InputMethodKit

let kConnectionName = "sh.send.inputmethod.Lexime_Connection"

// Initialize shared app context (loads dictionary, connection matrix, user history)
_ = AppContext.shared

guard let bundleId = Bundle.main.bundleIdentifier else {
    NSLog("Lexime: Bundle.main.bundleIdentifier is nil")
    exit(1)
}

guard let server = IMKServer(name: kConnectionName, bundleIdentifier: bundleId) else {
    NSLog("Lexime: Failed to create IMKServer")
    exit(1)
}

NSLog("Lexime: IMKServer started (connection: %@)", kConnectionName)

_ = server  // keep the server alive

NSApplication.shared.run()

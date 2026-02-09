import Cocoa
import InputMethodKit

let kConnectionName = "dev.sendsh.inputmethod.Lexime_Connection"

guard let server = IMKServer(name: kConnectionName,
                             bundleIdentifier: Bundle.main.bundleIdentifier!) else {
    NSLog("Lexime: Failed to create IMKServer")
    exit(1)
}

NSLog("Lexime: IMKServer started (connection: %@)", kConnectionName)

_ = server  // keep the server alive

NSApplication.shared.run()

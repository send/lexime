import SwiftUI

struct SettingsView: View {

    @State private var developerMode = UserDefaults.standard.bool(forKey: "developerMode")

    var body: some View {
        TabView {
            UserDictionaryView()
                .tabItem { Label("ユーザ辞書", systemImage: "book") }

            if developerMode {
                DeveloperSettingsView()
                    .tabItem { Label("開発者", systemImage: "wrench") }
            }
        }
        .frame(minWidth: 480, minHeight: 360)
    }
}

// MARK: - Developer Settings

struct DeveloperSettingsView: View {

    @State private var conversionMode = UserDefaults.standard.integer(forKey: "conversionMode")
    @State private var romajiText = ""
    @State private var settingsText = ""
    @State private var needsRestart = false

    private let hasNeural = AppContext.shared.engine?.hasNeural() ?? false
    private let supportDir = AppContext.shared.supportDir

    var body: some View {
        Form {
            Section("変換モード") {
                Picker("モード", selection: $conversionMode) {
                    Text("Standard").tag(0)
                    Text("Predictive").tag(1)
                    if hasNeural {
                        Text("GhostText").tag(2)
                    }
                }
                .onChange(of: conversionMode) { newValue in
                    UserDefaults.standard.set(newValue, forKey: "conversionMode")
                }
            }

            Section("romaji.toml") {
                TextEditor(text: $romajiText)
                    .font(.system(.body, design: .monospaced))
                    .frame(minHeight: 120)
                HStack {
                    Button("保存") { saveFile(name: "romaji.toml", content: romajiText) }
                    Button("リセット") { loadRomaji() }
                }
            }

            Section("settings.toml") {
                TextEditor(text: $settingsText)
                    .font(.system(.body, design: .monospaced))
                    .frame(minHeight: 120)
                HStack {
                    Button("保存") { saveFile(name: "settings.toml", content: settingsText) }
                    Button("リセット") { loadSettings() }
                }
            }

            if needsRestart {
                Text("変更を適用するには Lexime の再起動が必要です")
                    .foregroundColor(.orange)
                    .font(.callout)
            }
        }
        .padding()
        .onAppear {
            loadRomaji()
            loadSettings()
        }
    }

    private func loadRomaji() {
        let path = supportDir + "/romaji.toml"
        romajiText = (try? String(contentsOfFile: path, encoding: .utf8)) ?? "# romaji.toml が見つかりません\n# mise run romaji-export で生成できます"
    }

    private func loadSettings() {
        let path = supportDir + "/settings.toml"
        settingsText = (try? String(contentsOfFile: path, encoding: .utf8)) ?? "# settings.toml が見つかりません\n# mise run settings-export で生成できます"
    }

    private func saveFile(name: String, content: String) {
        let path = supportDir + "/" + name
        do {
            try FileManager.default.createDirectory(
                atPath: supportDir, withIntermediateDirectories: true)
            try content.write(toFile: path, atomically: true, encoding: .utf8)
            needsRestart = true
            NSLog("Lexime: Saved %@", path)
        } catch {
            NSLog("Lexime: Failed to save %@: %@", path, "\(error)")
        }
    }
}

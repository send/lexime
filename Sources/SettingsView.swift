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
        .frame(minWidth: 520, minHeight: 500)
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

    private let editorHeight: CGFloat = 150

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                groupBox("変換モード") {
                    Picker("モード", selection: $conversionMode) {
                        Text("Standard").tag(0)
                        Text("Predictive").tag(1)
                        if hasNeural {
                            Text("GhostText").tag(2)
                        }
                    }
                    .onChange(of: conversionMode) { newValue in
                        UserDefaults.standard.set(newValue, forKey: "conversionMode")
                        needsRestart = true
                    }
                }

                groupBox("romaji.toml") {
                    TextEditor(text: $romajiText)
                        .font(.system(.body, design: .monospaced))
                        .frame(height: editorHeight)
                        .border(Color(nsColor: .separatorColor))
                    tomlButtons(
                        onSave: { saveFile(name: "romaji.toml", content: romajiText) },
                        onReload: { loadRomaji() },
                        onReset: { romajiText = romajiDefaultConfig() }
                    )
                }

                groupBox("settings.toml") {
                    TextEditor(text: $settingsText)
                        .font(.system(.body, design: .monospaced))
                        .frame(height: editorHeight)
                        .border(Color(nsColor: .separatorColor))
                    tomlButtons(
                        onSave: { saveFile(name: "settings.toml", content: settingsText) },
                        onReload: { loadSettings() },
                        onReset: { settingsText = settingsDefaultConfig() }
                    )
                }

                if needsRestart {
                    HStack {
                        Text("変更を適用するには再起動が必要です")
                            .foregroundColor(.orange)
                            .font(.callout)
                        Spacer()
                        Button("Lexime を再起動") {
                            NSLog("Lexime: Restarting via settings UI")
                            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                                exit(0)
                            }
                        }
                        .buttonStyle(.borderedProminent)
                        .tint(.orange)
                    }
                }
            }
            .padding(20)
        }
        .onAppear {
            loadRomaji()
            loadSettings()
        }
    }

    // MARK: - Components

    private func groupBox<Content: View>(
        _ title: String, @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.headline)
            content()
        }
    }

    private func tomlButtons(
        onSave: @escaping () -> Void,
        onReload: @escaping () -> Void,
        onReset: @escaping () -> Void
    ) -> some View {
        HStack(spacing: 8) {
            Button("保存") { onSave() }
                .buttonStyle(.borderedProminent)
            Button("再読み込み") { onReload() }
                .buttonStyle(.bordered)
            Button("デフォルトに戻す") { onReset() }
                .buttonStyle(.bordered)
        }
    }

    // MARK: - File I/O

    private func loadRomaji() {
        let path = supportDir + "/romaji.toml"
        romajiText = (try? String(contentsOfFile: path, encoding: .utf8))
            ?? "# romaji.toml が見つかりません\n# mise run romaji-export で生成できます"
    }

    private func loadSettings() {
        let path = supportDir + "/settings.toml"
        settingsText = (try? String(contentsOfFile: path, encoding: .utf8))
            ?? "# settings.toml が見つかりません\n# mise run settings-export で生成できます"
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


import SwiftUI

struct SettingsView: View {

    @State private var developerMode = UserDefaults.standard.bool(forKey: DefaultsKey.developerMode)

    var body: some View {
        TabView {
            UserDictionaryView()
                .tabItem { Label("ユーザ辞書", systemImage: "book") }

            SnippetView()
                .tabItem { Label("スニペット", systemImage: "text.snippet") }

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

    @State private var conversionMode = UserDefaults.standard.integer(forKey: DefaultsKey.conversionMode)
    @State private var romajiText = ""
    @State private var settingsText = ""
    @State private var needsRestart = false
    @State private var showResetConfirm = false

    private let supportDir = AppContext.shared.supportDir

    private let editorHeight: CGFloat = 150

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                groupBox("変換モード") {
                    Picker("モード", selection: $conversionMode) {
                        Text("Standard").tag(0)
                        Text("Predictive").tag(1)
                    }
                    .onChange(of: conversionMode) { newValue in
                        UserDefaults.standard.set(newValue, forKey: DefaultsKey.conversionMode)
                        needsRestart = true
                    }
                }

                groupBox("romaji.toml") {
                    TextEditor(text: $romajiText)
                        .font(.system(.body, design: .monospaced))
                        .frame(height: editorHeight)
                        .border(Color(nsColor: .separatorColor))
                        .accessibilityLabel("ローマ字設定エディタ")
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
                        .accessibilityLabel("設定エディタ")
                    tomlButtons(
                        onSave: { saveFile(name: "settings.toml", content: settingsText) },
                        onReload: { loadSettings() },
                        onReset: { settingsText = settingsDefaultConfig() }
                    )
                }

                groupBox("初期化") {
                    Text("設定ファイル・学習履歴をすべて削除し、組み込みデフォルトに戻します。")
                        .font(.callout)
                        .foregroundColor(.secondary)
                    Button("すべて初期化…") {
                        showResetConfirm = true
                    }
                    .buttonStyle(.bordered)
                    .foregroundColor(.red)
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
                                // IME processes are managed by launchd; exit(0) triggers automatic restart.
                                exit(0)
                            }
                        }
                        .buttonStyle(.borderedProminent)
                    }
                }
            }
            .padding(20)
        }
        .onAppear {
            loadRomaji()
            loadSettings()
        }
        .alert("すべて初期化", isPresented: $showResetConfirm) {
            Button("キャンセル", role: .cancel) {}
            Button("初期化", role: .destructive) { resetAll() }
        } message: {
            Text("設定ファイル（settings.toml, romaji.toml）と学習履歴を削除して再起動します。この操作は取り消せません。")
        }
    }

    // MARK: - Components

    private func groupBox<Content: View>(
        _ title: LocalizedStringKey, @ViewBuilder content: () -> Content
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

    // MARK: - Reset

    private func resetAll() {
        NSLog("Lexime: Resetting all settings and history")

        // 1. Clear learning history via engine (closes WAL handle + deletes files)
        if let engine = AppContext.shared.engine {
            do {
                try engine.clearHistory()
                NSLog("Lexime: History cleared")
            } catch {
                NSLog("Lexime: Failed to clear history: %@", "\(error)")
            }
        } else {
            NSLog("Lexime: Engine not available; skipping history clear")
        }

        // 2. Delete config files
        let fm = FileManager.default
        for name in ["settings.toml", "romaji.toml"] {
            let path = (supportDir as NSString).appendingPathComponent(name)
            if fm.fileExists(atPath: path) {
                do {
                    try fm.removeItem(atPath: path)
                    NSLog("Lexime: Deleted %@", path)
                } catch {
                    NSLog("Lexime: Failed to delete %@: %@", path, "\(error)")
                }
            }
        }

        // 3. Restart
        NSLog("Lexime: Restarting after reset")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
            // IME processes are managed by launchd; exit(0) triggers automatic restart.
            exit(0)
        }
    }

    // MARK: - File I/O

    private func loadRomaji() {
        let path = (supportDir as NSString).appendingPathComponent("romaji.toml")
        romajiText = (try? String(contentsOfFile: path, encoding: .utf8))
            ?? "# romaji.toml が見つかりません\n# mise run romaji-export で生成できます"
    }

    private func loadSettings() {
        let path = (supportDir as NSString).appendingPathComponent("settings.toml")
        settingsText = (try? String(contentsOfFile: path, encoding: .utf8))
            ?? "# settings.toml が見つかりません\n# mise run settings-export で生成できます"
    }

    private func saveFile(name: String, content: String) {
        let path = (supportDir as NSString).appendingPathComponent(name)
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


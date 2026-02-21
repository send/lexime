import SwiftUI

struct UserDictionaryView: View {

    @State private var words: [LexUserWord] = []
    @State private var showingAddSheet = false
    @State private var selectedIndex: Int?
    @State private var saveError: String?

    var body: some View {
        VStack(spacing: 0) {
            if words.isEmpty {
                Spacer()
                Text("ユーザ辞書は空です")
                    .foregroundColor(.secondary)
                Spacer()
            } else {
                List(selection: $selectedIndex) {
                    ForEach(Array(words.enumerated()), id: \.offset) { index, word in
                        HStack {
                            Text(word.reading)
                                .frame(maxWidth: .infinity, alignment: .leading)
                            Text(word.surface)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .tag(index)
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel("\(word.reading)、\(word.surface)")
                    }
                }
            }

            Divider()

            HStack {
                Button(action: { showingAddSheet = true }) {
                    Image(systemName: "plus")
                }
                .accessibilityLabel("単語を追加")
                Button(action: removeSelected) {
                    Image(systemName: "minus")
                }
                .accessibilityLabel("選択した単語を削除")
                .disabled(selectedIndex == nil)
                Spacer()
                Text("\(words.count) 語")
                    .foregroundColor(.secondary)
                    .font(.callout)
            }
            .padding(8)
        }
        .sheet(isPresented: $showingAddSheet) {
            AddWordSheet { reading, surface in
                addWord(reading: reading, surface: surface)
            }
        }
        .alert("保存エラー", isPresented: Binding(
            get: { saveError != nil },
            set: { if !$0 { saveError = nil } }
        )) {
            Button("OK") { saveError = nil }
        } message: {
            Text(saveError ?? "")
        }
        .onAppear { refresh() }
    }

    private func refresh() {
        words = AppContext.shared.engine?.listUserWords() ?? []
    }

    private func addWord(reading: String, surface: String) {
        guard let engine = AppContext.shared.engine else { return }
        let added = engine.registerWord(reading: reading, surface: surface)
        if added {
            do {
                try engine.saveUserDict(path: AppContext.shared.userDictPath)
            } catch {
                NSLog("Lexime: Failed to save user dict: %@", "\(error)")
                saveError = "辞書の保存に失敗しました: \(error.localizedDescription)"
            }
        }
        refresh()
    }

    private func removeSelected() {
        guard let engine = AppContext.shared.engine,
              let index = selectedIndex, index < words.count else { return }
        let word = words[index]
        _ = engine.unregisterWord(reading: word.reading, surface: word.surface)
        do {
            try engine.saveUserDict(path: AppContext.shared.userDictPath)
        } catch {
            NSLog("Lexime: Failed to save user dict: %@", "\(error)")
            saveError = "辞書の保存に失敗しました: \(error.localizedDescription)"
        }
        selectedIndex = nil
        refresh()
    }
}

// MARK: - Add Word Sheet

struct AddWordSheet: View {

    @Environment(\.dismiss) private var dismiss
    @State private var reading = ""
    @State private var surface = ""

    let onAdd: (String, String) -> Void

    var body: some View {
        VStack(spacing: 16) {
            Text("単語を追加")
                .font(.headline)
            Form {
                TextField("読み（ひらがな）", text: $reading)
                TextField("表層（漢字など）", text: $surface)
            }
            HStack {
                Button("キャンセル") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("追加") {
                    onAdd(reading, surface)
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(reading.isEmpty || surface.isEmpty)
            }
        }
        .padding()
        .frame(width: 320)
    }
}

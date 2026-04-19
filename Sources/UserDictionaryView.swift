import SwiftUI

struct UserDictionaryView: View {

    @State private var words: [LexUserWord] = []
    @State private var showingAddSheet = false
    @State private var selectedIndex: Int?
    @State private var saveError: String?

    private let service: UserDictionaryService

    init(service: UserDictionaryService? = nil) {
        self.service = service ?? AppContext.shared.makeUserDictionaryService()
    }

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
        words = service.list()
    }

    private func addWord(reading: String, surface: String) {
        do {
            try service.register(reading: reading, surface: surface)
            try service.save()
        } catch {
            NSLog("Lexime: Failed to register word: %@", "\(error)")
            saveError = "辞書の保存に失敗しました: \(error.localizedDescription)"
        }
        refresh()
    }

    private func removeSelected() {
        guard let index = selectedIndex, index < words.count else { return }
        let word = words[index]
        do {
            try service.unregister(reading: word.reading, surface: word.surface)
            try service.save()
        } catch {
            NSLog("Lexime: Failed to unregister word: %@", "\(error)")
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

    private var isReadingValid: Bool {
        !reading.isEmpty && reading.allSatisfy { c in
            // ひらがな (U+3040..U+309F) + 長音 (ー)
            (c >= "\u{3040}" && c <= "\u{309F}") || c == "ー"
        }
    }

    private var canAdd: Bool {
        isReadingValid && !surface.isEmpty
    }

    var body: some View {
        VStack(spacing: 16) {
            Text("単語を追加")
                .font(.headline)
            Form {
                TextField("読み（ひらがな）", text: $reading)
                if !reading.isEmpty && !isReadingValid {
                    Text("読みはひらがなで入力してください")
                        .font(.caption)
                        .foregroundColor(.red)
                }
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
                .disabled(!canAdd)
            }
        }
        .padding()
        .frame(width: 320)
    }
}

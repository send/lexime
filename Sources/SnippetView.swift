import SwiftUI

struct SnippetView: View {

    @State private var entries: [LexSnippetEntry] = []
    @State private var showingAddSheet = false
    @State private var selectedKey: String?
    @State private var saveError: String?

    private let service: SnippetService

    init(service: SnippetService? = nil) {
        self.service = service ?? AppContext.shared.makeSnippetService()
    }

    var body: some View {
        VStack(spacing: 0) {
            if entries.isEmpty {
                Spacer()
                Text("スニペットは空です")
                    .foregroundColor(.secondary)
                Spacer()
            } else {
                List(selection: $selectedKey) {
                    ForEach(entries, id: \.key) { entry in
                        HStack {
                            Text(entry.key)
                                .fontWeight(.medium)
                                .frame(width: 120, alignment: .leading)
                            Text(entry.body)
                                .foregroundColor(.secondary)
                                .lineLimit(1)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .tag(entry.key)
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel("\(entry.key)、\(entry.body)")
                    }
                }
            }

            Divider()

            HStack {
                Button(action: { showingAddSheet = true }) {
                    Image(systemName: "plus")
                }
                .accessibilityLabel("スニペットを追加")
                Button(action: removeSelected) {
                    Image(systemName: "minus")
                }
                .accessibilityLabel("選択したスニペットを削除")
                .disabled(selectedKey == nil)
                Spacer()
                Text("\(entries.count) 件")
                    .foregroundColor(.secondary)
                    .font(.callout)
            }
            .padding(8)
        }
        .sheet(isPresented: $showingAddSheet) {
            AddSnippetSheet { key, body in
                addEntry(key: key, body: body)
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
        do {
            entries = try service.load()
        } catch {
            NSLog("Lexime: Failed to parse snippets.toml: %@", "\(error)")
            saveError = "スニペットの読み込みに失敗しました: \(error.localizedDescription)"
        }
    }

    private func addEntry(key: String, body: String) {
        // Avoid duplicate keys — overwrite if exists
        entries.removeAll { $0.key == key }
        entries.append(LexSnippetEntry(key: key, body: body))
        entries.sort { $0.key < $1.key }
        persist()
    }

    private func removeSelected() {
        guard let key = selectedKey else { return }
        entries.removeAll { $0.key == key }
        selectedKey = nil
        persist()
    }

    private func persist() {
        do {
            try service.save(entries)
            NSLog("Lexime: Saved snippets.toml")
        } catch {
            NSLog("Lexime: Failed to save snippets.toml: %@", "\(error)")
            saveError = "スニペットの保存に失敗しました: \(error.localizedDescription)"
            return
        }
        do {
            try service.reload()
        } catch {
            NSLog("Lexime: Failed to reload snippets.toml: %@", "\(error)")
            saveError = "保存は成功しましたが、再読み込みに失敗しました: \(error.localizedDescription)"
        }
    }
}

// MARK: - Add Snippet Sheet

struct AddSnippetSheet: View {

    @Environment(\.dismiss) private var dismiss
    @State private var key = ""
    @State private var snippetBody = ""

    let onAdd: (String, String) -> Void

    private var isKeyValid: Bool {
        !key.isEmpty && key.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "_" || $0 == "-") }
    }

    private var canAdd: Bool {
        isKeyValid && !snippetBody.isEmpty
    }

    var body: some View {
        VStack(spacing: 16) {
            Text("スニペットを追加")
                .font(.headline)
            Form {
                TextField("キー（英数字・ハイフン・アンダースコア）", text: $key)
                if !key.isEmpty && !isKeyValid {
                    Text("キーは英数字とハイフン・アンダースコアのみ使用できます")
                        .font(.caption)
                        .foregroundColor(.red)
                }
                TextField("展開テキスト", text: $snippetBody)
            }
            HStack {
                Button("キャンセル") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("追加") {
                    onAdd(key, snippetBody)
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!canAdd)
            }
        }
        .padding()
        .frame(width: 400)
    }
}

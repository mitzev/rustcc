// The IDE's SwiftUI view tree. Pure SwiftUI — every action calls into
// the Rust engine via `IDEEngine` (the only FFI touch-point).

import SwiftUI
import AppKit

struct ContentView: View {
    @StateObject private var eng = IDEEngine()

    var body: some View {
        NavigationSplitView {
            // Sidebar: the open project's file list.
            List(eng.files, id: \.self, selection: Binding(
                get: { eng.openRel },
                set: { if let rel = $0 { eng.openFile(rel) } }
            )) { rel in
                Label(rel, systemImage: icon(for: rel)).tag(rel)
            }
            .navigationTitle("Files")
            .frame(minWidth: 200)
        } detail: {
            VSplitView {
                // Editor
                Group {
                    if eng.openRel != nil {
                        TextEditor(text: $eng.source)
                            .font(.system(.body, design: .monospaced))
                            .autocorrectionDisabled()
                    } else {
                        ContentUnavailableView(
                            "rustcc SwiftUI IDE",
                            systemImage: "swift",
                            description: Text("New ▸ a Host project, or Open a folder. "
                                + "The engine is fork-Rust, reached over extern \"Swift\".")
                        )
                    }
                }
                .frame(minHeight: 240)

                // Console
                VStack(alignment: .leading, spacing: 0) {
                    HStack {
                        Text("Console").font(.headline)
                        if eng.running {
                            ProgressView().controlSize(.small).padding(.leading, 4)
                        }
                        Spacer()
                        Button("Clear") { eng.clearConsole() }.buttonStyle(.borderless)
                    }
                    .padding(.horizontal, 8).padding(.vertical, 4)
                    ScrollViewReader { proxy in
                        ScrollView {
                            Text(eng.console.isEmpty ? "—" : eng.console)
                                .font(.system(.caption, design: .monospaced))
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .textSelection(.enabled)
                                .padding(8)
                                .id("end")
                        }
                        .onChange(of: eng.console) { _, _ in
                            proxy.scrollTo("end", anchor: .bottom)
                        }
                    }
                }
                .frame(minHeight: 120)
                .background(Color(nsColor: .textBackgroundColor))
            }
            .navigationTitle(eng.openRel ?? "rustcc IDE")
            .toolbar { toolbar }
        }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItemGroup(placement: .primaryAction) {
            Button { newHostProject() } label: { Label("New Host", systemImage: "doc.badge.plus") }
            Button { openProject() } label: { Label("Open", systemImage: "folder") }
            Button { eng.save() } label: { Label("Save", systemImage: "square.and.arrow.down") }
                .disabled(eng.openRel == nil)
            Divider()
            Picker("Target", selection: $eng.target) {
                ForEach(Array(eng.targets.enumerated()), id: \.offset) { i, name in
                    Text(name).tag(i)
                }
            }
            .frame(maxWidth: 280)
            Button { eng.build(run: false) } label: { Label("Build", systemImage: "hammer") }
                .disabled(eng.running || eng.projectDir == nil)
            Button { eng.build(run: true) } label: { Label("Run", systemImage: "play.fill") }
                .disabled(eng.running || eng.projectDir == nil)
        }
    }

    private func icon(for rel: String) -> String {
        if rel.hasSuffix(".rs") { return "r.square" }
        if rel.hasSuffix(".toml") || rel.hasSuffix(".json") { return "gearshape" }
        if rel.hasSuffix(".md") { return "doc.text" }
        return "doc"
    }

    // MARK: - AppKit folder pickers

    private func newHostProject() {
        if let dir = pickFolder(prompt: "Create Host Project In…") {
            eng.scaffoldHost(into: dir)
        }
    }

    private func openProject() {
        if let dir = pickFolder(prompt: "Open Project Folder") {
            eng.open(dir)
        }
    }

    private func pickFolder(prompt: String) -> String? {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = prompt
        panel.message = prompt
        return panel.runModal() == .OK ? panel.url?.path : nil
    }
}

// The IDE's SwiftUI view tree. Pure SwiftUI — every action calls into
// the Rust engine via `IDEEngine` (the only FFI touch-point).

import SwiftUI
import AppKit

struct ContentView: View {
    @StateObject private var eng = IDEEngine()
    @State private var bpLine: Int = 1
    @State private var showFind = false
    @State private var findText = ""
    @State private var replaceText = ""
    @State private var editorSelection: TextSelection? = nil
    @State private var findAnchor: String.Index? = nil

    var body: some View {
        NavigationSplitView {
            List(eng.files, id: \.self, selection: Binding(
                get: { eng.openRel },
                set: { if let rel = $0 { eng.openFile(rel) } }
            )) { rel in
                Label(rel, systemImage: icon(for: rel)).tag(rel)
            }
            .navigationTitle("Files")
            .frame(minWidth: 200)
        } detail: {
            VStack(spacing: 0) {
                if !eng.openFiles.isEmpty {
                    tabStrip
                    Divider()
                }
                if showFind {
                    findBar
                    Divider()
                }
                VSplitView {
                    editorPane.frame(minHeight: 220)
                    consolePane.frame(minHeight: 110)
                }
                Divider()
                debugBar
            }
            .navigationTitle(eng.openRel ?? "rustcc IDE")
            .toolbar { toolbar }
            .onAppear { eng.refreshBreakpoints() }
        }
    }

    // MARK: - Find / Replace

    private var findBar: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField("Find", text: $findText)
                .textFieldStyle(.roundedBorder).frame(width: 180)
                .onChange(of: findText) { _, _ in findAnchor = nil }
                .onSubmit { findNext() }
            Text(findText.isEmpty ? "" : "\(matchCount)")
                .font(.caption).foregroundStyle(.secondary).frame(minWidth: 24)
            Button("Next") { findNext() }.disabled(findText.isEmpty)
            Divider().frame(height: 16)
            TextField("Replace", text: $replaceText)
                .textFieldStyle(.roundedBorder).frame(width: 180)
            Button("Replace All") { replaceAll() }.disabled(findText.isEmpty || matchCount == 0)
            Spacer()
            Button("Done") { showFind = false }
        }
        .padding(.horizontal, 10).padding(.vertical, 6)
        .background(Color(nsColor: .windowBackgroundColor))
    }

    private var matchCount: Int {
        findText.isEmpty ? 0 : eng.source.components(separatedBy: findText).count - 1
    }

    /// Select the next match (wrapping), scrolling the editor to it via
    /// the selection binding. Anchor advances so repeated Next walks
    /// through all matches.
    private func findNext() {
        guard !findText.isEmpty else { return }
        let s = eng.source
        let from = findAnchor ?? s.startIndex
        let hit = s.range(of: findText, range: from..<s.endIndex) ?? s.range(of: findText)
        if let r = hit {
            editorSelection = TextSelection(range: r)
            findAnchor = r.upperBound
        }
    }

    private func replaceAll() {
        guard !findText.isEmpty else { return }
        eng.source = eng.source.replacingOccurrences(of: findText, with: replaceText)
        findAnchor = nil
    }

    // MARK: - Tabs

    private var tabStrip: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 2) {
                ForEach(eng.openFiles, id: \.self) { rel in
                    let active = rel == eng.openRel
                    HStack(spacing: 4) {
                        Text(base(rel)).font(.caption)
                            .fontWeight(active ? .semibold : .regular)
                        Button {
                            eng.closeFile(rel)
                        } label: {
                            Image(systemName: "xmark").font(.system(size: 8))
                        }
                        .buttonStyle(.borderless)
                    }
                    .padding(.horizontal, 8).padding(.vertical, 4)
                    .background(active ? Color.accentColor.opacity(0.22) : Color.clear)
                    .clipShape(RoundedRectangle(cornerRadius: 5))
                    .contentShape(Rectangle())
                    .onTapGesture { eng.openFile(rel) }
                }
            }
            .padding(.horizontal, 6).padding(.vertical, 3)
        }
        .background(Color(nsColor: .windowBackgroundColor))
    }

    private func base(_ rel: String) -> String {
        rel.split(separator: "/").last.map(String.init) ?? rel
    }

    // MARK: - Panes

    private var editorPane: some View {
        Group {
            if eng.openRel != nil {
                TextEditor(text: $eng.source, selection: $editorSelection)
                    .font(.system(.body, design: .monospaced))
                    .autocorrectionDisabled()
            } else {
                ContentUnavailableView(
                    "rustcc SwiftUI IDE",
                    systemImage: "swift",
                    description: Text("New ▸ a Host project, or Open a folder. The "
                        + "engine is fork-Rust, reached over extern \"Swift\".")
                )
            }
        }
    }

    private var consolePane: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Console").font(.headline)
                if eng.running { ProgressView().controlSize(.small).padding(.leading, 4) }
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
                .onChange(of: eng.console) { _, _ in proxy.scrollTo("end", anchor: .bottom) }
            }
        }
        .background(Color(nsColor: .textBackgroundColor))
    }

    // MARK: - Debug bar (host target only)

    private var debugBar: some View {
        HStack(spacing: 8) {
            Image(systemName: "ladybug").foregroundStyle(eng.debugActive ? .green : .secondary)

            if eng.debugActive {
                Button { eng.dbgStop() } label: { Label("Stop", systemImage: "stop.fill") }
                ControlGroup {
                    Button { eng.stepOver() } label: { Image(systemName: "arrow.turn.down.right") }
                    Button { eng.stepInto() } label: { Image(systemName: "arrow.down.to.line") }
                    Button { eng.stepOut() } label: { Image(systemName: "arrow.up.to.line") }
                    Button { eng.continueRun() } label: { Image(systemName: "play.fill") }
                    Button { eng.variables() } label: { Image(systemName: "list.bullet.rectangle") }
                }
                .frame(width: 190)
                if let f = eng.stopFile, let l = eng.stopLine {
                    Text("⏸ \(f):\(l)")
                        .font(.caption.monospaced())
                        .foregroundStyle(.orange)
                }
            } else {
                Button { eng.dbgStart() } label: { Label("Start Debug", systemImage: "ladybug.fill") }
                    .disabled(eng.projectDir == nil || eng.target != 0)
                Text(eng.target == 0 ? "host lldb" : "host target only")
                    .font(.caption).foregroundStyle(.secondary)
            }

            Spacer()

            // Breakpoints: SwiftUI's TextEditor has no gutter/cursor
            // API, so place them by line number.
            Stepper("BP line \(bpLine)", value: $bpLine, in: 1...100_000)
                .fixedSize()
            Button("Toggle BP") {
                if let rel = eng.openRel { eng.toggleBreakpoint(rel: rel, line: bpLine) }
            }
            .disabled(eng.openRel == nil)
            ForEach(eng.breakpoints, id: \.self) { bp in
                Button {
                    if let c = bp.lastIndex(of: ":"), let n = Int(bp[bp.index(after: c)...]) {
                        eng.toggleBreakpoint(rel: String(bp[..<c]), line: n)
                    }
                } label: {
                    Text("● \(shortBP(bp))").font(.caption2.monospaced())
                }
                .buttonStyle(.borderless).foregroundStyle(.red)
                .help("Remove breakpoint \(bp)")
            }
        }
        .padding(.horizontal, 10).padding(.vertical, 6)
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItemGroup(placement: .primaryAction) {
            Menu {
                Button("Host Project…") { newProject { eng.scaffoldHost(into: $0) } }
                Button("RAK11161 RTOS Project…") { newProject { eng.scaffoldRTOS(into: $0) } }
            } label: {
                Label("New", systemImage: "doc.badge.plus")
            }
            Button { openProject() } label: { Label("Open", systemImage: "folder") }
            Button { eng.save() } label: { Label("Save", systemImage: "square.and.arrow.down") }
                .disabled(eng.openRel == nil)
            Button { showFind.toggle() } label: { Label("Find", systemImage: "magnifyingglass") }
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
            Button { eng.upload() } label: { Label("Upload", systemImage: "bolt.horizontal.circle") }
                .disabled(eng.running || eng.projectDir == nil || !eng.canUpload)
                .help("Flash the built firmware via upload.toml (RTOS targets)")
        }
    }

    // MARK: - Helpers

    private func shortBP(_ bp: String) -> String {
        guard let c = bp.lastIndex(of: ":") else { return bp }
        let base = bp[..<c].split(separator: "/").last.map(String.init) ?? String(bp[..<c])
        return base + bp[c...]
    }

    private func icon(for rel: String) -> String {
        if rel.hasSuffix(".rs") { return "r.square" }
        if rel.hasSuffix(".toml") || rel.hasSuffix(".json") { return "gearshape" }
        if rel.hasSuffix(".md") { return "doc.text" }
        return "doc"
    }

    private func newProject(_ scaffold: (String) -> Void) {
        if let dir = pickFolder(prompt: "Create Project In…") { scaffold(dir) }
    }
    private func openProject() {
        if let dir = pickFolder(prompt: "Open Project Folder") { eng.open(dir) }
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

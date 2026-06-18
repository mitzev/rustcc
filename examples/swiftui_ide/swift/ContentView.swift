// The IDE's SwiftUI view tree. Pure SwiftUI — every action calls into
// the Rust engine via `IDEEngine` (the only FFI touch-point).

import SwiftUI
import AppKit

struct ContentView: View {
    @StateObject private var eng = IDEEngine()
    @State private var showFind = false
    @State private var findText = ""
    @State private var replaceText = ""
    @State private var selectRange: NSRange? = nil
    @State private var findAnchor: Int = 0   // NSString offset for Find Next
    @State private var showVars = false
    @State private var baud = [115_200, 115_200]   // per channel
    @State private var serialSend = ["", ""]

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
                    editorPane.frame(minHeight: 200)
                    consolePane.frame(minHeight: 90)
                    if eng.serialOpen[0] { serialPane(0).frame(minHeight: 70) }
                    if eng.serialOpen[1] { serialPane(1).frame(minHeight: 70) }
                }
                Divider()
                debugBar
                Divider()
                serialBar
            }
            .navigationTitle(eng.openRel ?? "rustcc IDE")
            .toolbar { toolbar }
            .onAppear { eng.refreshBreakpoints() }
            .inspector(isPresented: $showVars) { varsInspector }
        }
    }

    // MARK: - Serial monitor

    private let bauds = [9_600, 19_200, 57_600, 115_200, 230_400, 460_800, 921_600]

    // Two serial channels — one per core on a dual-target board.
    private var serialBar: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 6) {
                Text("Serial monitors").font(.caption.bold()).foregroundStyle(.secondary)
                Button { eng.refreshPorts() } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.borderless).help("Rescan serial ports")
                Spacer()
            }
            serialRow(0, "Core A")
            serialRow(1, "Core B")
        }
        .padding(.horizontal, 10).padding(.vertical, 5)
    }

    private func serialRow(_ ch: Int, _ label: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "cable.connector")
                .foregroundStyle(eng.serialOpen[ch] ? .green : .secondary)
            Text(label).font(.caption).frame(width: 50, alignment: .leading)
            Picker("", selection: Binding(
                get: { eng.serialPort[ch] }, set: { eng.selectPort(ch, $0) }
            )) {
                Text("— no port —").tag("")
                ForEach(eng.serialPorts, id: \.self) { p in
                    Text(p.replacingOccurrences(of: "/dev/", with: "")).tag(p)
                }
            }
            .labelsHidden().frame(maxWidth: 220).disabled(eng.serialOpen[ch])
            Picker("", selection: Binding(get: { baud[ch] }, set: { baud[ch] = $0 })) {
                ForEach(bauds, id: \.self) { Text("\($0)").tag($0) }
            }
            .labelsHidden().frame(maxWidth: 100).disabled(eng.serialOpen[ch])

            if eng.serialOpen[ch] {
                Button { eng.closeSerial(ch) } label: { Label("Disconnect", systemImage: "xmark.circle") }
                TextField("send…", text: Binding(get: { serialSend[ch] }, set: { serialSend[ch] = $0 }))
                    .textFieldStyle(.roundedBorder).frame(width: 180)
                    .onSubmit { eng.sendSerial(ch, serialSend[ch]); serialSend[ch] = "" }
            } else {
                Button { eng.openSerial(ch, baud: baud[ch]) } label: {
                    Label("Connect", systemImage: "cable.connector.horizontal")
                }
                .disabled(eng.serialPort[ch].isEmpty)
            }
            Spacer()
        }
    }

    private func serialPane(_ ch: Int) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Serial — \(ch == 0 ? "Core A" : "Core B")").font(.headline)
                Spacer()
                Button("Clear") { eng.clearSerial(ch) }.buttonStyle(.borderless)
            }
            .padding(.horizontal, 8).padding(.vertical, 4)
            ScrollViewReader { proxy in
                ScrollView {
                    Text(eng.serialRx[ch].isEmpty ? "—" : eng.serialRx[ch])
                        .font(.system(.caption, design: .monospaced))
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .textSelection(.enabled)
                        .padding(8)
                        .id("send\(ch)")
                }
                .onChange(of: eng.serialRx[ch]) { _, _ in proxy.scrollTo("send\(ch)", anchor: .bottom) }
            }
        }
        .background(Color(nsColor: .textBackgroundColor))
    }

    // MARK: - Variables inspector

    private var varsInspector: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Label("Variables", systemImage: "list.bullet.rectangle").font(.headline)
                Spacer()
                Button {
                    eng.requestVars()
                } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(.borderless).disabled(!eng.debugActive)
                    .help("Re-capture frame variables")
            }
            .padding(.horizontal, 10).padding(.vertical, 6)
            Divider()
            ScrollView {
                Text(varsText)
                    .font(.system(.caption, design: .monospaced))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
                    .padding(8)
            }
        }
        .inspectorColumnWidth(min: 220, ideal: 300, max: 480)
    }

    private var varsText: String {
        if !eng.debugActive { return "(no debug session — Start Debug, then stop at a breakpoint)" }
        let v = eng.variables.trimmingCharacters(in: .whitespacesAndNewlines)
        return v.isEmpty ? "(no locals in this frame — step into a call)" : eng.variables
    }

    // MARK: - Find / Replace

    private var findBar: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
            TextField("Find", text: $findText)
                .textFieldStyle(.roundedBorder).frame(width: 180)
                .onChange(of: findText) { _, _ in findAnchor = 0 }
                .onSubmit { findNext() }
            Text(findText.isEmpty ? "" : "\(matchCount)")
                .font(.caption).foregroundStyle(.secondary).frame(minWidth: 24)
            Button("Next") { findNext() }.disabled(findText.isEmpty)
            Divider().frame(height: 16)
            TextField("Replace", text: $replaceText)
                .textFieldStyle(.roundedBorder).frame(width: 180)
            Button("Replace") { replaceCurrent() }.disabled(findText.isEmpty || matchCount == 0)
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

    /// Select the next match (wrapping), scrolling the editor to it.
    /// The anchor advances so repeated Next walks all matches.
    private func findNext() {
        guard !findText.isEmpty else { return }
        let ns = eng.source as NSString
        let from = findAnchor <= ns.length ? findAnchor : 0
        var r = ns.range(of: findText, options: [], range: NSRange(location: from, length: ns.length - from))
        if r.location == NSNotFound { r = ns.range(of: findText) } // wrap
        if r.location != NSNotFound {
            selectRange = r
            findAnchor = r.location + r.length
        }
    }

    /// Replace the current match (the one Find Next selected), then
    /// advance to the next. Falls back to Find Next if nothing is
    /// currently on a match.
    private func replaceCurrent() {
        guard !findText.isEmpty else { return }
        let ns = eng.source as NSString
        if let r = selectRange, r.location != NSNotFound, NSMaxRange(r) <= ns.length,
            ns.substring(with: r) == findText
        {
            eng.source = ns.replacingCharacters(in: r, with: replaceText)
            findAnchor = r.location + (replaceText as NSString).length
            selectRange = nil
        }
        findNext()
    }

    private func replaceAll() {
        guard !findText.isEmpty else { return }
        eng.source = eng.source.replacingOccurrences(of: findText, with: replaceText)
        findAnchor = 0
        selectRange = nil
    }

    private var currentBreakpointLines: Set<Int> {
        guard let rel = eng.openRel else { return [] }
        var out = Set<Int>()
        for bp in eng.breakpoints {   // "rel:line"
            if let c = bp.lastIndex(of: ":"), let n = Int(bp[bp.index(after: c)...]),
                String(bp[..<c]) == rel
            {
                out.insert(n)
            }
        }
        return out
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
                CodeEditorView(
                    text: $eng.source,
                    breakpointLines: currentBreakpointLines,
                    stopLine: eng.stopInOpenFile() ? eng.stopLine : nil,
                    selectRange: selectRange,
                    onToggleBreakpoint: { line in
                        if let rel = eng.openRel { eng.toggleBreakpoint(rel: rel, line: line) }
                    }
                )
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
                    Button {
                    showVars.toggle()
                    eng.autoVars = showVars
                    if showVars { eng.requestVars() }
                } label: { Image(systemName: "list.bullet.rectangle") }
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

            // Breakpoints are set by clicking the editor gutter; the
            // chips below mirror them and remove on click.
            if eng.breakpoints.isEmpty {
                Text("click the gutter to set a breakpoint")
                    .font(.caption2).foregroundStyle(.tertiary)
            }
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
                Button("RAK11161 FreeRTOS Project…") { newProject { eng.scaffoldRTOS(into: $0) } }
                Button("RAK11161 Zephyr Project…") { newProject { eng.scaffoldZephyr(into: $0) } }
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

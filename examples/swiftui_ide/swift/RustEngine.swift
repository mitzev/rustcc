// Bridge from Swift to the fork-Rust IDE engine.
//
// The Rust crate defines these as `extern "Swift"` functions (Swift's
// `swiftcc` calling convention) with stable `#[export_name]`s. We bind
// to those symbols with `@_silgen_name` — a Swift global func is
// `swiftcc` by default, so the ABI matches. Same mechanism the
// `swift_extern_call` probe validates, but exercised here with real
// data: C-strings in, Rust-owned C-strings out (freed via
// `rc_string_free`), and a polled console drain.

import Foundation
import Combine

// MARK: - Raw swiftcc symbols (resolved from libswiftui_ide.a)

@_silgen_name("rc_target_count") func rc_target_count() -> Int64
@_silgen_name("rc_target_name") func rc_target_name(_ i: Int64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("rc_scaffold") func rc_scaffold(_ kind: Int64, _ dir: UnsafePointer<CChar>?) -> Int64
@_silgen_name("rc_open") func rc_open(_ dir: UnsafePointer<CChar>?) -> Int64
@_silgen_name("rc_list_files") func rc_list_files() -> UnsafeMutablePointer<CChar>?
@_silgen_name("rc_read_file") func rc_read_file(_ rel: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("rc_save_file") func rc_save_file(_ rel: UnsafePointer<CChar>?, _ body: UnsafePointer<CChar>?) -> Int64
@_silgen_name("rc_build") func rc_build(_ target: Int64, _ run: Int64) -> Int64
@_silgen_name("rc_is_running") func rc_is_running() -> Int64
@_silgen_name("rc_console_drain") func rc_console_drain() -> UnsafeMutablePointer<CChar>?
@_silgen_name("rc_string_free") func rc_string_free(_ p: UnsafeMutablePointer<CChar>?)

@_silgen_name("rc_dbg_start") func rc_dbg_start(_ target: Int64) -> Int64
@_silgen_name("rc_dbg_send") func rc_dbg_send(_ cmd: UnsafePointer<CChar>?)
@_silgen_name("rc_dbg_stop") func rc_dbg_stop()
@_silgen_name("rc_dbg_toggle_breakpoint") func rc_dbg_toggle_breakpoint(_ rel: UnsafePointer<CChar>?, _ line: Int64) -> Int64
@_silgen_name("rc_dbg_active") func rc_dbg_active() -> Int64
@_silgen_name("rc_dbg_curline") func rc_dbg_curline() -> UnsafeMutablePointer<CChar>?
@_silgen_name("rc_dbg_breakpoints") func rc_dbg_breakpoints() -> UnsafeMutablePointer<CChar>?
@_silgen_name("rc_upload") func rc_upload(_ target: Int64) -> Int64
@_silgen_name("rc_dbg_request_vars") func rc_dbg_request_vars()
@_silgen_name("rc_dbg_vars") func rc_dbg_vars() -> UnsafeMutablePointer<CChar>?

/// Consume a Rust-owned C-string into a Swift `String`, freeing it the
/// way the engine's `rc_string_free` contract requires.
private func takeRustString(_ p: UnsafeMutablePointer<CChar>?) -> String {
    guard let p = p else { return "" }
    let s = String(cString: p)
    rc_string_free(p)
    return s
}

// MARK: - Idiomatic SwiftUI model over the engine

/// The IDE's observable state. SwiftUI owns the UI; every operation —
/// scaffold, open, read/save, build/run, console — is performed by the
/// Rust engine across the `swiftcc` boundary. This is the ONLY place
/// the FFI is touched; the views stay pure SwiftUI.
@MainActor
final class IDEEngine: ObservableObject {
    @Published var targets: [String] = []
    @Published var target: Int = 0
    @Published var projectDir: String? = nil
    @Published var files: [String] = []
    @Published var openRel: String? = nil       // active tab
    @Published var openFiles: [String] = []      // tab order
    @Published var source: String = ""
    /// Unsaved edits per open file, so switching tabs preserves them.
    private var buffers: [String: String] = [:]
    @Published var console: String = ""
    @Published var running: Bool = false

    // Debugger
    @Published var debugActive: Bool = false
    @Published var stopFile: String? = nil   // basename lldb reported
    @Published var stopLine: Int? = nil
    @Published var breakpoints: [String] = [] // "rel:line"
    @Published var variables: String = ""     // latest `frame variable`
    var autoVars = false                      // recapture vars on each stop
    private var lastStopSig = ""

    private var pollTimer: Timer?

    init() {
        let n = rc_target_count()
        targets = (0..<n).map { takeRustString(rc_target_name($0)) }
        // Poll the engine's console drain on the main thread (there is
        // no FLTK event loop; the build runs on a Rust background
        // thread and we pull its output).
        let t = Timer(timeInterval: 0.12, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.pump() }
        }
        RunLoop.main.add(t, forMode: .common)
        pollTimer = t
    }

    private func pump() {
        let chunk = takeRustString(rc_console_drain())
        if !chunk.isEmpty { console += chunk }
        running = rc_is_running() != 0
        debugActive = rc_dbg_active() != 0
        let loc = takeRustString(rc_dbg_curline())
        if let colon = loc.lastIndex(of: ":"), let n = Int(loc[loc.index(after: colon)...]) {
            stopFile = String(loc[..<colon]); stopLine = n
        } else {
            stopFile = nil; stopLine = nil
        }
        // On a NEW stop, recapture variables if the pane wants them.
        let sig = "\(stopFile ?? ""):\(stopLine ?? 0)"
        if sig != lastStopSig {
            lastStopSig = sig
            if autoVars && debugActive && stopLine != nil { rc_dbg_request_vars() }
        }
        let v = takeRustString(rc_dbg_vars())
        if v != variables { variables = v }
    }

    /// Ask the live session for a fresh `frame variable` capture.
    func requestVars() { rc_dbg_request_vars() }

    // MARK: - Debugger (host target only)

    func dbgStart() {
        if openRel != nil { save() }
        _ = rc_dbg_start(Int64(target))
    }
    func dbgStop() { rc_dbg_stop() }
    func dbgSend(_ cmd: String) { cmd.withCString { rc_dbg_send($0) } }
    func stepOver() { dbgSend("thread step-over") }
    func stepInto() { dbgSend("thread step-in") }
    func stepOut()  { dbgSend("thread step-out") }
    func continueRun() { dbgSend("continue") }

    /// Toggle a breakpoint at `rel:line` (replayed into a live session
    /// by the engine).
    func toggleBreakpoint(rel: String, line: Int) {
        _ = rel.withCString { rc_dbg_toggle_breakpoint($0, Int64(line)) }
        refreshBreakpoints()
    }
    func refreshBreakpoints() {
        let s = takeRustString(rc_dbg_breakpoints())
        breakpoints = s.isEmpty ? [] : s.split(separator: "\n").map(String.init)
    }

    /// Is the stopped line in the file currently open in the editor?
    func stopInOpenFile() -> Bool {
        guard let f = stopFile, let rel = openRel else { return false }
        return rel.hasSuffix(f)
    }

    func scaffoldHost(into dir: String) { scaffold(kind: 0, into: dir, defaultTarget: 0) }

    /// RAK11161 dual-core FreeRTOS firmware; default to the STM32WLE5
    /// (CM4) core so Build/Run picks `run_arm.sh`.
    func scaffoldRTOS(into dir: String) { scaffold(kind: 1, into: dir, defaultTarget: 1) }

    private func scaffold(kind: Int64, into dir: String, defaultTarget: Int) {
        let rc = dir.withCString { rc_scaffold(kind, $0) }
        if rc == 0 {
            projectDir = dir
            target = defaultTarget
            refreshFiles()
            openFirstSource()
        }
        pump()
    }

    func open(_ dir: String) {
        if dir.withCString({ rc_open($0) }) >= 0 {
            projectDir = dir
            refreshFiles()
            openFirstSource()
        }
        pump()
    }

    func refreshFiles() {
        let joined = takeRustString(rc_list_files())
        files = joined.isEmpty ? [] : joined.split(separator: "\n").map(String.init)
    }

    private func openFirstSource() {
        openFiles = []; buffers = [:]; openRel = nil; source = ""
        if let first = files.first(where: { $0.hasSuffix("main.rs") || $0.hasSuffix("lib.rs") })
            ?? files.first(where: { $0.hasSuffix(".rs") })
        {
            openFile(first)
        }
    }

    /// Open `rel` in a tab (or switch to it), preserving the current
    /// tab's unsaved edits. Reads from disk only on first open.
    func openFile(_ rel: String) {
        if let cur = openRel { buffers[cur] = source }
        if !openFiles.contains(rel) { openFiles.append(rel) }
        openRel = rel
        if let cached = buffers[rel] {
            source = cached
        } else {
            source = rel.withCString { takeRustString(rc_read_file($0)) }
            buffers[rel] = source
        }
    }

    /// Close a tab; switch to a neighbor (or empty if it was the last).
    func closeFile(_ rel: String) {
        buffers[rel] = nil
        if let i = openFiles.firstIndex(of: rel) { openFiles.remove(at: i) }
        if openRel == rel {
            if let next = openFiles.last {
                openRel = next
                source = buffers[next] ?? (next.withCString { takeRustString(rc_read_file($0)) })
                buffers[next] = source
            } else {
                openRel = nil; source = ""
            }
        }
    }

    func save() {
        guard let rel = openRel else { return }
        buffers[rel] = source
        _ = rel.withCString { relC in
            source.withCString { bodyC in rc_save_file(relC, bodyC) }
        }
        pump()
    }

    func build(run: Bool) {
        guard projectDir != nil else {
            console += "no project open — New or Open a project first\n"
            return
        }
        if openRel != nil { save() }
        _ = rc_build(Int64(target), run ? 1 : 0)
        running = true
    }

    /// Flash the built firmware for the current target (RTOS only;
    /// host has nothing to flash). Uses the project's upload.toml.
    func upload() {
        _ = rc_upload(Int64(target))
        running = rc_is_running() != 0
    }

    /// Whether the current target can be flashed (RTOS, not host).
    var canUpload: Bool { target >= 1 && target <= 5 }

    func clearConsole() { console = "" }
}

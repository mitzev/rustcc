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
    @Published var openRel: String? = nil
    @Published var source: String = ""
    @Published var console: String = ""
    @Published var running: Bool = false

    // Debugger
    @Published var debugActive: Bool = false
    @Published var stopFile: String? = nil   // basename lldb reported
    @Published var stopLine: Int? = nil
    @Published var breakpoints: [String] = [] // "rel:line"

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
    }

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
    func variables() { dbgSend("frame variable") }

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

    func scaffoldHost(into dir: String) {
        let rc = dir.withCString { rc_scaffold(0, $0) }
        if rc == 0 {
            projectDir = dir
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
        if let first = files.first(where: { $0.hasSuffix("main.rs") || $0.hasSuffix("lib.rs") })
            ?? files.first(where: { $0.hasSuffix(".rs") })
        {
            openFile(first)
        }
    }

    func openFile(_ rel: String) {
        openRel = rel
        source = rel.withCString { takeRustString(rc_read_file($0)) }
    }

    func save() {
        guard let rel = openRel else { return }
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

    func clearConsole() { console = "" }
}

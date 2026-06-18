// A real code editor for the IDE: SwiftUI's `TextEditor` has no
// gutter, no per-line decoration, and no programmatic scroll, so the
// editor + debugger want more. This wraps an `NSTextView` (AppKit)
// with a line-number ruler that also draws breakpoint dots (click the
// gutter to toggle) and highlights the debugger's current stop line.
//
// It's the one place the IDE reaches past SwiftUI into AppKit; the
// breakpoint + stop-line state still flows from the fork-Rust engine
// (via IDEEngine), so the gutter is a view onto engine state.

import SwiftUI
import AppKit

struct CodeEditorView: NSViewRepresentable {
    @Binding var text: String
    var breakpointLines: Set<Int>      // 1-based lines with a breakpoint
    var stopLine: Int?                 // 1-based debugger stop line, or nil
    var selectRange: NSRange?          // find: select + scroll to this range
    var onToggleBreakpoint: (Int) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.borderType = .noBorder
        scroll.autohidesScrollers = true

        let tv = NSTextView()
        tv.isRichText = false
        tv.isAutomaticQuoteSubstitutionEnabled = false
        tv.isAutomaticDashSubstitutionEnabled = false
        tv.isAutomaticSpellingCorrectionEnabled = false
        tv.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        tv.allowsUndo = true
        tv.delegate = context.coordinator
        tv.textContainerInset = NSSize(width: 2, height: 4)
        tv.isVerticallyResizable = true
        tv.isHorizontallyResizable = false
        tv.autoresizingMask = [.width]
        tv.textContainer?.widthTracksTextView = true
        tv.string = text

        scroll.documentView = tv

        let ruler = LineNumberRuler(textView: tv)
        ruler.onToggle = onToggleBreakpoint
        scroll.verticalRulerView = ruler
        scroll.hasVerticalRuler = true
        scroll.rulersVisible = true

        context.coordinator.textView = tv
        context.coordinator.ruler = ruler
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let tv = context.coordinator.textView else { return }
        let ns = tv.string as NSString

        // Sync text without stomping the cursor (avoid a feedback loop:
        // only reset when the model genuinely differs).
        if tv.string != text {
            let sel = tv.selectedRange()
            tv.string = text
            let len = (text as NSString).length
            tv.setSelectedRange(NSRange(location: min(sel.location, len), length: 0))
        }

        // Gutter state.
        context.coordinator.ruler?.breakpointLines = breakpointLines
        context.coordinator.ruler?.stopLine = stopLine
        context.coordinator.ruler?.needsDisplay = true

        // Current-line highlight (temporary attribute = doesn't touch
        // the document text or undo stack).
        if let lm = tv.layoutManager {
            let full = NSRange(location: 0, length: ns.length)
            lm.removeTemporaryAttribute(.backgroundColor, forCharacterRange: full)
            if let line = stopLine, let r = Self.lineRange(ns, line) {
                lm.addTemporaryAttributes(
                    [.backgroundColor: NSColor.systemOrange.withAlphaComponent(0.28)],
                    forCharacterRange: r
                )
                tv.scrollRangeToVisible(r)
            }
        }

        // Find: select + scroll to a requested range (once per change).
        if let r = selectRange, r.location != NSNotFound, NSMaxRange(r) <= ns.length,
            !NSEqualRanges(r, context.coordinator.lastSelect)
        {
            tv.setSelectedRange(r)
            tv.scrollRangeToVisible(r)
            context.coordinator.lastSelect = r
        }
    }

    /// Character range of 1-based `line` (including its trailing
    /// newline), or nil if out of range.
    static func lineRange(_ s: NSString, _ line: Int) -> NSRange? {
        guard line >= 1 else { return nil }
        var idx = 0, cur = 1
        while cur < line {
            let r = s.range(of: "\n", range: NSRange(location: idx, length: s.length - idx))
            if r.location == NSNotFound { return nil }
            idx = r.location + 1
            cur += 1
        }
        let lineRange = s.lineRange(for: NSRange(location: idx, length: 0))
        return lineRange
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        let parent: CodeEditorView
        weak var textView: NSTextView?
        weak var ruler: LineNumberRuler?
        var lastSelect = NSRange(location: NSNotFound, length: 0)

        init(_ parent: CodeEditorView) { self.parent = parent }

        func textDidChange(_ notification: Notification) {
            guard let tv = notification.object as? NSTextView else { return }
            parent.text = tv.string
            ruler?.needsDisplay = true
        }
    }
}

/// Left-margin ruler: line numbers + red breakpoint dots, with a
/// click target that toggles a breakpoint on the clicked line.
final class LineNumberRuler: NSRulerView {
    var breakpointLines: Set<Int> = []
    var stopLine: Int?
    var onToggle: ((Int) -> Void)?

    init(textView: NSTextView) {
        super.init(scrollView: textView.enclosingScrollView, orientation: .verticalRuler)
        clientView = textView
        ruleThickness = 44
    }
    required init(coder: NSCoder) { fatalError() }

    private var textView: NSTextView? { clientView as? NSTextView }

    override func drawHashMarksAndLabels(in rect: NSRect) {
        guard let tv = textView, let lm = tv.layoutManager, let tc = tv.textContainer else {
            return
        }
        let ns = tv.string as NSString
        let inset = tv.textContainerInset.height
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedSystemFont(ofSize: 10, weight: .regular),
            .foregroundColor: NSColor.secondaryLabelColor,
        ]
        // Walk line fragments; the line number increments on each
        // paragraph (text that began at a line start).
        let visible = tv.visibleRect
        var lineNo = 1
        var charIdx = 0
        while charIdx < ns.length || charIdx == 0 {
            let glyphIdx = lm.glyphIndexForCharacter(at: charIdx)
            var effective = NSRange()
            let fragRect = lm.lineFragmentRect(forGlyphAt: glyphIdx, effectiveRange: &effective)
            let y = fragRect.minY + inset - visible.minY
            if y + fragRect.height >= 0 && y <= bounds.height {
                if breakpointLines.contains(lineNo) {
                    NSColor.systemRed.setFill()
                    NSBezierPath(ovalIn: NSRect(x: 4, y: y + 3, width: 9, height: 9)).fill()
                }
                if stopLine == lineNo {
                    NSColor.systemOrange.withAlphaComponent(0.5).setFill()
                    NSRect(x: 0, y: y, width: ruleThickness, height: fragRect.height).fill()
                }
                let label = "\(lineNo)" as NSString
                let size = label.size(withAttributes: attrs)
                label.draw(at: NSPoint(x: ruleThickness - size.width - 5, y: y + 1), withAttributes: attrs)
            }
            // Advance to the next line (next char after this line's newline).
            let lineCharRange = ns.lineRange(for: NSRange(location: charIdx, length: 0))
            let next = NSMaxRange(lineCharRange)
            if next <= charIdx { break }
            charIdx = next
            lineNo += 1
            if y > bounds.height + fragRect.height { break }
            _ = tc
        }
    }

    override func mouseDown(with event: NSEvent) {
        guard let tv = textView, let lm = tv.layoutManager else { return }
        let p = convert(event.locationInWindow, from: nil)
        let yInText = p.y + tv.visibleRect.minY - tv.textContainerInset.height
        let glyph = lm.glyphIndex(for: NSPoint(x: 0, y: yInText), in: tv.textContainer!)
        let charIdx = lm.characterIndexForGlyph(at: glyph)
        let ns = tv.string as NSString
        // count newlines up to charIdx → 1-based line
        var line = 1, i = 0
        while i < charIdx {
            let r = ns.range(of: "\n", range: NSRange(location: i, length: charIdx - i))
            if r.location == NSNotFound { break }
            line += 1
            i = r.location + 1
        }
        onToggle?(line)
    }
}

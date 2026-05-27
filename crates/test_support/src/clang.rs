//! Drive `clang++ -Xclang -fdump-record-layouts` and parse its output
//! into the canonical [`golden::Dump`] form.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::golden::{BaseDump, Dump, FieldDump};

pub struct ClangInvocation {
    pub clang_bin: String,
    pub extra_flags: Vec<String>,
}

impl Default for ClangInvocation {
    fn default() -> Self {
        Self {
            clang_bin: String::from("clang++"),
            extra_flags: vec![String::from("-std=c++17")],
        }
    }
}

pub fn dump_record_layouts(
    inv: &ClangInvocation,
    source: &Path,
) -> Result<String, String> {
    let out_path: PathBuf = std::env::temp_dir().join("rustcc-clang-dump.o");
    let output = Command::new(&inv.clang_bin)
        .arg("-Xclang")
        .arg("-fdump-record-layouts")
        .arg("-c")
        .arg("-o")
        .arg(&out_path)
        .args(&inv.extra_flags)
        .arg(source)
        .output()
        .map_err(|e| format!("failed to spawn {}: {e}", inv.clang_bin))?;
    if !output.status.success() {
        return Err(format!(
            "{} failed (exit {}):\nstdout:\n{}\nstderr:\n{}",
            inv.clang_bin,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    // `-fdump-record-layouts` writes to stdout; diagnostics go to stderr.
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn host_target_triple(inv: &ClangInvocation) -> Result<String, String> {
    let output = Command::new(&inv.clang_bin)
        .arg("-dumpmachine")
        .output()
        .map_err(|e| format!("failed to spawn {}: {e}", inv.clang_bin))?;
    if !output.status.success() {
        return Err(format!("{} -dumpmachine failed", inv.clang_bin));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Parse Clang's `-fdump-record-layouts` output and extract the layout
/// of `target_class`.
pub fn parse_dump(text: &str, target_class: &str) -> Result<Dump, String> {
    for section in text.split("*** Dumping AST Record Layout").skip(1) {
        if let Some(dump) = try_parse_section(section, target_class)? {
            return Ok(dump);
        }
    }
    Err(format!(
        "class {target_class:?} not found in clang dump output"
    ))
}

fn try_parse_section(
    section: &str,
    target: &str,
) -> Result<Option<Dump>, String> {
    // Limit to the AST layout portion before any IRgen dump or AST
    // record diagnostic.
    let ast_only = section
        .split("*** Dumping IRgen")
        .next()
        .unwrap_or(section);

    let mut lines = ast_only.lines().filter(|l| !l.trim().is_empty());
    let header = match lines.next() {
        Some(l) => l,
        None => return Ok(None),
    };
    let class_name = match parse_header_class(header) {
        Some(n) => n,
        None => return Ok(None),
    };
    if class_name != target {
        return Ok(None);
    }

    let mut dump = Dump {
        target: String::new(),
        class: class_name,
        sizeof: 0,
        dsize: 0,
        align: 0,
        nvsize: 0,
        nvalign: 0,
        has_vptr: false,
        bases: Vec::new(),
        fields: Vec::new(),
    };

    // Derived classes inherit their primary base's vptr, and Clang doesn't
    // re-emit a vptr line at the derived record's own indent level — it
    // appears only inside the base subobject. Pre-scan the whole section
    // for any vtable-pointer marker so we catch that case.
    if ast_only.contains("vtable pointer") {
        dump.has_vptr = true;
    }

    let mut footer_buf = String::new();
    for line in lines {
        let (lhs, rhs) = match line.split_once('|') {
            Some(p) => p,
            None => continue,
        };
        let lhs = lhs.trim();
        if lhs.is_empty() {
            footer_buf.push_str(rhs);
            footer_buf.push(' ');
            continue;
        }
        // Clang prints a plain byte offset (`4`) for ordinary fields
        // and a `byte:startbit-endbit` form for bit-fields
        // (`0:4-23` = byte 0, bits 4..=23). Parse both; the bit form
        // yields a (byte, bit_offset, bit_width) triple.
        let (offset, bits) = match parse_offset_lhs(lhs) {
            Some(v) => v,
            None => continue,
        };
        let indent = rhs.chars().take_while(|c| *c == ' ').count();
        // Clang uses 1-space indent for the record header and 3-space indent
        // for each direct child; nested entries (a base's fields) indent
        // further and are skipped here — we track the base at its top-level
        // position instead.
        if indent != 3 {
            continue;
        }
        parse_entry(&mut dump, offset, bits, rhs.trim());
    }

    parse_footer(&footer_buf, &mut dump)?;

    Ok(Some(dump))
}

fn parse_header_class(header: &str) -> Option<String> {
    let rhs = header.split('|').nth(1)?.trim();
    let no_kind = strip_kind_prefix(rhs);
    let no_parens = strip_trailing_parens(no_kind);
    if no_parens.is_empty() {
        None
    } else {
        Some(no_parens.to_string())
    }
}

fn strip_kind_prefix(s: &str) -> &str {
    for prefix in ["struct ", "class ", "union "] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return rest.trim();
        }
    }
    s.trim()
}

fn strip_trailing_parens(s: &str) -> &str {
    let mut s = s.trim();
    loop {
        if !s.ends_with(')') {
            return s;
        }
        match s.rfind('(') {
            Some(idx) => s = s[..idx].trim(),
            None => return s,
        }
    }
}

/// Parse the left-hand offset column of a clang record-layout line.
/// Returns `(byte_offset, Option<(bit_offset, bit_width)>)`:
/// - `"4"`        → `(4, None)`           — ordinary field
/// - `"0:4-23"`   → `(0, Some((4, 20)))`  — bit-field at byte 0,
///   bits 4..=23 inclusive (width = 23 - 4 + 1 = 20).
fn parse_offset_lhs(lhs: &str) -> Option<(u64, Option<(u8, u64)>)> {
    match lhs.split_once(':') {
        None => lhs.parse::<u64>().ok().map(|b| (b, None)),
        Some((byte, bitrange)) => {
            let byte: u64 = byte.trim().parse().ok()?;
            let (start, end) = bitrange.split_once('-')?;
            let start: u64 = start.trim().parse().ok()?;
            let end: u64 = end.trim().parse().ok()?;
            let width = end.checked_sub(start)? + 1;
            Some((byte, Some((start as u8, width))))
        }
    }
}

fn parse_entry(dump: &mut Dump, offset: u64, bits: Option<(u8, u64)>, body: &str) {
    if body.contains("vtable pointer") {
        dump.has_vptr = true;
        return;
    }
    // Base-subobject suffixes: `(base)`, `(primary base)`, `(virtual base)`.
    // All end in `base)`, which fields never do.
    if body.contains("base)") {
        let stripped = strip_trailing_parens(body);
        let name = strip_kind_prefix(stripped).to_string();
        dump.bases.push(BaseDump {
            class: name,
            offset,
        });
        return;
    }
    // Field: last whitespace-separated token is the field name, with
    // any array brackets trimmed.
    let name = body
        .split_whitespace()
        .last()
        .unwrap_or("")
        .split('[')
        .next()
        .unwrap_or("")
        .to_string();
    if !name.is_empty() {
        dump.fields.push(FieldDump { name, offset, bits });
    }
}

fn parse_footer(buf: &str, dump: &mut Dump) -> Result<(), String> {
    let body = buf.trim().trim_start_matches('[').trim_end_matches(']');
    for pair in body.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some(p) => p,
            None => continue,
        };
        let v: u64 = v
            .trim()
            .parse()
            .map_err(|e| format!("bad {k}={v:?}: {e}"))?;
        match k.trim() {
            "sizeof" => dump.sizeof = v,
            "dsize" => dump.dsize = v,
            "align" => dump.align = v,
            "nvsize" => dump.nvsize = v,
            "nvalign" => dump.nvalign = v,
            _ => {}
        }
    }
    Ok(())
}

/// Read the `// @target ClassName` directive from the first few lines
/// of a corpus `.cpp` file. This tells `refresh-goldens` which class in
/// the Clang dump to extract.
pub fn read_target_directive(source: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(source)
        .map_err(|e| format!("reading {}: {e}", source.display()))?;
    for line in text.lines().take(10) {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("// @target ") {
            return Ok(rest.trim().to_string());
        }
    }
    Err(format!(
        "no `// @target ClassName` directive in first 10 lines of {}",
        source.display()
    ))
}

// -------- Full directive set -------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorpusKind {
    Layout,
    Mangle,
    Vtable,
}

#[derive(Debug, Clone)]
pub struct Directives {
    pub kind: CorpusKind,
    pub target: Option<String>,
}

pub fn read_directives(source: &Path) -> Result<Directives, String> {
    let text = std::fs::read_to_string(source)
        .map_err(|e| format!("reading {}: {e}", source.display()))?;
    let mut kind: Option<CorpusKind> = None;
    let mut target: Option<String> = None;
    for line in text.lines().take(15) {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("// @kind ") {
            kind = Some(match rest.trim() {
                "layout" => CorpusKind::Layout,
                "mangle" => CorpusKind::Mangle,
                "vtable" => CorpusKind::Vtable,
                other => {
                    return Err(format!(
                        "unknown @kind {other:?} in {}",
                        source.display()
                    ))
                }
            });
        } else if let Some(rest) = trimmed.strip_prefix("// @target ") {
            target = Some(rest.trim().to_string());
        }
    }
    // Backward compatibility: a file with only `// @target` and no `// @kind`
    // is a layout corpus entry (this is how the first batch of goldens was
    // authored; keep the default to avoid churning those files).
    let kind = kind.unwrap_or(CorpusKind::Layout);
    Ok(Directives { kind, target })
}

// -------- LLVM IR driver & extraction ---------------------------------

pub fn dump_llvm_ir(
    inv: &ClangInvocation,
    source: &Path,
) -> Result<String, String> {
    let output = Command::new(&inv.clang_bin)
        .arg("-S")
        .arg("-emit-llvm")
        .args(&inv.extra_flags)
        .arg(source)
        .arg("-o")
        .arg("-")
        .output()
        .map_err(|e| format!("failed to spawn {}: {e}", inv.clang_bin))?;
    if !output.status.success() {
        return Err(format!(
            "{} -S -emit-llvm failed (exit {}):\n{}",
            inv.clang_bin,
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Extract all C++ mangled symbol names (those starting with `_Z`) that
/// appear in `define` or `declare` positions in the given LLVM IR text.
/// Returns the sorted, deduplicated list.
pub fn extract_cxx_symbols(ir: &str) -> Vec<String> {
    let mut syms = Vec::new();
    for line in ir.lines() {
        let line = line.trim_start();
        if !(line.starts_with("define ") || line.starts_with("declare ")) {
            continue;
        }
        if let Some(at) = line.find("@_Z") {
            let rest = &line[at + 1..];
            let end = rest.find('(').unwrap_or(rest.len());
            let name = rest[..end].trim().to_string();
            if !name.is_empty() {
                syms.push(name);
            }
        }
    }
    syms.sort();
    syms.dedup();
    syms
}

/// Parse the `@_ZTV<mangled_class>` vtable constant from the given LLVM IR
/// and return its slot contents in order.
pub fn extract_vtable(
    ir: &str,
    mangled_class: &str,
) -> Result<Vec<crate::golden::VtableEntryDump>, String> {
    let symbol = format!("@_ZTV{mangled_class}");
    let line = ir
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("{symbol} = ")))
        .ok_or_else(|| {
            format!("vtable symbol {symbol:?} not found in LLVM IR")
        })?;

    // Locate the slot-array initializer. The line contains array-type
    // decls like `[6 x ptr]` and the slot-array `[ptr null, ptr @..., ...]`.
    // The latter always opens with `[ptr ` (trailing space) — a pattern that
    // can't occur inside the type decl, which is `[N x ptr]` with no space
    // between `ptr` and `]`.
    let slot_open = line.find("[ptr ").ok_or_else(|| {
        format!("could not locate slot array in: {line}")
    })?;
    let rest = &line[slot_open + 1..];
    let mut depth = 1i32;
    let mut end = 0usize;
    for (i, c) in rest.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(format!("unterminated slot array in: {line}"));
    }
    let slots_text = &rest[..end];

    let mut entries = Vec::new();
    for raw in split_top_level(slots_text, ',') {
        let slot = raw.trim();
        if slot.is_empty() {
            continue;
        }
        entries.push(parse_vtable_slot(slot)?);
    }
    Ok(entries)
}

fn parse_vtable_slot(
    s: &str,
) -> Result<crate::golden::VtableEntryDump, String> {
    let body = s.strip_prefix("ptr ").unwrap_or(s).trim();
    if body == "null" {
        return Ok(crate::golden::VtableEntryDump::OffsetToTop(0));
    }
    if let Some(rest) = body.strip_prefix("inttoptr (") {
        // `inttoptr (i64 N to ptr)` — extract the integer.
        let rest = rest.trim_start_matches("i64 ").trim_start();
        let end = rest
            .find(' ')
            .ok_or_else(|| format!("malformed inttoptr: {s:?}"))?;
        let num = rest[..end].trim();
        let n: i64 = num
            .parse()
            .map_err(|e| format!("bad inttoptr i64 {num:?}: {e}"))?;
        return Ok(crate::golden::VtableEntryDump::OffsetToTop(n));
    }
    if let Some(rest) = body.strip_prefix('@') {
        let sym = rest.trim();
        if sym.starts_with("_ZTI") {
            return Ok(crate::golden::VtableEntryDump::Rtti(sym.to_string()));
        }
        return Ok(crate::golden::VtableEntryDump::FunctionPointer(
            sym.to_string(),
        ));
    }
    Err(format!("unrecognized vtable slot: {s:?}"))
}

fn split_top_level(s: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut current = String::new();
    for c in s.chars() {
        match c {
            '[' | '(' | '{' => {
                depth += 1;
                current.push(c);
            }
            ']' | ')' | '}' => {
                depth -= 1;
                current.push(c);
            }
            c if c == delim && depth == 0 => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// Itanium-mangle a simple unqualified class name: `Base` → `4Base`.
/// Only valid for non-nested names; nested names (`ns::Bar`) require the
/// full nested-name encoding and aren't produced here.
pub fn mangle_unqualified_class_name(name: &str) -> String {
    format!("{}{}", name.len(), name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pod_scalar() {
        let text = "\
*** Dumping AST Record Layout
         0 | struct Foo
         0 |   int x
         4 |   char y
           | [sizeof=8, dsize=8, align=4,
           |  nvsize=8, nvalign=4]
";
        let d = parse_dump(text, "Foo").expect("parse");
        assert_eq!(d.class, "Foo");
        assert_eq!(d.sizeof, 8);
        assert_eq!(d.dsize, 8);
        assert_eq!(d.align, 4);
        assert_eq!(d.nvsize, 8);
        assert_eq!(d.nvalign, 4);
        assert!(!d.has_vptr);
        assert_eq!(d.fields.len(), 2);
        assert_eq!(d.fields[0].name, "x");
        assert_eq!(d.fields[0].offset, 0);
        assert_eq!(d.fields[1].name, "y");
        assert_eq!(d.fields[1].offset, 4);
        assert!(d.bases.is_empty());
    }

    #[test]
    fn parses_single_inheritance_with_tail_reuse() {
        let text = "\
*** Dumping AST Record Layout
         0 | struct Base
         0 |   int x
         4 |   char y
           | [sizeof=8, dsize=5, align=4,
           |  nvsize=5, nvalign=4]

*** Dumping AST Record Layout
         0 | struct Derived
         0 |   struct Base (base)
         0 |     int x
         4 |     char y
         5 |   char z
         8 |   int w
           | [sizeof=12, dsize=12, align=4,
           |  nvsize=12, nvalign=4]
";
        let d = parse_dump(text, "Derived").expect("parse");
        assert_eq!(d.class, "Derived");
        assert_eq!(d.sizeof, 12);
        assert_eq!(d.dsize, 12);
        assert_eq!(d.align, 4);
        assert_eq!(d.bases.len(), 1);
        assert_eq!(d.bases[0].class, "Base");
        assert_eq!(d.bases[0].offset, 0);
        assert_eq!(d.fields.len(), 2);
        assert_eq!(d.fields[0].name, "z");
        assert_eq!(d.fields[0].offset, 5);
        assert_eq!(d.fields[1].name, "w");
        assert_eq!(d.fields[1].offset, 8);
    }

    #[test]
    fn parses_empty_base_optimization() {
        let text = "\
*** Dumping AST Record Layout
         0 | struct EmptyDerived
         0 |   struct Empty (base) (empty)
         0 |   int x
           | [sizeof=4, dsize=4, align=4,
           |  nvsize=4, nvalign=4]
";
        let d = parse_dump(text, "EmptyDerived").expect("parse");
        assert_eq!(d.sizeof, 4);
        assert_eq!(d.bases.len(), 1);
        assert_eq!(d.bases[0].class, "Empty");
        assert_eq!(d.bases[0].offset, 0);
        assert_eq!(d.fields.len(), 1);
        assert_eq!(d.fields[0].name, "x");
        assert_eq!(d.fields[0].offset, 0);
    }

    #[test]
    fn extracts_cxx_symbols_from_ir() {
        let ir = "\
define void @_ZN3Foo3barEi(ptr %this, i32 %x) {
declare void @_Z7free_fni(i32)
define void @_ZN3Foo3barEi(ptr %this, i32 %x) {
; dupe above — dedup should collapse
@some_global = constant i32 0
";
        let syms = extract_cxx_symbols(ir);
        assert_eq!(syms, vec!["_Z7free_fni", "_ZN3Foo3barEi"]);
    }

    #[test]
    fn extracts_vtable_globals() {
        let ir = "\
@_ZTV4Base = unnamed_addr constant { [6 x ptr] } { [6 x ptr] [ptr null, ptr @_ZTI4Base, ptr @_ZN4BaseD1Ev, ptr @_ZN4BaseD0Ev, ptr @_ZN4Base6renderEv, ptr @_ZNK4Base4areaEv] }, align 8
";
        use crate::golden::VtableEntryDump as E;
        let entries = extract_vtable(ir, "4Base").expect("extract");
        assert_eq!(
            entries,
            vec![
                E::OffsetToTop(0),
                E::Rtti("_ZTI4Base".into()),
                E::FunctionPointer("_ZN4BaseD1Ev".into()),
                E::FunctionPointer("_ZN4BaseD0Ev".into()),
                E::FunctionPointer("_ZN4Base6renderEv".into()),
                E::FunctionPointer("_ZNK4Base4areaEv".into()),
            ]
        );
    }

    #[test]
    fn extracts_vtable_with_nonzero_offset_to_top() {
        // Contrived: verify the `inttoptr` parser handles negative offsets.
        let ir = "\
@_ZTV4Test = constant { [2 x ptr] } { [2 x ptr] [ptr inttoptr (i64 -16 to ptr), ptr @_ZTI4Test] }, align 8
";
        use crate::golden::VtableEntryDump as E;
        let entries = extract_vtable(ir, "4Test").expect("extract");
        assert_eq!(entries[0], E::OffsetToTop(-16));
        assert_eq!(entries[1], E::Rtti("_ZTI4Test".into()));
    }

    #[test]
    fn reports_missing_vtable_symbol() {
        let ir = "@_ZTV4Base = ...";
        let err = extract_vtable(ir, "7Missing").unwrap_err();
        assert!(err.contains("_ZTV7Missing"));
    }

    #[test]
    fn mangles_unqualified_class_name() {
        assert_eq!(mangle_unqualified_class_name("Base"), "4Base");
        assert_eq!(mangle_unqualified_class_name("Widget"), "6Widget");
    }

    #[test]
    fn parses_polymorphic_with_vptr() {
        let text = "\
*** Dumping AST Record Layout
         0 | struct Widget
         0 |   (Widget vtable pointer)
         8 |   int value
           | [sizeof=16, dsize=12, align=8,
           |  nvsize=12, nvalign=8]
";
        let d = parse_dump(text, "Widget").expect("parse");
        assert!(d.has_vptr);
        assert_eq!(d.sizeof, 16);
        assert_eq!(d.dsize, 12);
        assert_eq!(d.align, 8);
        assert_eq!(d.fields.len(), 1);
        assert_eq!(d.fields[0].name, "value");
        assert_eq!(d.fields[0].offset, 8);
    }
}

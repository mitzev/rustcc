//! `#define` constant capture (M12).
//!
//! libclang surfaces preprocessor macros as `EntityKind::MacroDefinition`
//! cursors when the parser flag `detailed_preprocessing_record` is set.
//! For each object-like macro (function-like macros are rejected —
//! they can't be lowered to a single Rust value), we ask clang to
//! evaluate the expansion and pick out the literal int / float /
//! bool / string variants. Anything more exotic (token sequences,
//! references to other symbols, etc.) is silently skipped — the
//! design doc's "soft warning" path.
//!
//! Output flows through [`MacroSet`], analogous to the
//! [`crate::AnnotationSet`] sidecar for inline `[[clang::annotate]]`
//! attrs. The `rust_bindings` emitter accepts a `MacroSet` and
//! renders each entry as a `pub const` at the top of the
//! generated module.

#[cfg(feature = "libclang")]
use std::path::Path;

#[cfg(feature = "libclang")]
use clang::{Clang, EntityKind, Index};

#[cfg(feature = "libclang")]
use crate::diagnostics::ImportError;

/// One captured `#define` constant.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "cache", derive(serde::Serialize, serde::Deserialize))]
pub struct MacroConst {
    /// The macro name as it appears in the C++ source (`FL_RED`,
    /// `FL_NORMAL_LABEL`, etc.). Macros don't have C++ namespace
    /// scoping, so this is a plain identifier.
    pub name: String,
    /// The evaluated value. Variants other than what
    /// [`MacroValue`] covers are skipped during capture.
    pub value: MacroValue,
}

/// Subset of clang's `EvaluationResult` we lower to Rust types.
/// Strings are kept as owned `String` for portability.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "cache", derive(serde::Serialize, serde::Deserialize))]
pub enum MacroValue {
    SignedInteger(i64),
    UnsignedInteger(u64),
    Float(f64),
    String(String),
    /// `#define X true` / `#define X false`. Captured via int
    /// evaluation that returns 0 or 1, so we synthesize the bool
    /// from there at lowering time.
    Bool(bool),
}

/// Side-table of captured `#define` constants for one or more
/// header roots. Pairs with [`crate::AnnotationSet`] in the same
/// way: the importer populates it; downstream emitters consume it.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "cache", derive(serde::Serialize, serde::Deserialize))]
pub struct MacroSet {
    pub entries: Vec<MacroConst>,
}

impl MacroSet {
    /// Iterate all captured macros in capture order. Stable across
    /// invocations of `cargo build` that see the same headers in
    /// the same order.
    pub fn iter(&self) -> impl Iterator<Item = &MacroConst> + '_ {
        self.entries.iter()
    }

    /// Look up a macro by name. `None` if not captured (either the
    /// header doesn't define it, or the value couldn't be lowered).
    pub fn get(&self, name: &str) -> Option<&MacroConst> {
        self.entries.iter().find(|m| m.name == name)
    }
}

/// Parse `source` with `detailed_preprocessing_record` on, walk
/// every `MacroDefinition` cursor, and lower the object-like ones
/// whose value clang can evaluate. Function-like macros are
/// skipped — they'd require expansion-context tracking that the
/// design doc explicitly defers.
///
/// `args` should be the same flag list the caller would pass to
/// [`crate::import_header`] — same `-x c++`, same `-I` paths,
/// etc. The macro pass needs an identical preprocessor view to
/// match what the regular import sees for class definitions.
#[cfg(feature = "libclang")]
pub fn collect_macros(
    source: &Path,
    args: &[&str],
) -> Result<MacroSet, ImportError> {
    let clang = Clang::new().map_err(|e| ImportError::ClangDiagnostic {
        file: source.display().to_string(),
        line: 0,
        message: format!("failed to initialize libclang: {e}"),
    })?;
    let index = Index::new(&clang, false, false);
    let tu = index
        .parser(source)
        .arguments(args)
        .detailed_preprocessing_record(true)
        .parse()
        .map_err(|e| ImportError::ClangDiagnostic {
            file: source.display().to_string(),
            line: 0,
            message: format!("parse failed (with preprocessing record): {e:?}"),
        })?;

    let mut entries: Vec<MacroConst> = Vec::new();
    for child in tu.get_entity().get_children() {
        if child.get_kind() != EntityKind::MacroDefinition {
            continue;
        }
        if child.is_function_like_macro() {
            // Function-like macros (e.g. `#define MIN(a, b) …`)
            // need expansion-context tracking that v0 doesn't
            // have. Skip silently.
            continue;
        }
        let name = match child.get_name() {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };
        // Skip clang-builtin / system macros — they pollute the
        // capture set and rarely belong in user-facing bindings.
        if is_likely_system_macro(&name) {
            continue;
        }
        // libclang's `Cursor_Evaluate` doesn't fire on macro
        // definitions (only on expression cursors), so we
        // tokenize the macro's source range and parse the body
        // tokens directly. The first token is the macro name; the
        // rest is the expansion. Single-token literal RHS forms
        // are what FLTK / Qt / most real headers use; multi-token
        // expansions (`(FL_A | FL_B)`) are skipped — `clang::annotate`
        // and sidecar overrides are the escape hatch.
        let value = match parse_macro_body_tokens(&child) {
            Some(v) => v,
            None => continue,
        };
        entries.push(MacroConst { name, value });
    }
    Ok(MacroSet { entries })
}

/// Parse the body tokens of an object-like macro. Returns `Some`
/// for single-token literal expansions (decimal int, hex int,
/// float, string, char-as-int, `true` / `false`); `None` for
/// anything more complex (parenthesized expressions, references
/// to other macros, etc.).
///
/// The shape we produce mirrors what clang's `Cursor_Evaluate`
/// would have given us if it worked on macro defs — but
/// extracted from raw source tokens because the libclang
/// evaluator doesn't fire on the preprocessor cursor.
#[cfg(feature = "libclang")]
fn parse_macro_body_tokens(entity: &clang::Entity<'_>) -> Option<MacroValue> {
    let range = entity.get_range()?;
    let toks = range.tokenize();
    // First token is the macro name. Remaining tokens are the body.
    // Use len > 0 to skip empty-body macros (`#define FOO`).
    if toks.len() < 2 {
        return None;
    }
    // Multi-token bodies aren't lowered in v0.
    if toks.len() > 2 {
        return None;
    }
    let body = toks[1].get_spelling();
    parse_literal_token(&body)
}

/// Parse a single literal token's spelling into a [`MacroValue`].
/// Handles the common shapes: signed/unsigned integer (decimal +
/// hex), float, string, `true` / `false`. Suffix characters
/// (`L`, `LL`, `U`, `f`) are stripped per their C++ semantics.
fn parse_literal_token(spelling: &str) -> Option<MacroValue> {
    let s = spelling.trim();
    // Boolean keywords.
    match s {
        "true" => return Some(MacroValue::Bool(true)),
        "false" => return Some(MacroValue::Bool(false)),
        _ => {}
    }
    // String literal: surrounded by `"`.
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        let inner = &s[1..s.len() - 1];
        // Naive unescape — handles `\"` and `\\` only. Real C++
        // string-literal parsing would also handle `\n`, `\t`,
        // `\xNN`, etc. Acceptable for the v0 capture path.
        let unescaped = inner.replace("\\\"", "\"").replace("\\\\", "\\");
        return Some(MacroValue::String(unescaped));
    }
    // Strip integer suffixes (L, LL, U, UL, ULL, etc.) and float
    // suffix (`f`/`F`). Consume from the right while we see
    // suffix characters.
    let mut end = s.len();
    while let Some(c) = s[..end].chars().next_back() {
        if matches!(c, 'L' | 'l' | 'U' | 'u' | 'F' | 'f') {
            end -= c.len_utf8();
        } else {
            break;
        }
    }
    let numeric = &s[..end];
    let unsigned_suffix = end < s.len()
        && s[end..]
            .chars()
            .any(|c| matches!(c, 'U' | 'u'));
    // Hex / octal int literal.
    if let Some(hex) = numeric.strip_prefix("0x").or_else(|| numeric.strip_prefix("0X")) {
        if let Ok(v) = u64::from_str_radix(hex, 16) {
            return Some(if unsigned_suffix {
                MacroValue::UnsignedInteger(v)
            } else {
                MacroValue::SignedInteger(v as i64)
            });
        }
    }
    // Float (must have `.` or `e`/`E`).
    if numeric.contains('.')
        || numeric.contains('e')
        || numeric.contains('E')
    {
        if let Ok(v) = numeric.parse::<f64>() {
            return Some(MacroValue::Float(v));
        }
    }
    // Decimal integer.
    if let Ok(v) = numeric.parse::<i64>() {
        return Some(if unsigned_suffix {
            MacroValue::UnsignedInteger(v as u64)
        } else {
            MacroValue::SignedInteger(v)
        });
    }
    if let Ok(v) = numeric.parse::<u64>() {
        return Some(MacroValue::UnsignedInteger(v));
    }
    None
}

/// Heuristic: skip macros whose names look like clang or system
/// builtins. The capture set should be the user's `#define`
/// constants, not the thousands of preprocessor symbols clang
/// itself injects.
fn is_likely_system_macro(name: &str) -> bool {
    // Anything starting with `__` is reserved for the
    // implementation per the C++ standard (and clang uses these
    // heavily). Same for anything starting with a single `_`
    // followed by an uppercase letter.
    name.starts_with("__")
        || matches!(name.as_bytes(), [b'_', c, ..] if c.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macroset_default_is_empty() {
        let s = MacroSet::default();
        assert!(s.iter().next().is_none());
        assert!(s.get("FOO").is_none());
    }

    #[test]
    fn macroset_get_finds_by_name() {
        let s = MacroSet {
            entries: vec![
                MacroConst {
                    name: "FOO".into(),
                    value: MacroValue::SignedInteger(42),
                },
                MacroConst {
                    name: "BAR".into(),
                    value: MacroValue::Float(3.14),
                },
            ],
        };
        assert_eq!(
            s.get("FOO").map(|m| m.value.clone()),
            Some(MacroValue::SignedInteger(42))
        );
        assert_eq!(
            s.get("BAR").map(|m| m.value.clone()),
            Some(MacroValue::Float(3.14))
        );
        assert!(s.get("BAZ").is_none());
    }

    #[test]
    fn system_macro_filter_rejects_double_underscore() {
        assert!(is_likely_system_macro("__GNUC__"));
        assert!(is_likely_system_macro("__APPLE__"));
        assert!(is_likely_system_macro("_GLIBCXX_HAVE_FOO"));
        assert!(!is_likely_system_macro("FL_RED"));
        assert!(!is_likely_system_macro("MAX_SIZE"));
        assert!(!is_likely_system_macro("Foo"));
    }
}

//! C++ → Rust identifier transformations.
//!
//! See `docs/cxx_importer.md §6, §7`.

use std::collections::HashMap;

use rustc_abi_cxx::{Ident, OperatorKind};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SelfReceiver {
    Shared,
    Mut,
    Owned,
    Static,
}

pub fn map_namespace(name: &Ident) -> Ident {
    name.clone()
}

pub fn map_class(name: &Ident) -> Ident {
    name.clone()
}

pub fn map_method(name: &Ident, is_const: bool) -> (Ident, SelfReceiver) {
    let receiver = if is_const {
        SelfReceiver::Shared
    } else {
        SelfReceiver::Mut
    };
    (name.clone(), receiver)
}

/// Map an [`OperatorKind`] + receiver constness to the Rust identifier
/// the bindings emitter should use for that method. Mirrors the
/// `op_<word>` / `op_<word>_mut` convention from the design doc:
/// const-qualified C++ operators (`operator[]() const`) drop straight
/// to `op_index`, while their non-const siblings get the `_mut` suffix.
///
/// The non-const distinction matters even when both overloads are
/// imported from the same class — each gets a unique Rust name so the
/// `impl` block doesn't collide on method symbols.
pub fn rust_name_for_operator(op: OperatorKind, is_const: bool) -> String {
    let base: &'static str = match op {
        OperatorKind::Plus => "op_add",
        OperatorKind::Minus => "op_sub",
        OperatorKind::Mul => "op_mul",
        OperatorKind::Div => "op_div",
        OperatorKind::Mod => "op_rem",
        OperatorKind::Assign => "op_assign",
        OperatorKind::PlusAssign => "op_add_assign",
        OperatorKind::Eq => "op_eq",
        OperatorKind::Ne => "op_ne",
        OperatorKind::Lt => "op_lt",
        OperatorKind::Le => "op_le",
        OperatorKind::Gt => "op_gt",
        OperatorKind::Ge => "op_ge",
        OperatorKind::Call => "op_call",
        OperatorKind::Index => "op_index",
        OperatorKind::Deref => "op_deref",
        OperatorKind::PreIncr => "op_pre_incr",
        OperatorKind::PreDecr => "op_pre_decr",
    };
    if is_const || is_op_pure_predicate(op) {
        base.into()
    } else {
        format!("{base}_mut")
    }
}

/// `==`, `!=`, `<`, etc. are logically pure even when the C++ side
/// drops the `const` qualifier. We special-case them so users don't
/// see a confusing `op_eq_mut` for a comparison operator that
/// happens to lack a `const` qualifier in some headers.
fn is_op_pure_predicate(op: OperatorKind) -> bool {
    matches!(
        op,
        OperatorKind::Eq
            | OperatorKind::Ne
            | OperatorKind::Lt
            | OperatorKind::Le
            | OperatorKind::Gt
            | OperatorKind::Ge,
    )
}

/// Disambiguate overloaded source names so each method gets a unique
/// Rust identifier in the impl block.
///
/// Algorithm (matches `docs/cxx_importer.md §7`):
///
/// 1. Group entries by `base_name`. If only one entry shares a base
///    name, keep it as-is.
/// 2. For collision groups, pick a deterministic suffix from each
///    entry's `disambiguator` token (typically a stringified
///    parameter-type signature). The first entry in source order
///    keeps the base name; subsequent ones get `<base>_<disamb>`.
///
/// The caller controls the disambiguator string. Common choices:
///   - For overloaded methods: stringified arg-type list joined by
///     `_` (e.g. `push_back_int_const_ref` vs `push_back_int_rref`).
///   - For operator-with-mutability: the `_mut` suffix (already
///     handled by [`rust_name_for_operator`], but also valid here).
///
/// Returns the disambiguated names in the same order as the input.
pub fn disambiguate_overloads<I, S>(entries: I) -> Vec<Ident>
where
    I: IntoIterator<Item = OverloadEntry<S>>,
    S: AsRef<str>,
{
    let entries: Vec<_> = entries.into_iter().collect();

    // First pass: count occurrences of each base name.
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for e in &entries {
        *counts.entry(e.base_name.as_ref()).or_default() += 1;
    }

    // Second pass: emit rename for collision groups, keep base for
    // unique names.
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::with_capacity(entries.len());
    for e in &entries {
        let base = e.base_name.as_ref();
        let count = *counts.get(base).unwrap_or(&1);
        if count == 1 {
            out.push(Ident(base.to_string()));
            continue;
        }
        let nth = seen.entry(base).or_insert(0);
        let name = if *nth == 0 {
            // First occurrence keeps the base name.
            base.to_string()
        } else {
            // Subsequent: append disambiguator. Sanitize so the
            // result is a valid Rust identifier (lowercase, replace
            // non-alphanumerics with `_`).
            let disamb = sanitize_disambiguator(e.disambiguator.as_ref());
            format!("{base}_{disamb}")
        };
        *nth += 1;
        out.push(Ident(name));
    }
    out
}

/// One entry in [`disambiguate_overloads`].
#[derive(Clone, Debug)]
pub struct OverloadEntry<S> {
    pub base_name: S,
    /// A token that distinguishes this overload from siblings with the
    /// same `base_name`. Free-form; sanitized into a valid Rust
    /// identifier suffix during disambiguation.
    pub disambiguator: S,
}

fn sanitize_disambiguator(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_underscore = false;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore && !out.is_empty() {
            out.push('_');
            last_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "ovl".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_const_drops_to_op_word() {
        assert_eq!(rust_name_for_operator(OperatorKind::Plus, true), "op_add");
        assert_eq!(rust_name_for_operator(OperatorKind::Index, true), "op_index");
        assert_eq!(rust_name_for_operator(OperatorKind::Deref, true), "op_deref");
    }

    #[test]
    fn operator_non_const_gets_mut_suffix() {
        assert_eq!(
            rust_name_for_operator(OperatorKind::Plus, false),
            "op_add_mut"
        );
        assert_eq!(
            rust_name_for_operator(OperatorKind::Index, false),
            "op_index_mut"
        );
    }

    #[test]
    fn comparison_operators_skip_mut_suffix() {
        // Comparison ops are logically pure; keeping `op_eq_mut`
        // around just because some header forgot the `const`
        // qualifier would be confusing.
        for op in [
            OperatorKind::Eq,
            OperatorKind::Ne,
            OperatorKind::Lt,
            OperatorKind::Le,
            OperatorKind::Gt,
            OperatorKind::Ge,
        ] {
            assert!(
                !rust_name_for_operator(op, false).ends_with("_mut"),
                "{op:?} (non-const) shouldn't get _mut suffix"
            );
        }
    }

    #[test]
    fn unique_base_names_pass_through_unchanged() {
        let names = disambiguate_overloads([
            OverloadEntry { base_name: "alpha", disambiguator: "i32" },
            OverloadEntry { base_name: "beta", disambiguator: "f64" },
        ]);
        assert_eq!(names[0].0, "alpha");
        assert_eq!(names[1].0, "beta");
    }

    #[test]
    fn collision_first_keeps_base_rest_get_disambiguator_suffix() {
        let names = disambiguate_overloads([
            OverloadEntry { base_name: "push_back", disambiguator: "T_const_ref" },
            OverloadEntry { base_name: "push_back", disambiguator: "T_rref" },
            OverloadEntry { base_name: "alone", disambiguator: "" },
        ]);
        assert_eq!(names[0].0, "push_back");
        assert_eq!(names[1].0, "push_back_t_rref");
        assert_eq!(names[2].0, "alone");
    }

    #[test]
    fn three_way_collision_gets_three_distinct_names() {
        let names = disambiguate_overloads([
            OverloadEntry { base_name: "f", disambiguator: "int" },
            OverloadEntry { base_name: "f", disambiguator: "double" },
            OverloadEntry { base_name: "f", disambiguator: "char_star" },
        ]);
        assert_eq!(names[0].0, "f");
        assert_eq!(names[1].0, "f_double");
        assert_eq!(names[2].0, "f_char_star");
    }

    #[test]
    fn empty_disambiguator_falls_back_to_ovl() {
        let names = disambiguate_overloads([
            OverloadEntry { base_name: "f", disambiguator: "" },
            OverloadEntry { base_name: "f", disambiguator: "" },
        ]);
        assert_eq!(names[0].0, "f");
        assert_eq!(names[1].0, "f_ovl");
    }
}

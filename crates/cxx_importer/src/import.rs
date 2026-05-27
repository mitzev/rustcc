//! libclang-backed importer — translates a C++ translation unit into
//! `rustc_abi_cxx`'s `CxxTypeCtx`.
//!
//! Current scope (validated by the libclang-gated suite in
//! `tests/import.rs`):
//!
//! - **Records**: top-level + namespace-nested struct / class /
//!   union definitions. Anonymous and named namespaces.
//! - **Fields**: primitives, pointers, references, nested records
//!   (by-value composition), arrays, enums.
//! - **Inheritance**: non-virtual (single + multiple), virtual
//!   bases including diamond. Polymorphism, vtable layout, and
//!   `is_polymorphic` propagation through base chains.
//! - **Methods**: instance + static methods, ctors (default + copy
//!   + move + `OtherCtor`), dtors, copy/move-assign operators,
//!   user-declared operator overloads (with the short Itanium codes
//!   `pl`, `ix`, …), conversion functions (`operator T()`).
//! - **Templates**: explicit class-template specializations (`vector<int>`).
//!
//! Self-doc gaps (open follow-ups, smaller still):
//!
//! - Annotations engine (`[[rustcc::*]]` inline + sidecar YAML).
//!   The schema and consumer types are defined in
//!   `crates/cxx_importer/src/annotations.rs`, but no clang-side
//!   walker reads `[[clang::annotate(...)]]` cursors during import
//!   yet. Tracked for a follow-up release.
//! - Pure virtuals: `populate_vtable_indices` skips them in v0
//!   because their slot target is the shared `__cxa_pure_virtual`
//!   symbol and `MethodId`-based disambiguation conflicts with
//!   the importer's eager method-vector clones. The bindings
//!   emitter rejects pure virtuals with a clear error.
//! - Multi-inheritance / virtual-base classes whose primary
//!   subobject doesn't sit at offset 0 — `populate_vtable_indices`
//!   walks the primary sub-table only, so secondary vtables
//!   aren't reflected in `MethodDef::vtable_index`. The
//!   single-inheritance case (the common one) works today.
//! - Templates: explicit/auto instantiations import as concrete
//!   classes. Type and non-type (integral) template arguments are
//!   captured and mangled (`Box<int>`, `Arr<int, 4>`); `Build`'s
//!   auto-instantiate pass + `Driver`'s discovery pre-scan
//!   force-instantiate specializations referenced in the headers so
//!   no hand-written list is needed for the common case. Still
//!   skipped: a method signature mentioning a *nested* template
//!   specialization parameterised on the class's own parameter (e.g.
//!   a method returning `Box<T>`); the bare `CXCursor_ClassTemplate`
//!   with no instantiation (Rust has no generic-C++-template
//!   representation); and template-template / pointer-to-member
//!   non-type arguments (libclang's type-only view can't recover
//!   them — they're rejected, not mis-mangled).
//!
//! Recursive import: when a field or base references a record type,
//! the importer recursively lowers that record before continuing. Each
//! class's USR (Clang's stable Unified-Symbol-Resolution identifier)
//! is cached to avoid duplicate work and to resolve repeat references
//! to the same class within a translation unit.

use std::collections::HashMap;
use std::path::Path;

use clang::{
    Clang, Entity, EntityKind, EntityVisitResult, ExceptionSpecification, Index,
    RefQualifier, TemplateArgument, Type, TypeKind,
};
use rustc_abi_cxx::{
    Access, BaseSpec, ClassDef, ClassId, CvQual, CxxType, CxxTypeCtx,
    FieldDef, FloatKind, FnSig, Ident, IntWidth, MethodDef, MethodName,
    NameSegment, NestedName, OperatorKind, RecordKind, RefKind, SpecialMember,
    TemplateArg, TypeId, VTableEntry, Virtuality,
};

use crate::aliases::{AliasSet, TypeAlias};
use crate::annotations::{Annotation, AnnotationSet};
use crate::diagnostics::{ImportError, SourceSpan};
use crate::enums::{CxxEnumDef, CxxEnumVariant, EnumSet};
use crate::free_fns::{FreeFnDef, FreeFnSet};
use crate::static_data::{StaticDataDef, StaticDataSet};

pub fn import_header(
    source: &Path,
    args: &[&str],
    ctx: &mut CxxTypeCtx,
) -> Result<Vec<ClassId>, ImportError> {
    let mut cache = HashMap::new();
    import_header_with_cache(source, args, ctx, &mut cache)
}

/// Like [`import_header`], but additionally returns the
/// [`AnnotationSet`] populated from inline
/// `[[clang::annotate("rustcc::…")]]` attributes on classes and
/// methods. Use this entry point with
/// `rust_bindings::generate_rust_bindings_with_annotations` to
/// honor user-supplied name overrides and nullability markers.
pub fn import_header_with_annotations(
    source: &Path,
    args: &[&str],
    ctx: &mut CxxTypeCtx,
) -> Result<(Vec<ClassId>, AnnotationSet), ImportError> {
    let mut cache = HashMap::new();
    let mut set = AnnotationSet::default();
    let mut aliases = AliasSet::default();
    let mut enums = EnumSet::default();
    let mut free_fns = FreeFnSet::default();
    let mut static_data = StaticDataSet::default();
    let ids = import_header_full(
        source,
        args,
        ctx,
        &mut cache,
        &mut set,
        &mut aliases,
        &mut enums,
        &mut free_fns,
        &mut static_data,
    )?;
    let _ = (aliases, enums, free_fns, static_data); // discard — caller wanted only annotations.
    Ok((ids, set))
}

/// Bundle of per-import side-tables produced alongside the class list.
///
/// Returned by [`import_header_with_extras`]. Each field corresponds to
/// a side-channel that the importer harvests in addition to the bare
/// class graph:
///
/// - `annotations` — `[[clang::annotate("rustcc::…")]]` markup
///   parsed off classes and methods (M6).
/// - `aliases` — `typedef` / `using` declarations at TU/namespace
///   scope (M17).
/// - `enums` — `enum` / `enum class` definitions at TU/namespace
///   scope, including variant lists (M16).
/// - `free_fns` — free functions at TU/namespace scope (M11.b).
/// - `static_data` — class-scope static data members (M11.c).
#[derive(Default, Clone, Debug)]
pub struct ImportExtras {
    pub annotations: AnnotationSet,
    pub aliases: AliasSet,
    pub enums: EnumSet,
    pub free_fns: FreeFnSet,
    pub static_data: StaticDataSet,
}

/// One-shot import that returns every side-table the importer can
/// produce. Use this when you want bindings emitted with full
/// fidelity (annotations + aliases + enum bodies).
pub fn import_header_with_extras(
    source: &Path,
    args: &[&str],
    ctx: &mut CxxTypeCtx,
) -> Result<(Vec<ClassId>, ImportExtras), ImportError> {
    let mut cache = HashMap::new();
    let mut annotations = AnnotationSet::default();
    let mut aliases = AliasSet::default();
    let mut enums = EnumSet::default();
    let mut free_fns = FreeFnSet::default();
    let mut static_data = StaticDataSet::default();
    let ids = import_header_full(
        source,
        args,
        ctx,
        &mut cache,
        &mut annotations,
        &mut aliases,
        &mut enums,
        &mut free_fns,
        &mut static_data,
    )?;
    Ok((
        ids,
        ImportExtras {
            annotations,
            aliases,
            enums,
            free_fns,
            static_data,
        },
    ))
}

/// Internal entry point used by [`Driver::parse_all`] to share its
/// USR cache and accumulate annotations + aliases across multiple
/// header roots in a single pass.
pub(crate) fn import_header_full(
    source: &Path,
    args: &[&str],
    ctx: &mut CxxTypeCtx,
    cache: &mut HashMap<String, ClassId>,
    annotations: &mut AnnotationSet,
    aliases: &mut AliasSet,
    enums: &mut EnumSet,
    free_fns: &mut FreeFnSet,
    static_data: &mut StaticDataSet,
) -> Result<Vec<ClassId>, ImportError> {
    let (
        ids,
        captured_aliases,
        captured_enums,
        captured_free_fns,
        captured_static_data,
    ) = import_header_with_cache_and_aliases(source, args, ctx, cache)?;
    aliases.entries.extend(captured_aliases);
    enums.entries.extend(captured_enums);
    free_fns.entries.extend(captured_free_fns);
    static_data.entries.extend(captured_static_data);
    // The cache-and-annotations collection is currently re-derived
    // by re-running the importer when the caller wants annotations;
    // a follow-up release can plumb annotations through the
    // existing `import_header_with_cache` call to avoid double work.
    let mut set_cache: HashMap<String, ClassId> = std::mem::take(cache);
    let collected = collect_annotations(source, args, ctx, &mut set_cache)?;
    *cache = set_cache;
    for (key, anns) in collected {
        annotations
            .inline
            .entry(key)
            .or_default()
            .extend(anns);
    }
    Ok(ids)
}

/// Re-run the libclang parse just to harvest annotations. This is
/// a temporary measure — the next iteration should fold the
/// annotation walk directly into [`import_header_with_cache`] so
/// each header is parsed once.
pub(crate) fn collect_annotations(
    source: &Path,
    args: &[&str],
    ctx: &mut CxxTypeCtx,
    cache: &mut HashMap<String, ClassId>,
) -> Result<HashMap<String, Vec<Annotation>>, ImportError> {
    let _ = (ctx, cache);
    let clang = Clang::new().map_err(|e| ImportError::ClangDiagnostic {
        file: source.display().to_string(),
        line: 0,
        message: format!("failed to initialize libclang: {e}"),
    })?;
    collect_annotations_with_clang(&clang, source, args)
}

/// v1.12.14: same as [`collect_annotations`] but reuses an
/// existing `Clang` instance. Callers running multiple imports
/// in the same process (e.g. `crate::build::Build::compile`)
/// hoist `Clang::new()` to the top of their loop and pass the
/// shared instance — re-initing libclang per parse has been
/// observed to segfault on libclang 17+ when ASTs from earlier
/// parses are still in scope. See
/// `import_header_with_clang`'s doc-comment for the long form.
pub(crate) fn collect_annotations_with_clang(
    clang: &Clang,
    source: &Path,
    args: &[&str],
) -> Result<HashMap<String, Vec<Annotation>>, ImportError> {
    let index = Index::new(clang, false, false);
    let tu = index
        .parser(source)
        .arguments(args)
        .parse()
        .map_err(|e| ImportError::ClangDiagnostic {
            file: source.display().to_string(),
            line: 0,
            message: format!("parse failed: {e:?}"),
        })?;

    let mut out: HashMap<String, Vec<Annotation>> = HashMap::new();
    walk_for_annotations(&tu.get_entity(), &mut out);
    Ok(out)
}

fn walk_for_annotations(
    entity: &Entity<'_>,
    out: &mut HashMap<String, Vec<Annotation>>,
) {
    match entity.get_kind() {
        EntityKind::StructDecl
        | EntityKind::ClassDecl
        | EntityKind::UnionDecl
        | EntityKind::Method
        | EntityKind::Constructor
        | EntityKind::Destructor
        | EntityKind::ConversionFunction
        | EntityKind::FunctionDecl => {
            // FunctionDecl picks up free functions at TU /
            // namespace scope (v1.12.2 — for the
            // `rustcc::cxx_throws` annotation). Class-scope
            // methods are not `FunctionDecl` in libclang's
            // schema; they go through the `Method` /
            // `Constructor` / etc. arms above.
            let anns = read_annotations(entity);
            if !anns.is_empty() {
                out.insert(entity_fqn(entity), anns);
            }
        }
        _ => {}
    }
    for child in entity.get_children() {
        walk_for_annotations(&child, out);
    }
}

/// Like [`import_header`], but threads a caller-owned USR→`ClassId`
/// cache through. Used by [`crate::Driver::parse_all`] to deduplicate
/// classes that appear in multiple root headers (e.g. a `common.h`
/// `#include`d from two different TUs): the second TU's references to
/// the same USR reuse the `ClassId` minted during the first TU rather
/// than allocating a fresh duplicate.
pub(crate) fn import_header_with_cache(
    source: &Path,
    args: &[&str],
    ctx: &mut CxxTypeCtx,
    cache: &mut HashMap<String, ClassId>,
) -> Result<Vec<ClassId>, ImportError> {
    // Aliases + enums are silently dropped on this back-compat
    // entry point. Callers that want them call
    // `import_header_with_cache_and_aliases` (or the public
    // `import_header_with_extras` wrapper) directly.
    let (ids, _aliases, _enums, _free_fns, _static_data) =
        import_header_with_cache_and_aliases(source, args, ctx, cache)?;
    Ok(ids)
}

/// Same as [`import_header_with_cache`], but also returns the
/// list of TU/namespace-scope `typedef` / `using` aliases (M17)
/// and `enum` / `enum class` definitions (M16) harvested from the
/// same parse. Internal because the public face for this is
/// [`import_header_with_extras`] (which bundles all the
/// side-tables together with annotations).
pub(crate) fn import_header_with_cache_and_aliases(
    source: &Path,
    args: &[&str],
    ctx: &mut CxxTypeCtx,
    cache: &mut HashMap<String, ClassId>,
) -> Result<
    (Vec<ClassId>, Vec<TypeAlias>, Vec<CxxEnumDef>, Vec<FreeFnDef>, Vec<StaticDataDef>),
    ImportError,
> {
    // One-shot path: mint a fresh `Clang` for this single header
    // parse. For multi-header builds (`Driver::parse_all`), the
    // caller hoists `Clang::new()` to the top of the loop and
    // calls [`import_header_with_clang`] directly — re-creating
    // the global libclang state per header has been observed to
    // segfault on libclang 17+ on macOS arm64 when the second
    // parse touches AST nodes referenced by the first.
    let clang = Clang::new().map_err(|e| ImportError::ClangDiagnostic {
        file: source.display().to_string(),
        line: 0,
        message: format!("failed to initialize libclang: {e}"),
    })?;
    import_header_with_clang(&clang, source, args, ctx, cache)
}

/// Variant of [`import_header_with_cache_and_aliases`] that takes a
/// caller-owned [`Clang`] instance instead of constructing one
/// internally. Use this when parsing multiple headers in the same
/// process — `Clang::new()` is the global libclang init and
/// re-initing it per parse has been observed to segfault on
/// libclang 17+ when ASTs from earlier parses are still in scope.
pub(crate) fn import_header_with_clang(
    clang: &Clang,
    source: &Path,
    args: &[&str],
    ctx: &mut CxxTypeCtx,
    cache: &mut HashMap<String, ClassId>,
) -> Result<
    (Vec<ClassId>, Vec<TypeAlias>, Vec<CxxEnumDef>, Vec<FreeFnDef>, Vec<StaticDataDef>),
    ImportError,
> {
    let index = Index::new(clang, false, false);
    let tu = index
        .parser(source)
        .arguments(args)
        .parse()
        .map_err(|e| ImportError::ClangDiagnostic {
            file: source.display().to_string(),
            line: 0,
            message: format!("parse failed: {e:?}"),
        })?;

    let mut importer = Importer::with_cache(ctx, std::mem::take(cache));
    let mut imported: Vec<ClassId> = Vec::new();
    let mut seen: std::collections::HashSet<ClassId> =
        std::collections::HashSet::new();

    // First pass: import all class/struct/union definitions reachable
    // from the TU, recursing into namespaces. Also captures
    // TU/namespace-scope `typedef` / `using` aliases (M17).
    for child in tu.get_entity().get_children() {
        walk_top_level(&child, &mut importer, &mut imported, &mut seen)?;
    }

    // Second pass: recursively scan the entire TU for method
    // definitions whose semantic parent matches a class we imported,
    // and attach them. This catches two cases the direct child walk
    // misses:
    //   - Out-of-class method definitions (`void Foo::bar() {}`) at
    //     TU or namespace scope.
    //   - Template-specialization methods nested inside the spec's
    //     AST subtree that libclang doesn't surface via `get_children`
    //     on the spec cursor.
    attach_methods_recursively(tu.get_entity(), &mut importer)?;

    // Third pass (M22): re-finalize is_polymorphic and re-run
    // populate_vtable_indices for every class we touched.
    //
    // Why a third pass is needed: `import_class` does both jobs at
    // the end of its body walk, but during that body walk it can
    // recursively trigger `import_class` for *related* classes via
    // method-parameter type lookups. E.g. importing Fl_Widget walks
    // its method `Fl_Group* parent() const`, which triggers
    // `import_class(Fl_Group)` mid-Fl_Widget-body. Fl_Group's body
    // walk then sees Fl_Widget as a placeholder (methods=0,
    // is_polymorphic=false) — Fl_Widget's body walk hasn't finished
    // and added them yet. Fl_Group's vtable then gets built without
    // the inherited dtor slots, and `populate_vtable_indices(Fl_Group)`
    // writes a bogus index map.
    //
    // Once *every* class is fully imported, re-running the same two
    // computations yields the correct steady-state result. Polymorphism
    // is recomputed in a fixed-point loop because the
    // base-is-polymorphic bit propagates one inheritance edge per
    // iteration.
    {
        let class_ids: Vec<ClassId> = importer.classes.values().copied().collect();
        // Polymorphism convergence loop.
        loop {
            let mut changed = false;
            for &id in &class_ids {
                let new_poly = recompute_is_polymorphic(importer.ctx, id);
                if importer.ctx.class(id).is_polymorphic != new_poly {
                    importer.ctx.class_mut(id).is_polymorphic = new_poly;
                    changed = true;
                }
            }
            if !changed { break; }
        }
        // Now re-run populate_vtable_indices for every polymorphic
        // class. populate_vtable_indices is idempotent: it overwrites
        // `vtable_index` from the freshly-recomputed vtable, so any
        // stale index from the in-class call gets corrected.
        for &id in &class_ids {
            if importer.ctx.class(id).is_polymorphic {
                populate_vtable_indices(importer.ctx, id);
            }
        }
    }

    // Hand the accumulated USR map back to the caller so the next
    // import call can dedup against it. Drain side-tables (aliases,
    // enums, free fns) so they ride out alongside the class list.
    let aliases = std::mem::take(&mut importer.aliases);
    let enums = std::mem::take(&mut importer.enums);
    let free_fns = std::mem::take(&mut importer.free_fns);
    let static_data = std::mem::take(&mut importer.static_data);
    *cache = importer.into_cache();
    Ok((imported, aliases, enums, free_fns, static_data))
}

fn attach_methods_recursively(
    root: Entity<'_>,
    importer: &mut Importer<'_>,
) -> Result<(), ImportError> {
    // Collect method entities first, then process. Avoids nested borrows
    // between the visitor closure and `importer`.
    let mut method_entities: Vec<Entity<'_>> = Vec::new();
    root.visit_children(|entity, _parent| match entity.get_kind() {
        EntityKind::Method
        | EntityKind::Constructor
        | EntityKind::Destructor
        | EntityKind::ConversionFunction => {
            method_entities.push(entity);
            EntityVisitResult::Continue
        }
        _ => EntityVisitResult::Recurse,
    });

    for method_entity in method_entities {
        let parent = match method_entity.get_semantic_parent() {
            Some(p) => p,
            None => continue,
        };
        let parent_usr = match parent.get_usr() {
            Some(u) => u.0,
            None => continue,
        };
        let class_id = match importer.classes.get(&parent_usr) {
            Some(&id) => id,
            None => continue,
        };
        let parent_name = parent.get_name().unwrap_or_default();
        // Same access filter as the in-class child walk: skip
        // protected / private methods. A free C trampoline can't
        // legally call them, and emitting shims for them would
        // produce un-compilable C++ source. Out-of-class
        // declarations (`void Foo::bar() {}` at TU scope) carry
        // the access info on the cursor too.
        if matches!(
            method_entity.get_accessibility(),
            Some(clang::Accessibility::Protected)
                | Some(clang::Accessibility::Private),
        ) {
            continue;
        }
        // `lower_method` may fail if the method uses an unsupported type
        // kind; ignore those cases rather than aborting the whole import
        // (they're typically template primary-definition methods with
        // un-substituted parameter types, which the spec-specific
        // instantiated version will also supply).
        let method =
            match importer.lower_method(&method_entity, &parent_name, class_id)
            {
                Ok(m) => m,
                Err(_) => continue,
            };
        let class = importer.ctx.class(class_id);
        let duplicate = class.methods.iter().any(|m| {
            m.name == method.name
                && m.sig.cv == method.sig.cv
                && m.sig.params == method.sig.params
        });
        if !duplicate {
            // M11: detect static methods on the just-pushed
            // entry. Same `is_static_method()` check as in
            // `import_class`'s child walk, deferred until after
            // the push so we can capture the final method index.
            let is_static = matches!(method_entity.get_kind(), EntityKind::Method)
                && method_entity.is_static_method();
            // M18: capture the trailing-default-arg count from
            // `lower_method`'s scratch slot before any further
            // call clobbers it.
            let default_count = importer.last_method_default_count;
            let method_idx = importer.ctx.class(class_id).methods.len();
            importer.ctx.class_mut(class_id).methods.push(method);
            if is_static {
                importer.ctx.mark_method_static(class_id, method_idx);
            }
            if default_count > 0 {
                importer.ctx.record_default_arg_count(
                    class_id,
                    method_idx,
                    default_count,
                );
            }
        }
    }
    Ok(())
}

/// Recursively visit top-level and namespace-nested declarations.
/// Records any struct/class *definition* we find by delegating to
/// `Importer::import_class`, which caches by USR.
fn walk_top_level(
    entity: &Entity<'_>,
    importer: &mut Importer<'_>,
    imported: &mut Vec<ClassId>,
    seen: &mut std::collections::HashSet<ClassId>,
) -> Result<(), ImportError> {
    match entity.get_kind() {
        EntityKind::Namespace => {
            // Anonymous and named namespaces alike — just descend.
            for child in entity.get_children() {
                walk_top_level(&child, importer, imported, seen)?;
            }
        }
        EntityKind::StructDecl
        | EntityKind::ClassDecl
        | EntityKind::UnionDecl => {
            if entity.is_definition() {
                let id = importer.import_class(entity)?;
                if seen.insert(id) {
                    imported.push(id);
                }
            }
        }
        // M16: capture `enum`, `enum class`, `enum struct` at
        // TU/namespace scope. Same parent-scope filter as
        // aliases — class-scope enums are deferred.
        EntityKind::EnumDecl => {
            let parent_kind = entity
                .get_semantic_parent()
                .map(|p| p.get_kind());
            let at_ns_scope = matches!(
                parent_kind,
                Some(EntityKind::Namespace)
                    | Some(EntityKind::TranslationUnit)
                    | Some(EntityKind::NotImplemented)
                    | None
            );
            if at_ns_scope {
                let _ = importer.collect_enum(entity);
            }
        }
        // M17: capture C++ `typedef T U;` and `using U = T;` at
        // TU/namespace scope. Failures are non-fatal — aliases
        // are emit-only ergonomics, so a target type we can't
        // import (e.g. a templated stdlib helper) just gets
        // skipped instead of poisoning the whole TU.
        EntityKind::TypedefDecl | EntityKind::TypeAliasDecl => {
            // Class-scope aliases require associated-type emission
            // we don't have yet; only namespace-scope aliases
            // ride the v0 path. Anything whose semantic parent
            // isn't a Namespace / TU / NotImplemented (the kind
            // libclang reports for the TU root in some libclang
            // builds) is dropped.
            let parent_kind = entity
                .get_semantic_parent()
                .map(|p| p.get_kind());
            let at_ns_scope = matches!(
                parent_kind,
                Some(EntityKind::Namespace)
                    | Some(EntityKind::TranslationUnit)
                    | Some(EntityKind::NotImplemented)
                    | None
            );
            if at_ns_scope {
                let _ = importer.collect_alias(entity);
            }
        }
        // M11.b: capture free functions at TU/namespace scope.
        // Static methods on classes already flow through the
        // class child-walk via M11.a; this branch picks up the
        // truly-free declarations (`fl_message`, `fl_color`) that
        // FLTK uses heavily. Skip class-scope friend functions
        // and operator overloads — those need extra naming
        // machinery we don't have yet.
        EntityKind::FunctionDecl => {
            let parent_kind = entity
                .get_semantic_parent()
                .map(|p| p.get_kind());
            let at_ns_scope = matches!(
                parent_kind,
                Some(EntityKind::Namespace)
                    | Some(EntityKind::TranslationUnit)
                    | Some(EntityKind::NotImplemented)
                    | None
            );
            if at_ns_scope {
                let _ = importer.collect_free_fn(entity);
            }
        }
        _ => {}
    }
    Ok(())
}

struct Importer<'a> {
    ctx: &'a mut CxxTypeCtx,
    // Clang's USR (unified symbol resolution) is the stable identity
    // for a decl; we use it to dedup repeat references to the same class
    // within the translation unit.
    classes: HashMap<String, ClassId>,
    /// Inline `[[clang::annotate("rustcc::…")]]` annotations parsed
    /// from class + method entities, keyed by fully-qualified C++
    /// path (e.g. `ns::Foo`, `ns::Foo::bar`).
    annotations: HashMap<String, Vec<Annotation>>,
    /// M17: type aliases captured at TU/namespace scope. Stored
    /// in source-declaration order. Class-scope aliases are
    /// deferred (see `aliases.rs` module docs).
    aliases: Vec<TypeAlias>,
    /// USR-keyed dedup for aliases. The same `using` declared
    /// in a header included from two roots would otherwise emit
    /// twice; libclang gives each a stable USR which we dedup
    /// against here.
    alias_usrs: std::collections::HashSet<String>,
    /// M16: imported enum bodies (variants + scoped flag) at
    /// TU/namespace scope. Class-scope enums are deferred for
    /// the same reason as class-scope aliases.
    enums: Vec<CxxEnumDef>,
    /// USR-keyed dedup for enums. Same rationale as `alias_usrs`.
    enum_usrs: std::collections::HashSet<String>,
    /// M11.b: free functions at TU/namespace scope. Same
    /// dedup-by-USR pattern as aliases / enums.
    free_fns: Vec<FreeFnDef>,
    free_fn_usrs: std::collections::HashSet<String>,
    /// M11.c: class-scope static data members. Captured during
    /// the per-class child walk; emission groups them under the
    /// owning class's `impl` block. Dedup'd by USR like the
    /// other side-tables.
    static_data: Vec<StaticDataDef>,
    static_data_usrs: std::collections::HashSet<String>,
    /// M18: scratch slot — `lower_method` writes the count of
    /// trailing default-argument parameters here as a side
    /// effect, and the call sites read it after pushing the
    /// returned `MethodDef` to record on the ctx side-table
    /// keyed by `(class, method_idx)`. Reset to 0 on every
    /// `lower_method` entry so a method without defaults
    /// doesn't pick up the previous method's count.
    last_method_default_count: usize,
    /// M24: template-parameter substitution map active when
    /// walking a class-template specialization's methods. Keyed
    /// by parameter name (`T`, `U`, ...) — the `lower_method`
    /// call iterates the underlying template's *un-substituted*
    /// method cursors, and `import_type` consults this map when
    /// it encounters an `Unexposed` type whose declaration is a
    /// `TemplateTypeParameter`. Reset to empty before any
    /// non-spec class is imported.
    current_template_subst: HashMap<String, TypeId>,
}

impl<'a> Importer<'a> {
    fn new(ctx: &'a mut CxxTypeCtx) -> Self {
        Self::with_cache(ctx, HashMap::new())
    }

    fn with_cache(
        ctx: &'a mut CxxTypeCtx,
        classes: HashMap<String, ClassId>,
    ) -> Self {
        Self {
            ctx,
            classes,
            annotations: HashMap::new(),
            aliases: Vec::new(),
            alias_usrs: std::collections::HashSet::new(),
            enums: Vec::new(),
            enum_usrs: std::collections::HashSet::new(),
            free_fns: Vec::new(),
            free_fn_usrs: std::collections::HashSet::new(),
            static_data: Vec::new(),
            static_data_usrs: std::collections::HashSet::new(),
            last_method_default_count: 0,
            current_template_subst: HashMap::new(),
        }
    }

    fn into_cache(self) -> HashMap<String, ClassId> {
        self.classes
    }

    fn into_annotations(self) -> HashMap<String, Vec<Annotation>> {
        self.annotations
    }

    fn into_aliases(self) -> Vec<TypeAlias> {
        self.aliases
    }

    /// Register a poison node for an entity whose lowering failed
    /// recoverably. Returns the [`ClassId`] consumers should refer
    /// to — the class definition is empty (no fields, no methods,
    /// no bases), and `ctx.is_poisoned(id)` returns `true` so the
    /// bindings emitter can render an opaque struct with a doc
    /// comment carrying `reason`.
    fn poison_class(
        &mut self,
        entity: &Entity<'_>,
        name: NestedName,
        reason: String,
    ) -> ClassId {
        let placeholder = ClassDef {
            name,
            bases: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            kind: RecordKind::Struct,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        };
        let id = self.ctx.define_class(placeholder);
        // Attach a span-augmented reason if the entity has a
        // location. The `ctx.poison` side-table stores reason
        // strings; we prefix with the C++ source location when
        // available.
        let reason_with_span = match span_of_entity(entity) {
            Some(span) => format!("{span}: {reason}"),
            None => reason,
        };
        self.ctx.poison(id, reason_with_span);
        // Keep USR-cache consistency so subsequent references to
        // the same forward-decl don't re-mint another poison node.
        if let Some(usr) = entity.get_usr() {
            self.classes.insert(usr.0, id);
        }
        id
    }

    /// M17: harvest a `typedef`/`using` alias at TU or namespace
    /// scope. Returns `Ok(())` whether or not the alias was
    /// recorded — the only "errors" worth signaling here are
    /// importer-level invariants, and target-type lookup failures
    /// silently skip (aliases are emit-only ergonomics).
    fn collect_alias(&mut self, entity: &Entity<'_>) -> Result<(), ImportError> {
        // Skip duplicates: the same alias declaration in a
        // header included from two roots otherwise emits twice.
        if let Some(usr) = entity.get_usr() {
            if !self.alias_usrs.insert(usr.0) {
                return Ok(());
            }
        }
        let name = match entity.get_name() {
            Some(n) if !n.is_empty() => n,
            _ => return Ok(()),
        };
        // libclang exposes the underlying type of a typedef-decl
        // via `get_typedef_underlying_type`. For `TypeAliasDecl`
        // (`using U = T;`) the same accessor returns the RHS.
        let underlying = match entity.get_typedef_underlying_type() {
            Some(t) => t,
            None => return Ok(()),
        };
        let where_ = format!("alias `{name}`");
        let target = match self.import_type(underlying, &where_) {
            Ok(id) => id,
            // Target type unsupported (e.g. references templated
            // stdlib types). Drop the alias rather than failing
            // the import — the user can re-add it by hand if
            // needed.
            Err(_) => return Ok(()),
        };
        // M17 + M17.b: walk every ancestor that contributes to
        // the alias's qualified name — namespaces *and* class
        // segments so a class-scope `using It = int;` lands with
        // its parent encoded as
        // `[Namespace("ns"), Class("Outer")]`.
        let parent_segments = build_full_parent_path(entity);
        self.aliases.push(TypeAlias {
            parent: parent_segments,
            name: Ident(name),
            target,
        });
        Ok(())
    }

    /// M16: harvest a `enum class` / `enum struct` / plain `enum`
    /// at TU or namespace scope. Returns `Ok(())` regardless of
    /// outcome — failures (missing underlying type, anonymous
    /// enum, …) silently skip just like aliases.
    fn collect_enum(&mut self, entity: &Entity<'_>) -> Result<(), ImportError> {
        // Forward declarations (`enum class Foo;`) carry no body.
        // libclang reports them as definitions only after the body
        // is seen, so this also dedups the case where the same
        // enum appears in multiple TU roots.
        if !entity.is_definition() {
            return Ok(());
        }
        if let Some(usr) = entity.get_usr() {
            if !self.enum_usrs.insert(usr.0) {
                return Ok(());
            }
        }
        let name = match entity.get_name() {
            Some(n) if !n.is_empty() && !is_synthetic_anonymous_name(&n) => n,
            // Anonymous enums (`enum { Red, Green };`) — for v0
            // we drop them; the variants leak as integer
            // constants in the source but Rust has nowhere
            // to hang them as a distinct named enum. Some
            // libclang builds report anonymous enums with a
            // synthetic name like
            // `(unnamed enum at /.../foo.h:42:1)` instead of
            // an empty string, so filter those too.
            _ => return Ok(()),
        };
        let underlying_ty = match entity.get_enum_underlying_type() {
            Some(t) => t,
            None => return Ok(()),
        };
        let where_ = format!("enum `{name}`");
        let underlying = match self.import_type(underlying_ty, &where_) {
            Ok(id) => id,
            Err(_) => return Ok(()),
        };
        let scoped = entity.is_scoped();

        // Walk children for `EnumConstantDecl`s. libclang exposes
        // each variant as a child cursor; the order matches source
        // order, which we want to preserve in emission.
        let mut variants: Vec<CxxEnumVariant> = Vec::new();
        for child in entity.get_children() {
            if child.get_kind() != EntityKind::EnumConstantDecl {
                continue;
            }
            let vname = match child.get_name() {
                Some(n) => n,
                None => continue,
            };
            let (signed, _unsigned) = match child.get_enum_constant_value() {
                Some(pair) => pair,
                None => continue,
            };
            variants.push(CxxEnumVariant {
                name: vname,
                value: signed,
            });
        }

        // M16 + M16.b: include class-scope ancestors so a
        // class-scope `enum class E { … };` lands with parent
        // `[…, Class("Outer")]`.
        let parent_segments = build_full_parent_path(entity);

        self.enums.push(CxxEnumDef {
            parent: parent_segments,
            name: Ident(name),
            underlying,
            scoped,
            variants,
        });
        Ok(())
    }

    /// M11.b: harvest a free function at TU or namespace scope.
    /// Skips on any failure — free fns are emit-only ergonomics
    /// just like aliases / enums; a function whose parameter type
    /// we can't import (templated, member-pointer, etc.) gets
    /// dropped instead of poisoning the whole TU.
    ///
    /// Filtered out:
    /// - Operator overloads at namespace scope (rare; need
    ///   identifier-mapping work we haven't done).
    /// - Compiler-generated builtins (`__builtin_*`,
    ///   `__sync_fetch_*`) that libclang surfaces as
    ///   `FunctionDecl`s on some configurations.
    /// - Variadic-only / inline body functions: kept; the
    ///   importer captures the syntactic signature regardless.
    fn collect_free_fn(&mut self, entity: &Entity<'_>) -> Result<(), ImportError> {
        let name = match entity.get_name() {
            Some(n) if !n.is_empty() => n,
            _ => return Ok(()),
        };
        // Filter compiler builtins. They're never useful from
        // Rust and FLTK headers transitively pull a few in.
        if name.starts_with("__builtin_")
            || name.starts_with("__sync_")
            || name.starts_with("__atomic_")
        {
            return Ok(());
        }
        // Operator overloads at namespace scope — defer.
        if name.starts_with("operator")
            && name
                .chars()
                .nth("operator".len())
                .is_some_and(|c| !c.is_alphanumeric() && c != '_')
        {
            return Ok(());
        }
        // Dedup by USR — the same function declared in a header
        // included from two roots otherwise emits twice.
        if let Some(usr) = entity.get_usr() {
            if !self.free_fn_usrs.insert(usr.0) {
                return Ok(());
            }
        }

        let where_ = format!("free fn `{name}`");

        // Lower the result type.
        let ret_ty = match entity.get_result_type() {
            Some(t) => t,
            None => return Ok(()),
        };
        let ret = match self.import_type(ret_ty, &where_) {
            Ok(id) => id,
            Err(_) => return Ok(()),
        };

        // Lower parameter types from `ParmDecl` children. We use
        // the cursor walk (not `Type::get_argument_types()`)
        // because the cursor preserves variadic-ness via
        // `is_variadic` on the entity type.
        let mut params: Vec<rustc_abi_cxx::TypeId> = Vec::new();
        for child in entity.get_children() {
            if child.get_kind() != EntityKind::ParmDecl {
                continue;
            }
            let pty = match child.get_type() {
                Some(t) => t,
                None => return Ok(()),
            };
            match self.import_type(pty, &where_) {
                Ok(id) => params.push(id),
                Err(_) => return Ok(()),
            }
        }
        let variadic = entity
            .get_type()
            .map(|t| t.is_variadic())
            .unwrap_or(false);

        // Build parent path: namespace ancestors only, outer→inner.
        let mut parent_segments: Vec<NameSegment> = Vec::new();
        let mut cur = entity.get_semantic_parent();
        while let Some(e) = cur {
            match e.get_kind() {
                EntityKind::Namespace => {
                    let pname = e.get_name().unwrap_or_default();
                    if pname.is_empty() {
                        parent_segments.push(NameSegment::AnonymousNamespace);
                    } else {
                        parent_segments
                            .push(NameSegment::Namespace(Ident(pname)));
                    }
                }
                _ => break,
            }
            cur = e.get_semantic_parent();
        }
        parent_segments.reverse();

        // v1.12.2: free-fn annotation capture happens via
        // `walk_for_annotations` (which now recognizes
        // `EntityKind::FunctionDecl`) — the per-importer
        // `self.annotations` HashMap is currently discarded by
        // the surrounding `import_header_full` path, so we don't
        // duplicate the work here. Tracking unification of the
        // two annotation paths as a v1.12.3 cleanup.

        self.free_fns.push(FreeFnDef {
            parent: parent_segments,
            name: Ident(name),
            sig: FnSig {
                params,
                ret,
                cv: CvQual::default(),
                ref_q: None,
                variadic,
                noexcept: false,
            },
        });
        Ok(())
    }

    /// M11.c: harvest one class-scope `static` data member.
    /// `class_name` is the owning class's `NestedName` so the
    /// emitter can group members under their class without
    /// re-walking semantic parents. Failures (unsupported member
    /// type, anonymous member) silently skip.
    fn collect_static_data_member(
        &mut self,
        entity: &Entity<'_>,
        class_name: &NestedName,
    ) -> Result<(), ImportError> {
        let name = match entity.get_name() {
            Some(n) if !n.is_empty() => n,
            _ => return Ok(()),
        };
        if let Some(usr) = entity.get_usr() {
            if !self.static_data_usrs.insert(usr.0) {
                return Ok(());
            }
        }
        let where_ = format!("static data member `{name}`");
        let raw_ty = match entity.get_type() {
            Some(t) => t,
            None => return Ok(()),
        };
        // Capture top-level cv-qualifiers before canonicalizing
        // (the canonical type strips typedef sugar but keeps
        // `const` / `volatile`).
        let cv = cv_from_type(raw_ty);
        let ty = match self.import_type(raw_ty.get_canonical_type(), &where_) {
            Ok(id) => id,
            Err(_) => return Ok(()),
        };
        self.static_data.push(StaticDataDef {
            parent: class_name.0.clone(),
            name: Ident(name),
            ty,
            cv,
        });
        Ok(())
    }

    fn import_class(
        &mut self,
        entity: &Entity<'_>,
    ) -> Result<ClassId, ImportError> {
        let usr = entity_usr(entity);

        // M13: USR-cache lookup with upgrade-on-later-definition.
        //
        // Three states for a cached entry:
        //   1. Not cached            → fall through to fresh import.
        //   2. Cached + healthy      → reuse the existing id.
        //   3. Cached + poisoned     → the previous import saw only
        //                              a forward decl. If THIS call
        //                              hands us a definition, we
        //                              upgrade the placeholder in
        //                              place and unpoison; otherwise
        //                              return the existing poison id.
        let upgrade_target = match self.classes.get(&usr).copied() {
            Some(id) if self.ctx.is_poisoned(id) => Some(id),
            Some(id) => {
                return Ok(id);
            }
            None => None,
        };

        // For template specializations reached via `Type::get_declaration()`,
        // the cursor handed to us can be a forward-decl whose
        // `get_children()` is empty — the full body lives on a sibling
        // `ClassTemplateSpecializationDecl` elsewhere in the TU. Resolve
        // to that definition up front so children iteration yields the
        // instantiated fields and methods.
        let entity = entity.get_definition().unwrap_or(*entity);
        let entity = &entity;

        if !entity.is_definition() {
            // Forward-only declaration. Mint a poison node and
            // carry on — downstream consumers see an opaque type
            // with a doc-comment reason rather than a hard error
            // that aborts the whole import. (M9: error recovery
            // via poison nodes per `docs/cxx_importer.md §11`.)
            //
            // If we already had a poisoned entry for this USR, just
            // return it — re-poisoning would erase any annotation
            // / span info the first poison call captured.
            if let Some(target) = upgrade_target {
                return Ok(target);
            }
            let name = entity.get_name().unwrap_or_default();
            return Ok(self.poison_class(
                entity,
                NestedName(vec![NameSegment::Class(Ident(name.clone()))]),
                format!(
                    "forward-declared class without definition: `{name}`. \
                     Provide the full declaration in a header passed to \
                     `Driver::parse_all` or in a sidecar `instantiate(...)` directive.",
                ),
            ));
        }

        let kind = match entity.get_kind() {
            EntityKind::StructDecl => RecordKind::Struct,
            EntityKind::ClassDecl => RecordKind::Class,
            EntityKind::UnionDecl => RecordKind::Union,
            _ => RecordKind::Class,
        };

        // Register a placeholder ClassDef with just the name, so any
        // self-referential type encountered while processing the body
        // (e.g. `const Bar&` parameter inside Bar's own method) can be
        // resolved to this same `ClassId` via the USR cache.
        //
        // Upgrade path (M13): if `upgrade_target` is set, we already
        // have a (poisoned) ClassId for this USR. Reuse it instead
        // of minting a new one — references from earlier imports
        // are still pointing at it. Overwrite the placeholder
        // ClassDef with the freshly-derived `kind` + correct
        // `name_path`; the body walk below will populate fields /
        // bases / methods.
        let name_path = self.build_nested_path(entity)?;
        let placeholder = ClassDef {
            name: NestedName(name_path),
            bases: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            kind,
            is_polymorphic: false,
            is_final: false,
            source_alignment: None,
        };
        let id = if let Some(target) = upgrade_target {
            *self.ctx.class_mut(target) = placeholder;
            target
        } else {
            let new_id = self.ctx.define_class(placeholder);
            self.classes.insert(usr, new_id);
            new_id
        };

        // Capture inline annotations (`[[clang::annotate("rustcc::…")]]`)
        // for this class. Keyed by the class's FQN so the bindings
        // emitter can look them up by `NestedName::display`.
        let class_anns = read_annotations(entity);
        if !class_anns.is_empty() {
            self.annotations.insert(entity_fqn(entity), class_anns);
        }

        // **Known v1 gap for template specializations.** libclang's
        // cursor traversal (`visit_children`) does not surface the
        // instantiated methods of a `ClassTemplateSpecializationDecl`:
        // the template's own `CXXMethodDecl` entities report their
        // semantic parent as the underlying `ClassTemplate`, not the
        // specialization, so neither the direct child walk nor a
        // recursive TU visit + USR-lookup produces them. Fields still
        // come through via `Type::get_fields()` (with per-field
        // `get_canonical_type()` to resolve `T` → `int`). Template
        // methods therefore have to be constructed on the caller side
        // when mangling them is needed.

        let name = entity.get_name().unwrap_or_default();
        let mut bases = Vec::new();
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        let mut pending_static_marks: Vec<usize> = Vec::new();
        // M18: per-method (method_idx, default_count) pairs for
        // recording on the ctx after the class body is assigned.
        // Same deferral pattern as `pending_static_marks`.
        let mut pending_default_arg_marks: Vec<(usize, usize)> = Vec::new();

        // Fields: prefer `Type::get_fields()` over `entity.get_children()`.
        // The former iterates through libclang's type-visitor which
        // returns instantiated fields even on template specializations,
        // whereas `get_children()` on a spec cursor sometimes comes back
        // empty.
        //
        // M21.b: bitfields now flow through the layout engine
        // via the `ctx.bitfield_widths` sidecar. The importer
        // captures `child.get_bit_field_width()` per bitfield
        // member; the layout engine reads it back to apply
        // Itanium bit-packing rules. Bitfield-bearing classes
        // are no longer poisoned wholesale — they emit through
        // the regular class path with their fields packed
        // correctly. Per-field bitfield accessors on the
        // generated Rust binding are tracked as M21.c.
        let mut pending_bitfield_marks: Vec<(usize, u64)> = Vec::new();
        if let Some(field_entities) =
            entity.get_type().and_then(|t| t.get_fields())
        {
            for child in field_entities {
                let fname = child.get_name().unwrap_or_default();
                let is_bitfield = child.is_bit_field();
                let bitfield_width =
                    child.get_bit_field_width().map(|w| w as u64);
                // On a template specialization, `child.get_type()` may
                // report the template's parameter type (e.g. `T`,
                // surfaced as `TypeKind::Unexposed`). Canonicalize the
                // type to resolve the parameter back to its concrete
                // instantiation (`int`).
                let fty = child.get_type().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "field without type",
                        where_: format!("{name}::{fname}"),
                        span: None,
                    }
                })?;
                let fty = fty.get_canonical_type();
                let ty_id =
                    self.import_type(fty, &format!("{name}::{fname}"))?;
                let field_idx = fields.len();
                fields.push(FieldDef {
                    name: Ident(fname),
                    ty: ty_id,
                    explicit_align: None,
                });
                if is_bitfield {
                    pending_bitfield_marks
                        .push((field_idx, bitfield_width.unwrap_or(0)));
                }
            }
        }
        // Apply pending bitfield marks against the ctx now that
        // the class has its final field indices. Done after the
        // field-walk loop to avoid re-borrow issues.
        for (field_idx, width) in &pending_bitfield_marks {
            self.ctx
                .record_bitfield_width(id, *field_idx, *width);
        }

        // Bases and methods: walk the child-entity list. For template
        // specs, methods may not be reachable this way — the
        // `attach_outclass_methods` post-pass in `import_header` catches
        // them by scanning TU-level method definitions.
        for child in entity.get_children() {
            match child.get_kind() {
                EntityKind::BaseSpecifier => {
                    bases.push(self.lower_base(&child, &name)?);
                }
                EntityKind::Method
                | EntityKind::Constructor
                | EntityKind::Destructor
                | EntityKind::ConversionFunction => {
                    // Skip non-public methods. C++ access control
                    // means a protected/private method can't be
                    // called from a free C trampoline anyway —
                    // including them in the binding would emit
                    // shims that fail to compile (`'foo' is a
                    // protected member of 'Bar'`). libclang
                    // reports access via `get_accessibility()`;
                    // ctors / dtors / conversions are always
                    // public-or-default.
                    if matches!(
                        child.get_accessibility(),
                        Some(clang::Accessibility::Protected)
                            | Some(clang::Accessibility::Private),
                    ) {
                        continue;
                    }
                    // M22: skip methods whose `lower_method` fails
                    // (e.g. unsupported parameter type kinds reachable
                    // only via this method's signature). Mirrors the
                    // `attach_methods_recursively` post-pass policy.
                    // Without this, a single broken method would
                    // bubble `?` out of `import_class` while the
                    // placeholder ClassDef registered in the cache
                    // earlier stayed in place, leaving the class
                    // permanently half-imported across the rest of
                    // the TU.
                    let m = match self.lower_method(&child, &name, id) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    // M11: capture static-method markers. libclang
                    // exposes `is_static_method()` only on `Method`
                    // entities (ctors / dtors / conversions can't
                    // be static in C++). The bindings emitter
                    // reads `ctx.is_method_static` to route static
                    // methods through the receiver-less wrapper
                    // path.
                    let is_static = matches!(child.get_kind(), EntityKind::Method)
                        && child.is_static_method();
                    // M18: capture before `lower_method` is called
                    // again on the next sibling (which would clobber
                    // the scratch slot).
                    let default_count = self.last_method_default_count;
                    let method_idx = methods.len();
                    methods.push(m);
                    if is_static {
                        // Defer the actual `mark_method_static`
                        // call until after `class.methods` is
                        // assigned at the end of `import_class`.
                        // Indices captured now are stable because
                        // we only push in this loop.
                        pending_static_marks.push(method_idx);
                    }
                    if default_count > 0 {
                        pending_default_arg_marks
                            .push((method_idx, default_count));
                    }
                }
                // M11.c: class-scope static data members
                // (`static int counter;` inside a class body).
                // libclang surfaces these as `VarDecl` cursors
                // with `StorageClass::Static`. Non-static fields
                // arrive as `FieldDecl` and are handled by the
                // earlier `Type::get_fields()` walk; non-static
                // VarDecls are extremely rare at class scope.
                EntityKind::VarDecl => {
                    let is_static = matches!(
                        child.get_storage_class(),
                        Some(clang::StorageClass::Static),
                    );
                    if is_static {
                        let class_name_path = NestedName(
                            self.build_nested_path(entity).unwrap_or_default(),
                        );
                        let _ = self.collect_static_data_member(
                            &child,
                            &class_name_path,
                        );
                    }
                }
                // M16.b: class-scope enums (`struct Outer { enum
                // class E { … }; };`). `collect_enum`'s
                // parent-path walk now includes `Class` segments,
                // so the captured `CxxEnumDef.parent` carries the
                // full `[…, Class("Outer")]` prefix. The emitter
                // flattens it to `Outer_E` at module root.
                EntityKind::EnumDecl => {
                    let _ = self.collect_enum(&child);
                }
                // M17.b: class-scope `using` / `typedef`. Same
                // shape as M16.b — `collect_alias`'s walk now
                // includes class segments, and the emitter
                // flattens to `Outer_It` at module root.
                EntityKind::TypedefDecl | EntityKind::TypeAliasDecl => {
                    let _ = self.collect_alias(&child);
                }
                _ => {
                    // FieldDecl is already handled above via
                    // `Type::get_fields()`. Nested types, templates,
                    // etc. are out of v1 scope.
                }
            }
        }

        // M24: if this class is a *class-template specialization*
        // (e.g. `Box<int>`), libclang's child walk on the spec
        // cursor returns no methods — the methods only live on the
        // underlying generic `ClassTemplate` cursor. Chain back to
        // the template, build a substitution map (T → int, ...)
        // from the spec's record-type template argument types,
        // then walk the template's children for methods.
        //
        // Each method's signature on the template references the
        // unsubstituted parameters (e.g. `T get() const` returns a
        // `TypeKind::Unexposed` whose declaration is the
        // `TemplateTypeParameter T`). `import_type`'s top-level
        // substitution check resolves those references against
        // `current_template_subst` before any other dispatch.
        //
        // Limitations (tracked as M24 follow-up):
        // - Only top-level `T` references are substituted — nested
        //   forms like `T*`, `Box<T>`, `pair<T, U>` fall back to
        //   the generic dispatch, which still sees `T` as
        //   `Unexposed` with no substitution path. Methods that
        //   contain such forms get silently skipped via the
        //   per-method `lower_method` continue policy from M22.
        // - Member templates (a `template<typename U>` method
        //   inside `Box<T>`) are not handled.
        if methods.is_empty() {
            if let Some(template_entity) = entity.get_template() {
                let spec_args = entity
                    .get_type()
                    .and_then(|t| t.get_template_argument_types())
                    .unwrap_or_default();
                // Build a name → TypeId map by pairing the
                // template's TemplateTypeParameter children with the
                // spec's argument types in declaration order.
                let mut subst: HashMap<String, TypeId> = HashMap::new();
                let template_params: Vec<Entity<'_>> = template_entity
                    .get_children()
                    .into_iter()
                    .filter(|c| {
                        c.get_kind() == EntityKind::TemplateTypeParameter
                    })
                    .collect();
                for (i, param) in template_params.iter().enumerate() {
                    let pname = match param.get_name() {
                        Some(n) => n,
                        None => continue,
                    };
                    let arg = match spec_args.get(i).and_then(|t| t.as_ref()) {
                        Some(t) => *t,
                        None => continue,
                    };
                    let arg_id = match self.import_type(
                        arg,
                        &format!("{name}::<template arg {i}>"),
                    ) {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    subst.insert(pname, arg_id);
                }
                // Stash + restore. Outer `import_class` calls (e.g.
                // recursive ones triggered via `import_type` below)
                // would otherwise inherit our substitution map and
                // mis-substitute their own parameters.
                let prev_subst = std::mem::replace(
                    &mut self.current_template_subst,
                    subst,
                );
                for child in template_entity.get_children() {
                    match child.get_kind() {
                        EntityKind::Method
                        | EntityKind::Constructor
                        | EntityKind::Destructor
                        | EntityKind::ConversionFunction => {
                            if matches!(
                                child.get_accessibility(),
                                Some(clang::Accessibility::Protected)
                                    | Some(clang::Accessibility::Private),
                            ) {
                                continue;
                            }
                            let m = match self
                                .lower_method(&child, &name, id)
                            {
                                Ok(m) => m,
                                Err(_) => continue,
                            };
                            let is_static = matches!(
                                child.get_kind(),
                                EntityKind::Method
                            ) && child.is_static_method();
                            let default_count =
                                self.last_method_default_count;
                            let method_idx = methods.len();
                            methods.push(m);
                            if is_static {
                                pending_static_marks.push(method_idx);
                            }
                            if default_count > 0 {
                                pending_default_arg_marks
                                    .push((method_idx, default_count));
                            }
                        }
                        _ => {}
                    }
                }
                self.current_template_subst = prev_subst;
            }
        }

        let self_has_virtual = methods
            .iter()
            .any(|m| m.virtuality != Virtuality::NonVirtual);
        let base_polymorphic = bases
            .iter()
            .any(|b| self.ctx.class(b.class).is_polymorphic);
        // A class with any virtual base (direct or transitive) is
        // polymorphic even without declaring virtual methods, because
        // it needs a vptr for virtual-base-offset lookup.
        let has_vbase = bases.iter().any(|b| b.virtual_)
            || bases.iter().any(|b| {
                class_has_virtual_base_chain(self.ctx, b.class)
            });
        let is_polymorphic =
            self_has_virtual || base_polymorphic || has_vbase;

        let class = self.ctx.class_mut(id);
        class.bases = bases;
        class.fields = fields;
        class.methods = methods;
        class.is_polymorphic = is_polymorphic;

        // After everything is in place, walk the class's primary
        // vtable and stamp `vtable_index` onto each virtual method
        // we own — useful metadata for downstream emitters that
        // want to dispatch through the vtable rather than calling
        // the mangled symbol directly.
        if is_polymorphic {
            populate_vtable_indices(self.ctx, id);
        }

        // M11: apply deferred static-method marks. The indices
        // captured during the child walk match the final
        // positions in `class.methods` because we only push
        // (never insert mid-vec) in that loop.
        for idx in &pending_static_marks {
            self.ctx.mark_method_static(id, *idx);
        }

        // M18: apply deferred default-arg counts.
        for (idx, count) in &pending_default_arg_marks {
            self.ctx.record_default_arg_count(id, *idx, *count);
        }

        // M13: if this import call upgraded a previously-poisoned
        // entry (forward-only → full definition), clear the poison
        // marker now that the class has real fields / methods /
        // bases. Downstream emitters render it as a concrete type
        // instead of an opaque struct.
        if upgrade_target.is_some() {
            self.ctx.unpoison(id);
        }
        Ok(id)
    }

    fn lower_method(
        &mut self,
        entity: &Entity<'_>,
        parent_name: &str,
        enclosing_class: ClassId,
    ) -> Result<MethodDef, ImportError> {
        // Reset the M18 scratch slot — every `lower_method` call
        // sets it as a side effect, but we want a clean baseline
        // so an early-error path doesn't carry over the previous
        // method's count.
        self.last_method_default_count = 0;
        let name = entity.get_name().unwrap_or_default();
        let kind = entity.get_kind();
        let ctx_where = format!("{parent_name}::{name}");

        // Capture inline annotations before lowering so the
        // bindings emitter can override Rust names per-method.
        // FQN matches what the bindings emitter constructs from
        // `<class FQN>::<method-source-name>`. The walk uses the
        // method entity's semantic-parent chain to recover the
        // class FQN — we ignore `parent_name` here because that
        // string is the class's *short* name, not the FQN.
        let method_anns = read_annotations(entity);
        if !method_anns.is_empty() {
            self.annotations.insert(entity_fqn(entity), method_anns);
        }

        // Parameters come from ParmDecl children.
        // M18: a ParmDecl with non-empty children carries a
        // default-argument expression as one of those children
        // (e.g. `IntegerLiteral`, `CXXBoolLiteralExpr`,
        // `GNUNullExpr`, …). We don't resolve the value here —
        // that requires constant-evaluation plumbing — but we
        // record per-parameter "has-default" so the emitter can
        // surface a doc comment listing optional trailing args.
        let mut params = Vec::new();
        let mut has_default: Vec<bool> = Vec::new();
        for child in entity.get_children() {
            if child.get_kind() == EntityKind::ParmDecl {
                let pty = child.get_type().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "param without type",
                        where_: ctx_where.clone(),
                    span: None,

                    }
                })?;
                params.push(self.import_type(pty, &ctx_where)?);
                has_default.push(!child.get_children().is_empty());
            }
        }
        // C++ defaults must occupy a contiguous tail
        // (`f(int a, int b = 1, int c)` is illegal), so a simple
        // suffix count is correct.
        let trailing_defaults = has_default
            .iter()
            .rev()
            .take_while(|&&b| b)
            .count();
        self.last_method_default_count = trailing_defaults;
        let _ = enclosing_class;

        // Ctors and dtors have no source-level return type; use `void`
        // as a placeholder so downstream consumers that read `sig.ret`
        // see a well-formed TypeId.
        let ret = match kind {
            EntityKind::Constructor | EntityKind::Destructor => {
                self.ctx.intern_type(CxxType::Void)
            }
            _ => {
                let r = entity.get_result_type().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "method without return type",
                        where_: ctx_where.clone(),
                    span: None,
                
                    }
                })?;
                self.import_type(r, &ctx_where)?
            }
        };

        let cv = CvQual {
            is_const: entity.is_const_method(),
            is_volatile: false,
        };

        // Ref-qualifier (`Foo::bar() &` vs `&&`) and variadic-ness
        // are properties of the *type* of the method, not the
        // method entity itself. `entity.get_type()` returns the
        // FunctionPrototype Type for a method, which carries both.
        let method_type = entity.get_type();
        let ref_q = method_type
            .as_ref()
            .and_then(|t| t.get_ref_qualifier())
            .map(|r| match r {
                RefQualifier::LValue => RefKind::Lvalue,
                RefQualifier::RValue => RefKind::Rvalue,
            });
        let variadic = method_type
            .as_ref()
            .map(|t| t.is_variadic())
            .unwrap_or(false);

        // `noexcept` extraction. C++17 made `noexcept` part of the
        // function type; the Itanium mangler doesn't fold it into
        // ordinary method symbols, but it's load-bearing for
        // pointer-to-member types and template signatures, plus it's
        // useful surface info for downstream emitters (e.g. shim
        // generation can drop the `try` wrapper for noexcept fns).
        //
        // Only `BasicNoexcept` and `ComputedNoexcept` map to
        // `noexcept = true`. `DynamicNone` (`throw()`) was C++03
        // syntax that doesn't participate in the type system, and
        // `NoThrow` (`__declspec(nothrow)`) is an MSVC annotation
        // that doesn't affect Itanium semantics.
        let noexcept = matches!(
            entity.get_exception_specification(),
            Some(ExceptionSpecification::BasicNoexcept)
                | Some(ExceptionSpecification::ComputedNoexcept),
        );

        let virtuality = if entity.is_pure_virtual_method() {
            Virtuality::PureVirtual
        } else if entity.is_virtual_method() {
            Virtuality::Virtual
        } else {
            Virtuality::NonVirtual
        };

        // Recognize operator overloads so the mangler emits the short
        // two-letter codes (`pl`, `ix`, ...) instead of spelling
        // `operator+` as a regular source-name.
        let parsed_op = parse_operator(&name, params.len());

        // Special-member classification:
        //   Destructor             → Dtor
        //   Constructor()          → DefaultCtor
        //   Constructor(const X&)  → CopyCtor
        //   Constructor(X&&)       → MoveCtor
        //   Constructor(other)     → OtherCtor  (still user-declared;
        //                            removes trivial default ctor)
        //   operator=(const X&)    → CopyAssign
        //   operator=(X&&)         → MoveAssign
        //   everything else        → None   (including conversion ops,
        //                                   which don't affect POD)
        let special = match kind {
            EntityKind::Destructor => Some(SpecialMember::Dtor),
            EntityKind::Constructor => {
                Some(classify_ctor(&params, enclosing_class, self.ctx))
            }
            EntityKind::Method
                if matches!(parsed_op, Some(OperatorKind::Assign)) =>
            {
                classify_assignment(&params, enclosing_class, self.ctx)
            }
            _ => None,
        };

        let method_name = match kind {
            EntityKind::ConversionFunction => {
                // For `operator <T>() [const]`, the source-level return
                // type IS the conversion target. Use it as the
                // `ConversionTo` payload; the mangler emits `cv<T>` in
                // the method-name position.
                MethodName::ConversionTo(ret)
            }
            _ => match parsed_op {
                Some(op) => MethodName::Operator(op),
                None => MethodName::Ident(Ident(name)),
            },
        };

        Ok(MethodDef {
            name: method_name,
            sig: FnSig {
                params,
                ret,
                cv,
                ref_q,
                variadic,
                noexcept,
            },
            virtuality,
            vtable_index: None,
            special,
        })
    }

    fn lower_base(
        &mut self,
        base_spec: &Entity<'_>,
        parent_name: &str,
    ) -> Result<BaseSpec, ImportError> {
        let virtual_ = base_spec.is_virtual_base();
        // A BaseSpecifier entity's type refers to the base class; its
        // declaration is the class entity we need.
        let base_type = base_spec.get_type().ok_or_else(|| {
            ImportError::UnsupportedFeature {
                what: "base specifier without type",
                where_: parent_name.to_string(),
                    span: None,
                
            }
        })?;
        let base_decl = base_type.get_declaration().ok_or_else(|| {
            ImportError::UnsupportedFeature {
                what: "base specifier resolves to a type without a declaration",
                where_: parent_name.to_string(),
                    span: None,
                
            }
        })?;
        let class_id = self.import_class(&base_decl)?;
        let access = match base_spec.get_accessibility() {
            Some(clang::Accessibility::Public) => Access::Public,
            Some(clang::Accessibility::Protected) => Access::Protected,
            Some(clang::Accessibility::Private) => Access::Private,
            None => Access::Public,
        };
        Ok(BaseSpec {
            class: class_id,
            virtual_,
            access,
        })
    }

    /// Build the full nested-name path for a class or enum entity by
    /// climbing semantic parents. Class-template specializations
    /// (e.g., `Box<int>`) produce a `NameSegment::TemplateSpec` whose
    /// args are themselves recursively imported types.
    fn build_nested_path(
        &mut self,
        entity: &Entity<'_>,
    ) -> Result<Vec<NameSegment>, ImportError> {
        let mut segments = Vec::new();
        let mut cur = Some(*entity);
        while let Some(e) = cur {
            match e.get_kind() {
                EntityKind::Namespace => {
                    let name = e.get_name().unwrap_or_default();
                    if name.is_empty() {
                        segments.push(NameSegment::AnonymousNamespace);
                    } else {
                        segments.push(NameSegment::Namespace(Ident(name)));
                    }
                }
                EntityKind::StructDecl
                | EntityKind::ClassDecl
                | EntityKind::UnionDecl
                | EntityKind::ClassTemplatePartialSpecialization => {
                    let name = e.get_name().unwrap_or_default();
                    // A record decl is a template specialization iff its
                    // type exposes template arguments. Wrap in
                    // TemplateSpec so the mangler emits `<name>I<args>E`.
                    let is_spec = e
                        .get_type()
                        .and_then(|t| t.get_template_argument_types())
                        .is_some();
                    if is_spec {
                        let template_args =
                            self.lower_template_args(&e, &name)?;
                        segments.push(NameSegment::TemplateSpec {
                            name: Ident(name),
                            args: template_args,
                        });
                    } else {
                        segments.push(NameSegment::Class(Ident(name)));
                    }
                }
                EntityKind::EnumDecl => {
                    let name = e.get_name().unwrap_or_default();
                    segments.push(NameSegment::Enum(Ident(name)));
                }
                // TranslationUnit or anything else — we're done walking.
                _ => break,
            }
            cur = e.get_semantic_parent();
        }
        segments.reverse();
        Ok(segments)
    }

    /// Lower the template arguments of a class-template specialization
    /// cursor into IR `TemplateArg`s.
    ///
    /// Prefers libclang's cursor-based `get_template_arguments()` view,
    /// which surfaces non-type (integral) arguments. Type-only arguments
    /// fall back to `get_template_argument_types()` when the cursor API
    /// doesn't classify the decl as a specialization.
    fn lower_template_args(
        &mut self,
        spec: &Entity<'_>,
        parent_name: &str,
    ) -> Result<Vec<TemplateArg>, ImportError> {
        if let Some(args) = spec.get_template_arguments() {
            // Recover each parameter's declared type from the primary
            // template, positionally — integral NTTPs need it to pick the
            // Itanium type letter (`Li…E`, `Lm…E`, `Lb…E`, …).
            let param_types = template_param_types(spec);
            let mut out = Vec::with_capacity(args.len());
            for (i, arg) in args.iter().enumerate() {
                match arg {
                    TemplateArgument::Type(t) => {
                        out.push(TemplateArg::Type(
                            self.import_type(*t, parent_name)?,
                        ));
                    }
                    TemplateArgument::Integral(signed, unsigned) => {
                        let pty = param_types.get(i).copied().flatten();
                        let (ty, value) = self.lower_nttp_integral(
                            pty, *signed, *unsigned, parent_name,
                        )?;
                        out.push(TemplateArg::Integral { value, ty });
                    }
                    // Template-template, declaration (pointer/reference/
                    // member-pointer), nullptr, parameter-pack, and
                    // unresolved-expression arguments are not recoverable
                    // through libclang's type-only argument view (the
                    // `clang` crate's `Template` variant carries no name).
                    // Reject rather than silently mis-mangle.
                    other => {
                        return Err(ImportError::UnsupportedFeature {
                            what: unsupported_template_arg_kind(other),
                            where_: parent_name.to_string(),
                            span: None,
                        });
                    }
                }
            }
            return Ok(out);
        }

        // Type-only fallback: the cursor API didn't expose arguments, but
        // the type does. A `None` entry here is a non-type argument the
        // type view can't represent — reject to avoid mis-mangling.
        let arg_tys = spec
            .get_type()
            .and_then(|t| t.get_template_argument_types())
            .unwrap_or_default();
        let mut out = Vec::with_capacity(arg_tys.len());
        for arg in &arg_tys {
            let ty = arg.ok_or_else(|| ImportError::UnsupportedFeature {
                what: "non-type template argument",
                where_: parent_name.to_string(),
                span: None,
            })?;
            out.push(TemplateArg::Type(self.import_type(ty, parent_name)?));
        }
        Ok(out)
    }

    /// Lower an integral non-type template argument: import its declared
    /// type (defaulting to `int` when the parameter type is unavailable)
    /// and select the signed or unsigned value interpretation based on
    /// that type's signedness.
    fn lower_nttp_integral(
        &mut self,
        param_ty: Option<Type<'_>>,
        signed: i64,
        unsigned: u64,
        parent_name: &str,
    ) -> Result<(TypeId, i128), ImportError> {
        let ty = match param_ty {
            Some(t) => self.import_type(t, parent_name)?,
            None => self.ctx.intern_type(CxxType::Int {
                signed: true,
                width: IntWidth::I32,
            }),
        };
        let is_unsigned =
            matches!(self.ctx.type_of(ty), CxxType::Int { signed: false, .. });
        let value: i128 =
            if is_unsigned { unsigned as i128 } else { signed as i128 };
        Ok((ty, value))
    }

    /// M15: lower a `FunctionPrototype` / `FunctionNoPrototype`
    /// libclang `Type` to a `CxxType::Fn(FnSig)`. Used for both
    /// pointer-to-function (`void (*)(int)` collapses one layer
    /// of `Ptr` and lands here) and bare function-typed alias
    /// targets (`using F = void(int);`).
    ///
    /// Returns the `CxxType` (not yet interned) so the caller can
    /// integrate it with surrounding pointer logic. v0 captures
    /// param + result types and a best-effort `variadic` flag;
    /// `noexcept` / `cv` / `ref_q` aren't part of a function
    /// pointer's syntactic surface and stay at their defaults.
    fn import_function_proto(
        &mut self,
        ty: Type<'_>,
        where_: &str,
    ) -> Result<CxxType, ImportError> {
        let ret_ty = ty.get_result_type().ok_or_else(|| {
            ImportError::UnsupportedFeature {
                what: "function type without result type",
                where_: where_.to_string(),
                span: None,
            }
        })?;
        let ret = self.import_type(ret_ty, where_)?;
        let mut params: Vec<TypeId> = Vec::new();
        if let Some(arg_tys) = ty.get_argument_types() {
            for at in arg_tys {
                let id = self.import_type(at, where_)?;
                params.push(id);
            }
        }
        let variadic = ty.is_variadic();
        Ok(CxxType::Fn(FnSig {
            params,
            ret,
            cv: CvQual::default(),
            ref_q: None,
            variadic,
            // Function-pointer types don't carry a syntactic
            // `noexcept` qualifier in pre-C++17 source. Even in
            // C++17+, libclang exposes the noexcept-ness on the
            // declaration, not the standalone type. Default to
            // `false` (potentially-throwing); the bindings
            // emitter renders `extern "C" fn(...)` either way.
            noexcept: false,
        }))
    }

    fn import_type(
        &mut self,
        ty: Type<'_>,
        where_: &str,
    ) -> Result<TypeId, ImportError> {
        // M24: when walking the underlying template's methods of a
        // class-template specialization, type references to template
        // parameters (`T`, `U`, ...) come through as
        // `TypeKind::Unexposed` whose declaration is a
        // `TemplateTypeParameter` cursor. Substitute them with the
        // spec's argument types via the active substitution map
        // before any other dispatch — this handles `T get()` and
        // `void set(T)` directly. Pointer/reference forms (`T*`,
        // `const T&`, `T&&`) resolve too: libclang reports them as
        // structured `Pointer`/`LValueReference` types whose pointee is
        // the `Unexposed` `T`, so the regular dispatch recurses here and
        // substitutes the leaf (the leading `const`/`volatile` words in
        // the pointee display are stripped before lookup). The remaining
        // gap is a *nested template specialization* parameterised on `T`
        // (e.g. a method returning `Box<T>` / `vector<T>`): that arrives
        // as an opaque `Unexposed` with display `Box<T>` and no
        // structure to recurse into, so it can't be resolved without
        // instantiating the nested spec — such methods are skipped.
        if !self.current_template_subst.is_empty()
            && ty.get_kind() == TypeKind::Unexposed
        {
            // libclang surfaces template-parameter types in
            // un-instantiated method signatures with an empty
            // declaration cursor. The only stable handle is the
            // type's display name, which matches the parameter's
            // identifier (e.g. `T`, `U`).
            //
            // CV qualifiers come through as a `"const T"` /
            // `"volatile T"` / `"const volatile T"` display.
            // Strip the leading qualifier words before lookup so
            // pointee-of-pointer-to-const cases (`const T*`)
            // substitute correctly. The CV bits themselves are
            // preserved by the parent type's `cv_from_type(pointee)`
            // call against the ORIGINAL pointee.
            let display = ty.get_display_name();
            let stripped = display
                .trim_start_matches("const ")
                .trim_start_matches("volatile ")
                .trim_start_matches("const ");
            if let Some(&substituted) =
                self.current_template_subst.get(stripped)
            {
                return Ok(substituted);
            }
        }
        // Strip elaborated-type-specifier sugar (`struct Inner`) and
        // typedefs so the match below sees the canonical kind.
        let ty = match ty.get_kind() {
            TypeKind::Elaborated | TypeKind::Typedef => ty.get_canonical_type(),
            _ => ty,
        };
        let cxx = match ty.get_kind() {
            TypeKind::Void => CxxType::Void,
            TypeKind::Bool => CxxType::Bool,
            TypeKind::CharS | TypeKind::SChar => int(true, IntWidth::I8),
            TypeKind::CharU | TypeKind::UChar => int(false, IntWidth::I8),
            TypeKind::Short => int(true, IntWidth::I16),
            TypeKind::UShort => int(false, IntWidth::I16),
            TypeKind::Int => int(true, IntWidth::I32),
            TypeKind::UInt => int(false, IntWidth::I32),
            TypeKind::Long | TypeKind::LongLong => int(true, IntWidth::I64),
            TypeKind::ULong | TypeKind::ULongLong => int(false, IntWidth::I64),
            TypeKind::Float => CxxType::Float {
                kind: FloatKind::F32,
            },
            TypeKind::Double => CxxType::Float {
                kind: FloatKind::F64,
            },
            TypeKind::Pointer => {
                let pointee = ty.get_pointee_type().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "pointer with no pointee",
                        where_: where_.to_string(),
                    span: None,

                    }
                })?;
                // M15: collapse pointer-to-function-type to a bare
                // `CxxType::Fn` rather than `Ptr { pointee: Fn }`.
                // Itanium and Rust both treat function pointers as
                // a single ABI unit, so the extra `Ptr` indirection
                // would lead the renderer to emit `*const fn(...)`
                // — wrong for callbacks. Keep one level of pointer
                // indirection (`void (*)(int)`) but drop it for
                // higher levels (`void (**)(int)` keeps the outer
                // Ptr around the Fn).
                if matches!(
                    pointee.get_kind(),
                    TypeKind::FunctionPrototype | TypeKind::FunctionNoPrototype,
                ) {
                    self.import_function_proto(pointee, where_)?
                } else {
                    let id = self.import_type(pointee, where_)?;
                    CxxType::Ptr {
                        pointee: id,
                        cv: cv_from_type(pointee),
                    }
                }
            }
            // Bare function-prototype type — usually only seen on
            // alias targets (`using SignalHandler = void(int);`).
            // C++ implicitly converts function-typed lvalues to
            // function pointers at use sites, so the alias is most
            // useful when treated as the equivalent function-pointer
            // type from the Rust side.
            TypeKind::FunctionPrototype | TypeKind::FunctionNoPrototype => {
                self.import_function_proto(ty, where_)?
            }
            TypeKind::LValueReference => {
                let pointee = ty.get_pointee_type().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "reference with no pointee",
                        where_: where_.to_string(),
                    span: None,
                
                    }
                })?;
                let id = self.import_type(pointee, where_)?;
                CxxType::Ref {
                    pointee: id,
                    kind: RefKind::Lvalue,
                    cv: cv_from_type(pointee),
                }
            }
            TypeKind::RValueReference => {
                let pointee = ty.get_pointee_type().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "rvalue ref with no pointee",
                        where_: where_.to_string(),
                    span: None,
                
                    }
                })?;
                let id = self.import_type(pointee, where_)?;
                CxxType::Ref {
                    pointee: id,
                    kind: RefKind::Rvalue,
                    cv: cv_from_type(pointee),
                }
            }
            TypeKind::Record => {
                let decl = ty.get_declaration().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "record type without declaration",
                        where_: where_.to_string(),
                    span: None,
                
                    }
                })?;
                let class_id = self.import_class(&decl)?;
                CxxType::Record(class_id)
            }
            TypeKind::Enum => {
                let decl = ty.get_declaration().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "enum type without declaration",
                        where_: where_.to_string(),
                    span: None,
                
                    }
                })?;
                let name = NestedName(self.build_nested_path(&decl)?);
                let underlying = decl
                    .get_enum_underlying_type()
                    .ok_or_else(|| {
                        ImportError::UnsupportedFeature {
                            what: "enum without underlying type",
                            where_: where_.to_string(),
                    span: None,
                
                        }
                    })?;
                let underlying_id = self.import_type(underlying, where_)?;
                // `enum class`/`enum struct` are scoped; plain `enum` is
                // unscoped. The flag doesn't affect mangling (both forms
                // use source-name mangling) but we record it for future
                // codegen consumers.
                // `Entity::is_scoped()` isn't stable in the `clang`
                // crate's 2.x API; we don't currently need the scoped
                // flag for mangling (both forms mangle identically) so
                // fall back to `false`. Revisit when codegen needs it.
                let scoped = false;
                CxxType::Enum {
                    name,
                    underlying: underlying_id,
                    scoped,
                }
            }
            TypeKind::ConstantArray => {
                let elem = ty.get_element_type().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "array without element type",
                        where_: where_.to_string(),
                    span: None,

                    }
                })?;
                let len = ty.get_size().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "array with unknown size",
                        where_: where_.to_string(),
                    span: None,

                    }
                })?;
                let elem_id = self.import_type(elem, where_)?;
                CxxType::Array {
                    elem: elem_id,
                    len: len as u64,
                }
            }
            // C array-to-pointer decay. `T arr[]` in a function
            // parameter list (or any "incomplete array" position
            // libclang preserves) lowers to `*T` for ABI purposes.
            // Without this arm, e.g. FLTK's
            // `static void default_icons(const Fl_Image *icons[], int)`
            // — where libclang reports the param type as
            // IncompleteArray-of-pointer rather than pointer-to-pointer
            // — bubbles an "unsupported clang type kind" error out of
            // the offending method's `lower_method`, which (before
            // M22) poisoned the *entire* enclosing class body via
            // the placeholder cache.
            TypeKind::IncompleteArray => {
                let elem = ty.get_element_type().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "incomplete array without element type",
                        where_: where_.to_string(),
                        span: None,
                    }
                })?;
                let elem_id = self.import_type(elem, where_)?;
                CxxType::Ptr {
                    pointee: elem_id,
                    cv: cv_from_type(elem),
                }
            }
            other => {
                return Err(ImportError::UnsupportedFeature {
                    what: "unsupported clang type kind",
                    where_: format!("{where_}: {other:?}"),
                    span: None,
                });
            }
        };
        Ok(self.ctx.intern_type(cxx))
    }
}


/// Classify a constructor by its parameter list.
fn classify_ctor(
    params: &[TypeId],
    enclosing: ClassId,
    ctx: &CxxTypeCtx,
) -> SpecialMember {
    if params.is_empty() {
        return SpecialMember::DefaultCtor;
    }
    if params.len() == 1 {
        if let Some(ref_kind) = self_ref_kind(params[0], enclosing, ctx) {
            return match ref_kind {
                RefKind::Lvalue => SpecialMember::CopyCtor,
                RefKind::Rvalue => SpecialMember::MoveCtor,
            };
        }
    }
    SpecialMember::OtherCtor
}

/// Classify an `operator=` member function. Only single-parameter forms
/// taking a reference to the enclosing class count as copy/move-assign;
/// anything else is a regular method that happens to be named
/// `operator=` (e.g. `X& operator=(int)`) and returns `None`.
fn classify_assignment(
    params: &[TypeId],
    enclosing: ClassId,
    ctx: &CxxTypeCtx,
) -> Option<SpecialMember> {
    if params.len() == 1 {
        match self_ref_kind(params[0], enclosing, ctx)? {
            RefKind::Lvalue => Some(SpecialMember::CopyAssign),
            RefKind::Rvalue => Some(SpecialMember::MoveAssign),
        }
    } else {
        None
    }
}

/// If `ty` is a reference (lvalue or rvalue) to the enclosing class,
/// return its `RefKind`. Ignores CV qualifiers on the pointee — a
/// `const X&` and a plain `X&` both count as copy-ctor/copy-assign
/// candidates per the C++ rules.
fn self_ref_kind(
    ty: TypeId,
    enclosing: ClassId,
    ctx: &CxxTypeCtx,
) -> Option<RefKind> {
    match ctx.type_of(ty) {
        CxxType::Ref { pointee, kind, .. } => match ctx.type_of(*pointee) {
            CxxType::Record(c) if *c == enclosing => Some(*kind),
            _ => None,
        },
        _ => None,
    }
}

/// Recognize C++ operator method names as emitted by Clang (e.g.
/// `"operator+"`, `"operator[]"`). Where an operator has both unary and
/// binary member-function forms, `arity` disambiguates: a member binary
/// operator takes one parameter (the RHS; LHS is `this`), while a
/// member unary operator takes zero. Unary forms we don't yet model
/// return `None` so the method falls back to an identifier name rather
/// than mis-mangle.
fn parse_operator(name: &str, arity: usize) -> Option<OperatorKind> {
    match (name, arity) {
        ("operator+", 1) => Some(OperatorKind::Plus),
        ("operator-", 1) => Some(OperatorKind::Minus),
        ("operator*", 1) => Some(OperatorKind::Mul),
        ("operator/", 1) => Some(OperatorKind::Div),
        ("operator%", 1) => Some(OperatorKind::Mod),
        ("operator=", _) => Some(OperatorKind::Assign),
        ("operator+=", _) => Some(OperatorKind::PlusAssign),
        ("operator==", _) => Some(OperatorKind::Eq),
        ("operator!=", _) => Some(OperatorKind::Ne),
        ("operator<", _) => Some(OperatorKind::Lt),
        ("operator<=", _) => Some(OperatorKind::Le),
        ("operator>", _) => Some(OperatorKind::Gt),
        ("operator>=", _) => Some(OperatorKind::Ge),
        ("operator()", _) => Some(OperatorKind::Call),
        ("operator[]", _) => Some(OperatorKind::Index),
        ("operator++", _) => Some(OperatorKind::PreIncr),
        ("operator--", _) => Some(OperatorKind::PreDecr),
        // `operator*`/`operator&` as unary deref/addressof and all other
        // unmapped operators fall through to `None`.
        _ => None,
    }
}

fn class_has_virtual_base_chain(
    ctx: &CxxTypeCtx,
    class_id: ClassId,
) -> bool {
    let class = ctx.class(class_id);
    for base in &class.bases {
        if base.virtual_ {
            return true;
        }
        if class_has_virtual_base_chain(ctx, base.class) {
            return true;
        }
    }
    false
}

/// Build a `::`-joined FQN by walking semantic parents up to the
/// translation-unit boundary. Used as the lookup key for inline
/// annotations stored on the [`Importer`]. Mirrors what the
/// [`AnnotationSet`] consumer in `rust_bindings` constructs from
/// `NestedName` after lowering — same string, different source.
///
/// libclang reports the TU root cursor with kind `NotImplemented`
/// rather than the (non-existent in this enum) `TranslationUnit`,
/// and its `name` is the file path. We stop the walk on either
/// shape, plus on a missing semantic parent, so file paths never
/// appear in the FQN.
/// Capture the C++ source location of `entity` as a [`SourceSpan`].
/// Returns `None` for synthetic / built-in cursors that don't have
/// an owning file (those usually come from libclang's stdlib
/// stubs and aren't useful in user-facing diagnostics anyway).
/// True for libclang-synthesized names that aren't real C++
/// identifiers — e.g. `(unnamed enum at /opt/.../foo.h:42:1)`,
/// `(anonymous union at ...)`. Some libclang builds return these
/// in `get_name()` instead of an empty string for tag-less
/// declarations; we filter them so they don't leak into the
/// generated Rust source as invalid identifiers.
/// Walk `entity`'s semantic-parent chain and return the full
/// nested-name path — namespaces *and* class scopes — in
/// outer-to-inner order. Used by M16/M17 (enums + aliases) so
/// class-scope items land with their owning class encoded in
/// `parent`. The emitter tells namespace-scope from class-scope
/// by inspecting `parent` and emits class-scope items at module
/// root with a `<Outer>_<Inner>` joined name (Rust doesn't
/// allow `pub enum` / `pub type` inside `impl` blocks, so the
/// bindgen-style flattening is the only viable shape on stable
/// rustc).
///
/// Stops at the first ancestor that isn't a namespace, class,
/// struct, or union — so the TU root and any leftover synthetic
/// kinds don't leak into the path.
fn build_full_parent_path(entity: &Entity<'_>) -> Vec<NameSegment> {
    let mut segments: Vec<NameSegment> = Vec::new();
    let mut cur = entity.get_semantic_parent();
    while let Some(e) = cur {
        match e.get_kind() {
            EntityKind::Namespace => {
                let pname = e.get_name().unwrap_or_default();
                if pname.is_empty() {
                    segments.push(NameSegment::AnonymousNamespace);
                } else {
                    segments.push(NameSegment::Namespace(Ident(pname)));
                }
            }
            EntityKind::StructDecl
            | EntityKind::ClassDecl
            | EntityKind::UnionDecl => {
                let cname = e.get_name().unwrap_or_default();
                if !cname.is_empty() && !is_synthetic_anonymous_name(&cname) {
                    segments.push(NameSegment::Class(Ident(cname)));
                }
            }
            _ => break,
        }
        cur = e.get_semantic_parent();
    }
    segments.reverse();
    segments
}

fn is_synthetic_anonymous_name(name: &str) -> bool {
    name.starts_with('(')
        || name.contains(" enum at ")
        || name.contains(" union at ")
        || name.contains(" struct at ")
        || name.contains(" class at ")
}

pub(crate) fn span_of_entity(entity: &Entity<'_>) -> Option<SourceSpan> {
    let loc = entity.get_location()?;
    let file_loc = loc.get_file_location();
    let path = file_loc.file?.get_path();
    Some(SourceSpan::new(path, file_loc.line, file_loc.column))
}

fn entity_fqn(entity: &Entity<'_>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = Some(*entity);
    while let Some(e) = cur {
        if matches!(
            e.get_kind(),
            EntityKind::TranslationUnit | EntityKind::NotImplemented,
        ) {
            break;
        }
        if let Some(name) = e.get_name() {
            if !name.is_empty() {
                parts.push(name);
            }
        }
        cur = e.get_semantic_parent();
    }
    parts.reverse();
    parts.join("::")
}

/// Read inline `[[clang::annotate("rustcc::…")]]` annotations from
/// `entity`'s direct children. Recognizes the v0 set:
///
/// - `rustcc::name=NewName`     → [`Annotation::Name`]
/// - `rustcc::nullable`         → [`Annotation::Nullable`]
/// - `rustcc::nonnull`          → [`Annotation::NonNull`]
/// - `rustcc::skip`             → [`Annotation::Skip`]
///
/// Annotations whose payload doesn't match one of these patterns
/// are silently ignored — a stricter v0.X release can promote
/// unknowns to a diagnostic.
fn read_annotations(entity: &Entity<'_>) -> Vec<Annotation> {
    let mut out = Vec::new();
    for child in entity.get_children() {
        if child.get_kind() != EntityKind::AnnotateAttr {
            continue;
        }
        // Both `get_display_name()` and `get_name()` work for
        // `AnnotateAttr` cursors on modern libclang (>= 11). We try
        // the more reliable display-name route first.
        let raw = child
            .get_display_name()
            .or_else(|| child.get_name())
            .unwrap_or_default();
        if let Some(ann) = parse_rustcc_annotation(&raw) {
            out.push(ann);
        }
    }
    out
}

fn parse_rustcc_annotation(text: &str) -> Option<Annotation> {
    let s = text.trim();
    let rest = s.strip_prefix("rustcc::")?;
    if let Some(value) = rest.strip_prefix("name=") {
        return Some(Annotation::Name(value.trim().to_string()));
    }
    // v1.12.9: typed-throws form `cxx_throws(T1, T2, …)`. Parse
    // the parenthesized list into `Vec<String>` and produce
    // `Annotation::CxxThrowsTyped`. We split on top-level commas
    // (respecting nested generics) so a single type like
    // `std::pair<int, double>` doesn't get sliced. Empty list
    // `cxx_throws()` collapses to the bare `cxx_throws` form for
    // forgiveness.
    if let Some(args) = rest.strip_prefix("cxx_throws(") {
        if let Some(inner) = args.strip_suffix(')') {
            let types = split_top_level_args(inner);
            if types.is_empty() {
                return Some(Annotation::CxxThrows);
            }
            return Some(Annotation::CxxThrowsTyped(types));
        }
        // Mis-formatted (`cxx_throws(` with no closing paren).
        // Silently ignore — matches the v0 unknown-annotation
        // policy.
        return None;
    }
    match rest {
        "nullable" => Some(Annotation::Nullable),
        "nonnull" => Some(Annotation::NonNull),
        "skip" => Some(Annotation::Skip),
        "cxx_throws" => Some(Annotation::CxxThrows),
        _ => None,
    }
}

/// Split a comma-separated argument list, respecting nested
/// `<…>` so generic types like `std::pair<int, double>` count
/// as a single argument. Trims whitespace around each entry
/// and drops empty entries (so `cxx_throws()` or
/// `cxx_throws(, )` returns an empty vec).
fn split_top_level_args(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            b',' if depth == 0 => {
                let part = s[start..i].trim();
                if !part.is_empty() {
                    out.push(part.to_string());
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

/// Recompute `is_polymorphic` for a class from its current state.
/// Mirrors the inline computation in `import_class`'s body walk.
/// Used by the M22 third pass to converge polymorphism flags across
/// inheritance edges that may have been computed against placeholder
/// base classes during recursive `import_class` calls.
fn recompute_is_polymorphic(ctx: &CxxTypeCtx, class_id: ClassId) -> bool {
    let class = ctx.class(class_id);
    let self_has_virtual = class
        .methods
        .iter()
        .any(|m| m.virtuality != Virtuality::NonVirtual);
    let base_polymorphic = class
        .bases
        .iter()
        .any(|b| ctx.class(b.class).is_polymorphic);
    let has_vbase = class.bases.iter().any(|b| b.virtual_)
        || class
            .bases
            .iter()
            .any(|b| class_has_virtual_base_chain(ctx, b.class));
    self_has_virtual || base_polymorphic || has_vbase
}

/// Walk a polymorphic class's primary vtable and stamp `vtable_index`
/// onto each virtual method the class owns or overrides.
///
/// The fork-side vtable layout (`CxxTypeCtx::vtable`) computes one
/// `VTableEntry::FunctionPointer` slot per dispatchable method, in
/// the canonical Itanium order: base virtuals first (slots that
/// either keep base targets or get rewritten with our overriders),
/// then any new virtuals introduced by this class.
///
/// Per-method `vtable_index` is the *function-pointer rank* in the
/// primary sub-table — the count of `FunctionPointer` slots that
/// precede this one, ignoring the virtual-base offsets, offset-to-
/// top, and RTTI slots that lead each table.
///
/// We only update methods that match by Itanium-mangled symbol with
/// what's actually in the slot. This handles two cases cleanly:
///
/// - A virtual we override: our class's own mangled symbol is in
///   the slot, so we own the index.
/// - A virtual a base owns and we don't override: the slot's
///   target is the base's symbol, so the lookup misses and our
///   `vtable_index` stays `None` (we never see the inherited
///   methods on this class anyway since the importer copies
///   methods from `child.get_children()`, not from base classes).
///
/// Pure virtuals are skipped in v0 — their slot target is
/// `__cxa_pure_virtual` (a single shared symbol), so the symbol-
/// match approach can't disambiguate them. A future revision can
/// reach pure virtuals via the `MethodId` field once the importer
/// stops eagerly cloning method vectors.
fn populate_vtable_indices(ctx: &mut CxxTypeCtx, class_id: ClassId) {
    let Some(vtable) = ctx.vtable(class_id) else {
        return;
    };
    let Some(primary) = vtable.sub_tables.first() else {
        return;
    };

    // Walk the primary sub-table and record the function-pointer
    // rank for every method whose `MethodId` shows up in a
    // `FunctionPointer` slot.
    //
    // M23: this match-by-MethodId path covers pure-virtual
    // methods correctly. The vtable builder routes pure
    // virtuals to `__cxa_pure_virtual` for the slot's
    // `mangled_target`, but the `method: MethodId` field on
    // the slot still points back at the originating method,
    // so the walker can assign it the correct vtable_index
    // without mangled-symbol matching. The downstream
    // bindings emitter then routes pure-virtual calls
    // through the regular vtable-lookup path; if the runtime
    // object is the actually-abstract base, the lookup hits
    // `__cxa_pure_virtual` and terminates (matching C++
    // semantics). If a derived override is in scope, that
    // override fires.
    let mut updates: Vec<(usize, u32)> = Vec::new();
    let mut fp_rank: u32 = 0;
    for entry in &primary.entries {
        if let VTableEntry::FunctionPointer { method, .. } = entry {
            updates.push((method.as_index(), fp_rank));
            fp_rank += 1;
        }
    }

    let class_mut = ctx.class_mut(class_id);
    for (m_idx, vt) in updates {
        if m_idx < class_mut.methods.len() {
            class_mut.methods[m_idx].vtable_index = Some(vt);
        }
    }
}

fn entity_usr(entity: &Entity<'_>) -> String {
    entity
        .get_usr()
        .map(|u| u.0)
        .unwrap_or_else(|| entity.get_name().unwrap_or_default())
}

fn cv_from_type(ty: Type<'_>) -> CvQual {
    CvQual {
        is_const: ty.is_const_qualified(),
        is_volatile: ty.is_volatile_qualified(),
    }
}

fn int(signed: bool, width: IntWidth) -> CxxType {
    CxxType::Int { signed, width }
}

/// Collect the declared types of a template's parameters, in declaration
/// order, so a specialization's positional arguments can recover each
/// parameter's type. Type and template-template parameters contribute a
/// `None` (they have no value type); non-type parameters contribute their
/// declared type (e.g. `int`, `size_t`, `bool`).
fn template_param_types<'tu>(spec: &Entity<'tu>) -> Vec<Option<Type<'tu>>> {
    let Some(tmpl) = spec.get_template() else {
        return Vec::new();
    };
    tmpl.get_children()
        .into_iter()
        .filter(|c| {
            matches!(
                c.get_kind(),
                EntityKind::TemplateTypeParameter
                    | EntityKind::NonTypeTemplateParameter
                    | EntityKind::TemplateTemplateParameter
            )
        })
        .map(|c| c.get_type())
        .collect()
}

/// A stable `&'static str` describing a template-argument kind that the
/// importer cannot lower (used in `UnsupportedFeature` diagnostics).
fn unsupported_template_arg_kind(arg: &TemplateArgument<'_>) -> &'static str {
    match arg {
        TemplateArgument::Template | TemplateArgument::TemplateExpansion => {
            "template-template argument"
        }
        TemplateArgument::Declaration => {
            "pointer/reference/member-pointer non-type template argument"
        }
        TemplateArgument::Nullptr => "null-pointer non-type template argument",
        TemplateArgument::Pack => "template parameter pack",
        TemplateArgument::Expression => {
            "unresolved-expression template argument"
        }
        TemplateArgument::Null => "null template argument",
        // Type and Integral are lowered, never routed here.
        TemplateArgument::Type(_) | TemplateArgument::Integral(..) => {
            "template argument"
        }
    }
}

// -------- v1.11 stretch 3: STL container auto-discovery (M24 follow-up) --

/// Parse `source` headers via libclang and discover every class
/// template specialization referenced in field / parameter /
/// return-type positions. Returns the deduplicated set of
/// canonical instantiation strings (e.g. `"std::vector<int>"`,
/// `"std::optional<MyClass>"`) suitable for inserting into a
/// [`crate::driver::HeaderGraph::template_instantiations`] list.
///
/// Why this exists: libclang only surfaces methods for a
/// `ClassTemplateSpecialization` when the spec has been *forced*
/// to instantiate (via `template class T<int>;` in a synthetic
/// root). For specs that only appear by reference (e.g. a header
/// declares `void foo(std::vector<int>);` but never explicitly
/// instantiates `vector<int>`), the methods on `vector<int>`
/// don't make it into the imported `ClassDef`. Pre-scanning the
/// AST for such references lets the driver auto-populate the
/// `template_instantiations` list and re-parse with the
/// synthesized force-instantiations, yielding full method
/// coverage for STL containers and similar template-heavy types.
///
/// **Scope (v1)**: top-level spec references in fields /
/// parameters / returns. The walker recurses into pointer /
/// reference / array element types to find inner spec
/// references (e.g. `std::vector<int>*` finds `std::vector<int>`).
/// Nested specs (`std::vector<std::optional<int>>`) ARE captured
/// — both the outer and inner specs land in the result set.
///
/// **Limits**: 
/// - Iterator types (`std::vector<int>::iterator`) are NOT
///   auto-discovered — they're nested types inside a spec and
///   need their own walker pass to lift them. Track as a v2
///   follow-up.
/// - Allocator-defaulted specs (`std::vector<int>` vs
///   `std::vector<int, std::allocator<int>>`) are captured by
///   the canonical-display-name they happen to surface; libclang
///   typically reports the short form. The forced-instantiation
///   synth then re-parses libstdc++'s template with the default
///   allocator, which is the desired behavior.
#[cfg(feature = "libclang")]
pub fn discover_template_instantiations(
    clang: &Clang,
    source: &Path,
    args: &[&str],
) -> Result<std::collections::HashSet<String>, ImportError> {
    let index = Index::new(clang, false, false);
    let tu = index
        .parser(source)
        .arguments(args)
        .parse()
        .map_err(|e| ImportError::ClangDiagnostic {
            file: source.display().to_string(),
            line: 0,
            message: format!("discovery parse failed: {e:?}"),
        })?;

    let mut found: std::collections::HashSet<String> = std::collections::HashSet::new();
    walk_entity_for_specs(&tu.get_entity(), &mut found);
    Ok(found)
}

#[cfg(feature = "libclang")]
fn walk_entity_for_specs(
    entity: &Entity<'_>,
    found: &mut std::collections::HashSet<String>,
) {
    // For this entity itself: examine its type (if any) and the
    // type-result for functions / methods. Then recurse into
    // children.
    if let Some(ty) = entity.get_type() {
        collect_specs_in_type(ty, found);
    }
    if let Some(rt) = entity.get_result_type() {
        collect_specs_in_type(rt, found);
    }
    for child in entity.get_children() {
        walk_entity_for_specs(&child, found);
    }
}

#[cfg(feature = "libclang")]
fn collect_specs_in_type(
    ty: Type<'_>,
    found: &mut std::collections::HashSet<String>,
) {
    // Unwrap pointer / reference / array layers to find the
    // pointee. `vector<int>*` has TypeKind::Pointer with a
    // pointee TypeKind::Record (or Elaborated) that's the spec.
    let mut cur = ty;
    let mut depth = 0;
    while depth < 16 {
        depth += 1;
        match cur.get_kind() {
            TypeKind::Pointer | TypeKind::LValueReference | TypeKind::RValueReference => {
                if let Some(p) = cur.get_pointee_type() {
                    cur = p;
                    continue;
                }
            }
            TypeKind::ConstantArray
            | TypeKind::IncompleteArray
            | TypeKind::VariableArray => {
                if let Some(elem) = cur.get_element_type() {
                    cur = elem;
                    continue;
                }
            }
            TypeKind::Elaborated | TypeKind::Typedef => {
                cur = cur.get_canonical_type();
                continue;
            }
            _ => {}
        }
        break;
    }

    // Now check if `cur` is a class template specialization. The
    // signal libclang gives us: `get_template_argument_types()`
    // returns Some(_) with non-empty inner vector.
    let targs = cur.get_template_argument_types();
    let is_spec = matches!(targs, Some(ref v) if !v.is_empty());
    if !is_spec {
        return;
    }

    // Capture the canonical instantiation string. The
    // display-name format is exactly what the synth-root
    // `template class <name>;` line wants — `"std::vector<int>"`.
    let display = cur.get_display_name();
    if display.is_empty() {
        return;
    }
    // Filter heuristic: only capture types that look like
    // "namespace::Name<args>" — exclude bare template names
    // and anything containing local-only types we can't
    // re-instantiate.
    if !display.contains('<') || !display.contains('>') {
        return;
    }
    // Recurse into the template arguments to capture nested
    // specs (e.g. `vector<optional<int>>` should add both
    // `vector<optional<int>>` and `optional<int>`).
    if let Some(arg_tys) = targs {
        for arg in arg_tys.iter().flatten() {
            collect_specs_in_type(*arg, found);
        }
    }
    found.insert(display);
}

/// v1.12.8: synthesize known "companion" template instantiations
/// for STL containers + smart pointers that are typically
/// composed inside other templates (and therefore never appear
/// directly in user code, so `discover_template_instantiations`
/// doesn't pick them up).
///
/// For every entry in `discovered`, the function inspects the
/// template head + first type argument and adds any companion
/// specs the container is known to internally instantiate:
///
/// - `std::vector<T, …>` → `std::allocator<T>`
/// - `std::deque<T, …>` → `std::allocator<T>`
/// - `std::list<T, …>` → `std::allocator<T>`
/// - `std::map<K, V, …>` → `std::allocator<std::pair<const K, V>>`,
///   `std::pair<const K, V>`
/// - `std::unordered_map<K, V, …>` → same allocator + pair spec
/// - `std::set<T, …>` → `std::allocator<T>`
/// - `std::unique_ptr<T, …>` → `std::default_delete<T>`
/// - `std::shared_ptr<T>` → `std::__shared_ptr<T>` (libstdc++ /
///   libc++ both expose this as a base class)
/// - `std::function<R(Args…)>` → companion deleter / control-block
///   specs are libstdc++-internal and skipped for now (tracked
///   for a v1.12.8.1 follow-up)
///
/// The function is pure — it inspects only the input set and
/// returns the additional names to merge in. Companions that
/// match the input format ("`std::allocator<int>`" with the
/// `std::` namespace prefix) — caller can drop the prefix if
/// the discovery walker happens to surface allocator-free names.
///
/// **Iterator typedefs** (`std::vector<int>::iterator`,
/// `::const_iterator`) are NOT auto-synthesized here. The
/// underlying iterator types are libstdc++-vs-libc++-specific
/// (`__gnu_cxx::__normal_iterator<int*, std::vector<int>>` vs
/// `std::__1::__wrap_iter<int*>`); users who need iterator
/// methods exposed should list the implementation-specific
/// type in the sidecar's `template_instantiations:` block.
/// Iterator type-name synthesis is tracked as v1.12.8.1.
pub fn synthesize_stl_companions(
    discovered: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    let mut out: std::collections::HashSet<String> = std::collections::HashSet::new();

    for spec in discovered {
        // Parse `<template-head>` + first type arg out of the
        // canonical-name form. For `"std::vector<int, std::allocator<int>>"`
        // the head is `"std::vector"` and the first arg is `"int"`.
        let Some((head, args)) = split_template_head(spec) else {
            continue;
        };
        let arg_list = split_template_args(args);
        let Some(first_arg) = arg_list.first().map(|s| s.trim()) else {
            continue;
        };

        match head.trim() {
            // Sequence containers — allocator per element type.
            "std::vector" | "std::deque" | "std::list" | "std::forward_list" => {
                out.insert(format!("std::allocator<{first_arg}>"));
            }
            // Sets — allocator per element type.
            "std::set" | "std::multiset" | "std::unordered_set" | "std::unordered_multiset" => {
                out.insert(format!("std::allocator<{first_arg}>"));
            }
            // Maps — pair<const K, V> + its allocator.
            "std::map" | "std::multimap" | "std::unordered_map" | "std::unordered_multimap" => {
                let Some(second_arg) = arg_list.get(1).map(|s| s.trim()) else {
                    continue;
                };
                let pair_ty = format!("std::pair<const {first_arg}, {second_arg}>");
                out.insert(format!("std::allocator<{pair_ty}>"));
                out.insert(pair_ty);
            }
            "std::unique_ptr" => {
                out.insert(format!("std::default_delete<{first_arg}>"));
            }
            "std::shared_ptr" | "std::weak_ptr" => {
                // libstdc++ + libc++ both expose `__shared_ptr<T>`
                // as the base; force-instantiating it picks up
                // the control-block accessors users sometimes
                // need.
                out.insert(format!("std::__shared_ptr<{first_arg}>"));
            }
            _ => {}
        }
    }

    // Don't return entries that were already in the input — the
    // caller merges, so we only ship NEW synthesized specs.
    out.retain(|s| !discovered.contains(s));
    out
}

/// Split a template spec into `(head, args)` where `head` is
/// the part before the first `<` and `args` is the contents
/// between the outermost `<…>` pair. Returns `None` if the
/// input doesn't have a top-level template-argument list.
fn split_template_head(spec: &str) -> Option<(&str, &str)> {
    let lt = spec.find('<')?;
    // Find the matching `>` accounting for nested generics.
    let bytes = spec.as_bytes();
    let mut depth = 0i32;
    let mut end = None;
    for (i, &b) in bytes.iter().enumerate().skip(lt) {
        match b {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    Some((&spec[..lt], &spec[lt + 1..end]))
}

/// Split a template argument list into individual argument
/// strings, respecting nested `<…>` so a single argument like
/// `std::pair<const int, double>` doesn't get sliced on its
/// internal comma.
fn split_template_args(args: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let bytes = args.as_bytes();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&args[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < args.len() {
        out.push(&args[start..]);
    }
    out
}

#[cfg(test)]
mod stl_companion_tests {
    use super::*;
    use std::collections::HashSet;

    fn set<I: IntoIterator<Item = &'static str>>(items: I) -> HashSet<String> {
        items.into_iter().map(String::from).collect()
    }

    #[test]
    fn vector_int_synthesizes_allocator_int() {
        let input = set(["std::vector<int>"]);
        let companions = synthesize_stl_companions(&input);
        assert!(
            companions.contains("std::allocator<int>"),
            "expected std::allocator<int> companion; got {companions:?}"
        );
    }

    #[test]
    fn vector_with_explicit_allocator_still_emits_companion_if_distinct() {
        // libclang sometimes reports the long form
        // `std::vector<int, std::allocator<int>>` directly — in
        // that case our synth still wants to emit
        // `std::allocator<int>` (the underlying spec) so the
        // importer can attach methods.
        let input = set(["std::vector<int, std::allocator<int>>"]);
        let companions = synthesize_stl_companions(&input);
        assert!(
            companions.contains("std::allocator<int>"),
            "expected std::allocator<int> synthesized from the explicit form; got {companions:?}"
        );
    }

    #[test]
    fn map_synthesizes_pair_and_pair_allocator() {
        let input = set(["std::map<int, double>"]);
        let companions = synthesize_stl_companions(&input);
        assert!(
            companions.contains("std::pair<const int, double>"),
            "expected pair companion; got {companions:?}"
        );
        assert!(
            companions.contains("std::allocator<std::pair<const int, double>>"),
            "expected pair-allocator companion; got {companions:?}"
        );
    }

    #[test]
    fn unique_ptr_synthesizes_default_delete() {
        let input = set(["std::unique_ptr<MyType>"]);
        let companions = synthesize_stl_companions(&input);
        assert!(
            companions.contains("std::default_delete<MyType>"),
            "expected default_delete companion; got {companions:?}"
        );
    }

    #[test]
    fn shared_ptr_synthesizes_shared_ptr_base() {
        let input = set(["std::shared_ptr<MyType>"]);
        let companions = synthesize_stl_companions(&input);
        assert!(
            companions.contains("std::__shared_ptr<MyType>"),
            "expected __shared_ptr base companion; got {companions:?}"
        );
    }

    #[test]
    fn non_stl_specs_get_no_companions() {
        let input = set(["MyTemplate<int>", "Vec<double>"]);
        let companions = synthesize_stl_companions(&input);
        assert!(
            companions.is_empty(),
            "expected no companions for user templates; got {companions:?}"
        );
    }

    #[test]
    fn companions_already_in_input_are_not_duplicated() {
        let input = set(["std::vector<int>", "std::allocator<int>"]);
        let companions = synthesize_stl_companions(&input);
        assert!(
            !companions.contains("std::allocator<int>"),
            "expected dedup against input; got {companions:?}"
        );
    }

    #[test]
    fn split_template_args_respects_nested_generics() {
        let args = "int, std::pair<int, double>, std::allocator<int>";
        let parts = split_template_args(args);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].trim(), "int");
        assert_eq!(parts[1].trim(), "std::pair<int, double>");
        assert_eq!(parts[2].trim(), "std::allocator<int>");
    }

    #[test]
    fn split_template_head_returns_none_for_non_template_input() {
        assert!(split_template_head("plain_type").is_none());
    }
}

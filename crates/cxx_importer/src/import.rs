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
//! Self-doc gaps (still open, smaller now):
//!
//! - `noexcept`, ref-qualifiers (`&` / `&&`), and variadics in
//!   `FnSig` are extracted as defaults today (`false`, `None`,
//!   `false`). Scheduled in the polish pass that lands alongside
//!   this revision of the docs.
//! - Vtable indices are computed at layout time inside
//!   `rustc_abi_cxx::vtable`, not propagated back into per-method
//!   `MethodDef::vtable_index`. The mangler / dispatcher reads the
//!   index out of the layout query, so this is a metadata gap, not
//!   a correctness one.
//! - Uninstantiated templates (`CXCursor_ClassTemplate`) are
//!   skipped. Sidecar-driven explicit instantiation lands later.
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
    RefQualifier, Type, TypeKind,
};
use rustc_abi_cxx::{
    Access, BaseSpec, ClassDef, ClassId, CvQual, CxxType, CxxTypeCtx,
    FieldDef, FloatKind, FnSig, Ident, IntWidth, MethodDef, MethodName,
    NameSegment, NestedName, OperatorKind, RecordKind, RefKind, SpecialMember,
    TemplateArg, TypeId, Virtuality,
};

use crate::diagnostics::ImportError;

pub fn import_header(
    source: &Path,
    args: &[&str],
    ctx: &mut CxxTypeCtx,
) -> Result<Vec<ClassId>, ImportError> {
    let mut cache = HashMap::new();
    import_header_with_cache(source, args, ctx, &mut cache)
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
    let clang = Clang::new().map_err(|e| ImportError::ClangDiagnostic {
        file: source.display().to_string(),
        line: 0,
        message: format!("failed to initialize libclang: {e}"),
    })?;
    let index = Index::new(&clang, false, false);
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
    // from the TU, recursing into namespaces.
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

    // Hand the accumulated USR map back to the caller so the next
    // `import_header_with_cache` call can dedup against it.
    *cache = importer.into_cache();
    Ok(imported)
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
            importer.ctx.class_mut(class_id).methods.push(method);
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
}

impl<'a> Importer<'a> {
    fn new(ctx: &'a mut CxxTypeCtx) -> Self {
        Self::with_cache(ctx, HashMap::new())
    }

    fn with_cache(
        ctx: &'a mut CxxTypeCtx,
        classes: HashMap<String, ClassId>,
    ) -> Self {
        Self { ctx, classes }
    }

    fn into_cache(self) -> HashMap<String, ClassId> {
        self.classes
    }

    fn import_class(
        &mut self,
        entity: &Entity<'_>,
    ) -> Result<ClassId, ImportError> {
        let usr = entity_usr(entity);
        if let Some(&id) = self.classes.get(&usr) {
            return Ok(id);
        }

        // For template specializations reached via `Type::get_declaration()`,
        // the cursor handed to us can be a forward-decl whose
        // `get_children()` is empty — the full body lives on a sibling
        // `ClassTemplateSpecializationDecl` elsewhere in the TU. Resolve
        // to that definition up front so children iteration yields the
        // instantiated fields and methods.
        let entity = entity.get_definition().unwrap_or(*entity);
        let entity = &entity;

        if !entity.is_definition() {
            return Err(ImportError::UnsupportedFeature {
                what: "forward-declared class without definition",
                where_: entity.get_name().unwrap_or_default(),
            });
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
        let id = self.ctx.define_class(placeholder);
        self.classes.insert(usr, id);

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

        // Fields: prefer `Type::get_fields()` over `entity.get_children()`.
        // The former iterates through libclang's type-visitor which
        // returns instantiated fields even on template specializations,
        // whereas `get_children()` on a spec cursor sometimes comes back
        // empty.
        if let Some(field_entities) =
            entity.get_type().and_then(|t| t.get_fields())
        {
            for child in field_entities {
                let fname = child.get_name().unwrap_or_default();
                // On a template specialization, `child.get_type()` may
                // report the template's parameter type (e.g. `T`,
                // surfaced as `TypeKind::Unexposed`). Canonicalize the
                // type to resolve the parameter back to its concrete
                // instantiation (`int`).
                let fty = child.get_type().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "field without type",
                        where_: format!("{name}::{fname}"),
                    }
                })?;
                let fty = fty.get_canonical_type();
                let ty_id = self
                    .import_type(fty, &format!("{name}::{fname}"))?;
                fields.push(FieldDef {
                    name: Ident(fname),
                    ty: ty_id,
                    explicit_align: None,
                });
            }
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
                    methods.push(self.lower_method(&child, &name, id)?);
                }
                _ => {
                    // FieldDecl is already handled above via
                    // `Type::get_fields()`. Nested types, templates,
                    // etc. are out of v1 scope.
                }
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

        Ok(id)
    }

    fn lower_method(
        &mut self,
        entity: &Entity<'_>,
        parent_name: &str,
        enclosing_class: ClassId,
    ) -> Result<MethodDef, ImportError> {
        let name = entity.get_name().unwrap_or_default();
        let kind = entity.get_kind();
        let ctx_where = format!("{parent_name}::{name}");

        // Parameters come from ParmDecl children.
        let mut params = Vec::new();
        for child in entity.get_children() {
            if child.get_kind() == EntityKind::ParmDecl {
                let pty = child.get_type().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "param without type",
                        where_: ctx_where.clone(),
                    }
                })?;
                params.push(self.import_type(pty, &ctx_where)?);
            }
        }

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
            }
        })?;
        let base_decl = base_type.get_declaration().ok_or_else(|| {
            ImportError::UnsupportedFeature {
                what: "base specifier resolves to a type without a declaration",
                where_: parent_name.to_string(),
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
                    let tmpl_args = e
                        .get_type()
                        .and_then(|t| t.get_template_argument_types());
                    if let Some(arg_tys) = tmpl_args {
                        let template_args =
                            self.lower_template_args(&arg_tys, &name)?;
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

    fn lower_template_args(
        &mut self,
        args: &[Option<Type<'_>>],
        parent_name: &str,
    ) -> Result<Vec<TemplateArg>, ImportError> {
        let mut out = Vec::with_capacity(args.len());
        for arg in args {
            // `None` entries represent non-type template arguments
            // (integral values, template-templates) that the clang
            // crate's type-only view cannot represent. These are out of
            // v1 scope — reject to avoid silent mis-mangling.
            let ty = arg.ok_or_else(|| ImportError::UnsupportedFeature {
                what: "non-type template argument",
                where_: parent_name.to_string(),
            })?;
            let id = self.import_type(ty, parent_name)?;
            out.push(TemplateArg::Type(id));
        }
        Ok(out)
    }

    fn import_type(
        &mut self,
        ty: Type<'_>,
        where_: &str,
    ) -> Result<TypeId, ImportError> {
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
                    }
                })?;
                let id = self.import_type(pointee, where_)?;
                CxxType::Ptr {
                    pointee: id,
                    cv: cv_from_type(pointee),
                }
            }
            TypeKind::LValueReference => {
                let pointee = ty.get_pointee_type().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "reference with no pointee",
                        where_: where_.to_string(),
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
                    }
                })?;
                let name = NestedName(self.build_nested_path(&decl)?);
                let underlying = decl
                    .get_enum_underlying_type()
                    .ok_or_else(|| {
                        ImportError::UnsupportedFeature {
                            what: "enum without underlying type",
                            where_: where_.to_string(),
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
                    }
                })?;
                let len = ty.get_size().ok_or_else(|| {
                    ImportError::UnsupportedFeature {
                        what: "array with unknown size",
                        where_: where_.to_string(),
                    }
                })?;
                let elem_id = self.import_type(elem, where_)?;
                CxxType::Array {
                    elem: elem_id,
                    len: len as u64,
                }
            }
            other => {
                return Err(ImportError::UnsupportedFeature {
                    what: "unsupported clang type kind",
                    where_: format!("{where_}: {other:?}"),
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

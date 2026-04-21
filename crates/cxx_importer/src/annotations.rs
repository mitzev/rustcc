//! `[[rustcc::...]]` attributes and sidecar YAML.
//!
//! See `docs/cxx_importer.md §5`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::diagnostics::ImportError;

#[derive(Clone, Debug, Default)]
pub struct AnnotationSet {
    /// Inline attributes, keyed by fully-qualified entity name as
    /// `cxx_importer` resolves it during import.
    pub inline: BTreeMap<String, Vec<Annotation>>,
    pub sidecar: Option<SidecarSchema>,
}

impl AnnotationSet {
    /// Effective annotations on `entity`: inline attrs first (they
    /// win on conflict per `docs/cxx_importer.md §5.2`), then sidecar
    /// entries for anything the inline set doesn't cover.
    pub fn effective(&self, entity: &str) -> Vec<Annotation> {
        let mut out = Vec::new();
        if let Some(inl) = self.inline.get(entity) {
            out.extend(inl.iter().cloned());
        }
        if let Some(sidecar) = &self.sidecar {
            if let Some(from_sidecar) = sidecar.annotations_for(entity) {
                // Skip annotations whose `kind` already appears inline;
                // that's the "inline wins" rule.
                for ann in from_sidecar {
                    let kind = ann.kind();
                    if !out.iter().any(|a| a.kind() == kind) {
                        out.push(ann);
                    }
                }
            }
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Annotation {
    /// Override the Rust-side name used by `cxx_importer::name_mapping`.
    Name(String),
    SharedReference {
        retain: String,
        release: String,
        atomic_refcount: bool,
    },
    Unique,
    Immortal,
    Unsafe,
    Nullable,
    NonNull,
    LifetimeBound,
    Noexcept,
    InteriorMutable,
    Instantiate(String),
    /// Skip this entity entirely when lowering. Useful for silencing
    /// decls that mis-parse or can't safely be exposed to Rust.
    Skip,
}

impl Annotation {
    /// Coarse kind used by the "inline wins over sidecar" merge rule.
    /// Inline/sidecar entries of the same kind conflict; different
    /// kinds stack.
    fn kind(&self) -> AnnotationKind {
        match self {
            Annotation::Name(_) => AnnotationKind::Name,
            Annotation::SharedReference { .. } => AnnotationKind::Ownership,
            Annotation::Unique => AnnotationKind::Ownership,
            Annotation::Immortal => AnnotationKind::Ownership,
            Annotation::Unsafe => AnnotationKind::Unsafe,
            Annotation::Nullable => AnnotationKind::Nullability,
            Annotation::NonNull => AnnotationKind::Nullability,
            Annotation::LifetimeBound => AnnotationKind::LifetimeBound,
            Annotation::Noexcept => AnnotationKind::Noexcept,
            Annotation::InteriorMutable => AnnotationKind::InteriorMutable,
            Annotation::Instantiate(_) => AnnotationKind::Instantiate,
            Annotation::Skip => AnnotationKind::Skip,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum AnnotationKind {
    Name,
    Ownership,
    Unsafe,
    Nullability,
    LifetimeBound,
    Noexcept,
    InteriorMutable,
    Instantiate,
    Skip,
}

/// Parsed sidecar YAML body. Schema mirrors `docs/cxx_importer.md §5.2`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SidecarSchema {
    pub schema: u32,
    #[serde(default)]
    pub types: BTreeMap<String, TypeEntry>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeEntry {
    /// Kind classification per `docs/ownership_and_safety.md` (value,
    /// shared, unique, immortal). Missing → inferred by the importer.
    #[serde(default)]
    pub kind: Option<TypeKind>,
    /// Override the Rust-side type name.
    #[serde(default)]
    pub rust_name: Option<String>,
    /// Retain/release pair for `SharedReference` types (C++ smart
    /// pointers or intrusive-refcount types that don't model cleanly as
    /// `unique_ptr`).
    #[serde(default)]
    pub retain: Option<String>,
    #[serde(default)]
    pub release: Option<String>,
    #[serde(default)]
    pub atomic_refcount: Option<bool>,
    /// If true, skip the whole type.
    #[serde(default)]
    pub skip: Option<bool>,
    /// Per-method overrides, keyed by the C++ signature string as
    /// produced by `cxx_importer::name_mapping::disambiguate_overloads`.
    #[serde(default)]
    pub methods: BTreeMap<String, MethodEntry>,
    /// Forced template instantiations (`std::vector<int>`, etc.).
    #[serde(default)]
    pub instantiations: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TypeKind {
    Value,
    Shared,
    Unique,
    Immortal,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MethodEntry {
    #[serde(default)]
    pub rust_name: Option<String>,
    #[serde(default)]
    pub skip: Option<bool>,
    #[serde(default)]
    pub noexcept: Option<bool>,
    #[serde(default)]
    pub nullable: Option<bool>,
}

impl SidecarSchema {
    /// Return annotations that apply to `entity`, whether that's a
    /// type-level entity (`"std::vector"`) or a method-level entity
    /// (`"std::vector::push_back(T&&)"`).
    pub fn annotations_for(&self, entity: &str) -> Option<Vec<Annotation>> {
        if let Some(t) = self.types.get(entity) {
            return Some(annotations_from_type(t));
        }
        // Method lookup: split on the last `::` and look up the type.
        if let Some((type_name, method_sig)) = split_method_entity(entity) {
            if let Some(t) = self.types.get(type_name) {
                if let Some(m) = t.methods.get(method_sig) {
                    return Some(annotations_from_method(m));
                }
            }
        }
        None
    }
}

fn split_method_entity(entity: &str) -> Option<(&str, &str)> {
    // Find the last `::` that isn't inside a parameter list. Methods
    // are spelled as `Type::method(params)`; params may contain `::`
    // (e.g., `std::string`). Anchor from the opening paren.
    let paren = entity.find('(')?;
    let prefix = &entity[..paren];
    let split = prefix.rfind("::")?;
    Some((&entity[..split], &entity[split + 2..]))
}

fn annotations_from_type(entry: &TypeEntry) -> Vec<Annotation> {
    let mut out = Vec::new();
    if let Some(name) = &entry.rust_name {
        out.push(Annotation::Name(name.clone()));
    }
    match entry.kind {
        Some(TypeKind::Shared) => {
            let retain = entry.retain.clone().unwrap_or_default();
            let release = entry.release.clone().unwrap_or_default();
            out.push(Annotation::SharedReference {
                retain,
                release,
                atomic_refcount: entry.atomic_refcount.unwrap_or(false),
            });
        }
        Some(TypeKind::Unique) => out.push(Annotation::Unique),
        Some(TypeKind::Immortal) => out.push(Annotation::Immortal),
        Some(TypeKind::Value) | None => {}
    }
    if entry.skip == Some(true) {
        out.push(Annotation::Skip);
    }
    for inst in &entry.instantiations {
        out.push(Annotation::Instantiate(inst.clone()));
    }
    out
}

fn annotations_from_method(entry: &MethodEntry) -> Vec<Annotation> {
    let mut out = Vec::new();
    if let Some(name) = &entry.rust_name {
        out.push(Annotation::Name(name.clone()));
    }
    if entry.skip == Some(true) {
        out.push(Annotation::Skip);
    }
    if entry.noexcept == Some(true) {
        out.push(Annotation::Noexcept);
    }
    if entry.nullable == Some(true) {
        out.push(Annotation::Nullable);
    }
    out
}

/// Parse a sidecar file. Returns a versioned schema ready for lookup.
/// A malformed YAML or unknown schema version bubbles up as
/// `ImportError::SidecarParse` carrying the file path and a human
/// message.
pub fn load_sidecar(path: &Path) -> Result<SidecarSchema, ImportError> {
    let body = std::fs::read_to_string(path).map_err(|e| {
        ImportError::SidecarParse {
            path: path.display().to_string(),
            message: format!("io: {e}"),
        }
    })?;
    let schema: SidecarSchema = serde_yaml::from_str(&body).map_err(|e| {
        ImportError::SidecarParse {
            path: path.display().to_string(),
            message: format!("{e}"),
        }
    })?;
    if schema.schema != 1 {
        return Err(ImportError::SidecarParse {
            path: path.display().to_string(),
            message: format!(
                "unsupported sidecar schema version {} (only 1 is accepted)",
                schema.schema,
            ),
        });
    }
    Ok(schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> SidecarSchema {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.yaml");
        std::fs::write(&p, yaml).unwrap();
        load_sidecar(&p).unwrap()
    }

    #[test]
    fn parses_minimal_schema() {
        let s = parse("schema: 1\n");
        assert_eq!(s.schema, 1);
        assert!(s.types.is_empty());
    }

    #[test]
    fn parses_full_example_from_docs() {
        let s = parse(
            r#"schema: 1
types:
  "std::vector":
    kind: value
    methods:
      "push_back(T const&)":
        rust_name: push_back
      "push_back(T&&)":
        rust_name: push_back_move
"#,
        );
        assert_eq!(s.types.len(), 1);
        let v = &s.types["std::vector"];
        assert_eq!(v.kind, Some(TypeKind::Value));
        assert_eq!(
            v.methods["push_back(T const&)"].rust_name.as_deref(),
            Some("push_back")
        );
    }

    #[test]
    fn annotations_for_type_level_entity() {
        let s = parse(
            r#"schema: 1
types:
  "Foo":
    rust_name: Bar
    kind: unique
"#,
        );
        let anns = s.annotations_for("Foo").unwrap();
        // Unordered check.
        assert!(anns.contains(&Annotation::Name("Bar".into())));
        assert!(anns.contains(&Annotation::Unique));
    }

    #[test]
    fn annotations_for_method_level_entity() {
        let s = parse(
            r#"schema: 1
types:
  "std::vector":
    methods:
      "push_back(T&&)":
        rust_name: push_back_move
        noexcept: true
"#,
        );
        let anns =
            s.annotations_for("std::vector::push_back(T&&)").unwrap();
        assert!(anns.contains(&Annotation::Name("push_back_move".into())));
        assert!(anns.contains(&Annotation::Noexcept));
    }

    #[test]
    fn method_split_handles_qualified_params() {
        // `std::vector::push_back(std::string const&)` — the naive
        // "last `::`" split would cut inside the param list.
        assert_eq!(
            split_method_entity("std::vector::push_back(std::string const&)"),
            Some(("std::vector", "push_back(std::string const&)"))
        );
    }

    #[test]
    fn shared_reference_pulls_retain_release() {
        let s = parse(
            r#"schema: 1
types:
  "HandleCC":
    kind: shared
    retain: cc_retain
    release: cc_release
    atomic_refcount: true
"#,
        );
        let anns = s.annotations_for("HandleCC").unwrap();
        let shared =
            anns.iter().find_map(|a| match a {
                Annotation::SharedReference {
                    retain,
                    release,
                    atomic_refcount,
                } => Some((retain.clone(), release.clone(), *atomic_refcount)),
                _ => None,
            });
        assert_eq!(
            shared,
            Some(("cc_retain".into(), "cc_release".into(), true))
        );
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.yaml");
        std::fs::write(
            &p,
            "schema: 1\nbogus: stuff\n",
        )
        .unwrap();
        let err = load_sidecar(&p).unwrap_err();
        match err {
            ImportError::SidecarParse { .. } => {}
            other => panic!("expected Sidecar error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_schema_version_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.yaml");
        std::fs::write(&p, "schema: 99\n").unwrap();
        let err = load_sidecar(&p).unwrap_err();
        match err {
            ImportError::SidecarParse { message, .. } => {
                assert!(message.contains("unsupported sidecar schema version"));
            }
            other => panic!("expected Sidecar error, got {other:?}"),
        }
    }

    #[test]
    fn inline_wins_over_sidecar_on_same_kind() {
        let sidecar = parse(
            r#"schema: 1
types:
  "Foo":
    rust_name: FromSidecar
"#,
        );
        let mut inline = BTreeMap::new();
        inline.insert(
            "Foo".into(),
            vec![Annotation::Name("FromInline".into())],
        );
        let set = AnnotationSet {
            inline,
            sidecar: Some(sidecar),
        };
        let eff = set.effective("Foo");
        let name = eff.iter().find_map(|a| match a {
            Annotation::Name(n) => Some(n.as_str()),
            _ => None,
        });
        assert_eq!(name, Some("FromInline"));
    }

    #[test]
    fn sidecar_fills_in_missing_kinds() {
        let sidecar = parse(
            r#"schema: 1
types:
  "Foo":
    rust_name: FromSidecar
    kind: unique
"#,
        );
        let mut inline = BTreeMap::new();
        inline.insert(
            "Foo".into(),
            vec![Annotation::Name("FromInline".into())],
        );
        let set = AnnotationSet {
            inline,
            sidecar: Some(sidecar),
        };
        let eff = set.effective("Foo");
        // Name came from inline; Unique came from sidecar.
        let kinds: Vec<_> = eff.iter().map(|a| a.kind()).collect();
        assert!(kinds.contains(&AnnotationKind::Name));
        assert!(kinds.contains(&AnnotationKind::Ownership));
    }
}

use cxx_importer::{load_sidecar, Driver, HeaderGraph, SidecarSchema};

#[test]
fn driver_constructs() {
    let g = HeaderGraph::default();
    let d = Driver::new(g);
    assert!(d.graph().roots.is_empty());
}

// ============================================================
// M25: HeaderGraph::extend_from_sidecar plumbing.
// ============================================================

#[test]
fn m25_extend_from_sidecar_appends_template_instantiations() {
    let yaml = r#"schema: 1
types:
  "std::vector":
    instantiations:
      - "std::vector<int>"
      - "std::vector<double>"
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.yaml");
    std::fs::write(&path, yaml).unwrap();
    let schema: SidecarSchema = load_sidecar(&path).unwrap();

    let mut graph = HeaderGraph::default();
    graph.extend_from_sidecar(&schema);
    assert_eq!(
        graph.template_instantiations,
        vec![
            "std::vector<int>".to_string(),
            "std::vector<double>".to_string(),
        ],
    );
}

#[test]
fn m25_extend_from_sidecar_dedups_against_existing_entries() {
    let yaml = r#"schema: 1
types:
  "A":
    instantiations: ["A<int>", "A<long>"]
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.yaml");
    std::fs::write(&path, yaml).unwrap();
    let schema = load_sidecar(&path).unwrap();

    // Pre-seed the graph with one entry that overlaps the schema.
    let mut graph = HeaderGraph {
        template_instantiations: vec!["A<int>".into(), "Pre<existing>".into()],
        ..HeaderGraph::default()
    };
    graph.extend_from_sidecar(&schema);

    // `A<int>` already there; should not duplicate. `A<long>`
    // appended in source order. Pre-existing entries preserved.
    assert_eq!(
        graph.template_instantiations,
        vec![
            "A<int>".to_string(),
            "Pre<existing>".to_string(),
            "A<long>".to_string(),
        ],
    );
}

#[test]
fn m25_extend_from_sidecar_idempotent_on_repeat_call() {
    let yaml = r#"schema: 1
types:
  "Foo":
    instantiations: ["Foo<bar>"]
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.yaml");
    std::fs::write(&path, yaml).unwrap();
    let schema = load_sidecar(&path).unwrap();

    let mut graph = HeaderGraph::default();
    graph.extend_from_sidecar(&schema);
    graph.extend_from_sidecar(&schema); // second call: no-op
    assert_eq!(graph.template_instantiations, vec!["Foo<bar>".to_string()]);
}

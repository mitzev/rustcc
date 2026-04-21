use cxx_importer::{Driver, HeaderGraph};

#[test]
fn driver_constructs() {
    let g = HeaderGraph {
        roots: Vec::new(),
        include_paths: Vec::new(),
        clang_flags: Vec::new(),
    };
    let d = Driver::new(g);
    assert!(d.graph().roots.is_empty());
}

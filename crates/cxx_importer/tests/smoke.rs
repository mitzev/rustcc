use cxx_importer::{Driver, HeaderGraph};

#[test]
fn driver_constructs() {
    let g = HeaderGraph::default();
    let d = Driver::new(g);
    assert!(d.graph().roots.is_empty());
}

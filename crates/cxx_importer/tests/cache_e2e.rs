//! End-to-end test for the `Driver::load_or_parse` path (M10).
//!
//! Builds a minimal `HeaderGraph`, runs `load_or_parse` twice on
//! the same inputs (expecting a cache hit on the second call),
//! then mutates the header content and re-runs (expecting a
//! cache miss because the SHA-256 of the header bytes changed).

#![cfg(all(feature = "libclang", feature = "cache"))]

use std::path::PathBuf;
use std::sync::Mutex;

use cxx_importer::{AnnotationSet, Driver, HeaderGraph};
use rustc_abi_cxx::{CxxTypeCtx, NameSegment, Target};

static LIBCLANG: Mutex<()> = Mutex::new(());

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_cache_e2e_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn load_or_parse_writes_cache_on_miss_and_reuses_it_on_hit() {
    let _g = LIBCLANG.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmpdir("hit_miss");
    let header = dir.join("widget.hpp");
    std::fs::write(
        &header,
        b"struct Widget { int x; int compute() const; };\n",
    )
    .unwrap();
    let cache_path = dir.join("cache.json");

    let driver = Driver::new(HeaderGraph {
        roots: vec![header.clone()],
        clang_flags: vec!["-std=c++17".into()],
        ..HeaderGraph::default()
    });

    // First call: cache miss. Parses via libclang, writes the file.
    let mut ctx1 = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let mut ann1 = AnnotationSet::default();
    let ids1 = driver
        .load_or_parse(&cache_path, &mut ctx1, &mut ann1)
        .expect("first load_or_parse");
    assert!(
        cache_path.exists(),
        "cache file should be created on miss"
    );
    assert_eq!(ids1.len(), 1, "expected one imported class");

    // Capture how big the cache file is + its mtime so we can
    // confirm the second call doesn't rewrite it.
    let cache_meta_before = std::fs::metadata(&cache_path).unwrap();
    let before_modified = cache_meta_before
        .modified()
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    // Second call with identical inputs: cache hit. The driver
    // shouldn't even invoke libclang. We can't directly observe
    // that, but we can check: ctx2 ends up with the same imported
    // class set, and the cache file mtime is unchanged (we only
    // write on miss).
    std::thread::sleep(std::time::Duration::from_millis(50));
    let mut ctx2 = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let mut ann2 = AnnotationSet::default();
    let ids2 = driver
        .load_or_parse(&cache_path, &mut ctx2, &mut ann2)
        .expect("second load_or_parse should hit cache");
    assert_eq!(ids2.len(), 1, "cache hit should restore the class set");
    let class = ctx2.class(ids2[0]);
    match class.name.0.last() {
        Some(NameSegment::Class(i)) => assert_eq!(i.0, "Widget"),
        other => panic!("unexpected class name: {other:?}"),
    }

    let cache_meta_after = std::fs::metadata(&cache_path).unwrap();
    let after_modified = cache_meta_after
        .modified()
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    assert_eq!(
        before_modified, after_modified,
        "cache file mtime should be unchanged on a hit (not rewritten)"
    );

    // Mutate the header — different bytes hash to a different
    // digest, so the cache key changes and the third call must
    // miss + re-write.
    std::fs::write(
        &header,
        b"struct Widget { int x; int y; int compute() const; };\n",
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut ctx3 = CxxTypeCtx::new(Target::aarch64_apple_darwin());
    let mut ann3 = AnnotationSet::default();
    let ids3 = driver
        .load_or_parse(&cache_path, &mut ctx3, &mut ann3)
        .expect("third load_or_parse after header change");
    assert_eq!(ids3.len(), 1);

    let cache_meta_post_mutate = std::fs::metadata(&cache_path).unwrap();
    let post_modified = cache_meta_post_mutate
        .modified()
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    assert!(
        post_modified > before_modified,
        "cache file should be rewritten after header content change"
    );

    // Updated Widget should have the new field count.
    let class3 = ctx3.class(ids3[0]);
    assert_eq!(
        class3.fields.len(),
        2,
        "expected updated Widget to have 2 fields after header mutation"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

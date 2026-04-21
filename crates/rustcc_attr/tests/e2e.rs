//! End-to-end: parse a Rust source file with `#[repr(cpp)]`, lower
//! into `CxxTypeCtx`, generate the `.hpp`, compile the header with
//! `clang++ -c` to confirm the declarations are valid C++ and the
//! `static_assert`s on `sizeof`/`alignof` hold.
//!
//! Runs `clang++` directly; skipped implicitly if `clang++` isn't on
//! `$PATH` (the invocation call panics with a descriptive message so
//! the reason surfaces in CI logs).

use std::path::PathBuf;
use std::process::Command;

use cxx_importer::hpp::generate_hpp_for_rust_types;
use rustc_abi_cxx::{CxxTypeCtx, Target};
use rustcc_attr::{lower_into_ctx, parse_source};

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rustcc_attr_e2e_{}_{}_{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&dir).expect("mkdir tmp");
    dir
}

#[test]
fn rust_source_roundtrips_through_generated_header() {
    let rust_source = r#"
        #[repr(cpp)]
        pub struct Point {
            pub x: i32,
            pub y: i32,
        }

        impl Point {
            pub fn new(x: i32, y: i32) -> Self { Point { x, y } }
            pub fn magnitude_sq(&self) -> i32 {
                self.x * self.x + self.y * self.y
            }
        }

        #[repr(cpp, align(16))]
        #[cpp_name = "Vec4"]
        pub struct AlignedFour {
            pub a: f32,
            pub b: f32,
            pub c: f32,
            pub d: f32,
        }
    "#;

    let parsed = parse_source(rust_source).expect("parse ok");
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let ids = lower_into_ctx(&parsed, &mut ctx).expect("lower ok");
    assert!(ids.contains_key("Point"));
    assert!(ids.contains_key("AlignedFour"));

    let hpp = generate_hpp_for_rust_types(&ctx).expect("hpp emission");

    // Sanity: both types show up in the output.
    assert!(
        hpp.contains("class Point {"),
        "Point not emitted: {hpp}"
    );
    assert!(
        hpp.contains("class Vec4 {"),
        "cpp_name override didn't take effect:\n{hpp}"
    );

    // Write .hpp + consumer and compile.
    let dir = tmpdir("point");
    let hpp_path = dir.join("point-cxx.hpp");
    let consumer_path = dir.join("consumer.cpp");
    let obj_path = dir.join("consumer.o");
    std::fs::write(&hpp_path, &hpp).expect("write hpp");

    let consumer_src = r#"
#include "point-cxx.hpp"

static_assert(sizeof(Point) == 8, "Point 2xi32 must be 8 bytes");
static_assert(alignof(Point) == 4, "Point must align to 4");

static_assert(sizeof(Vec4) == 16, "Vec4 4xf32 must be 16 bytes");
static_assert(alignof(Vec4) == 16, "Vec4 #[repr(align(16))] must align to 16");
"#;
    std::fs::write(&consumer_path, consumer_src).expect("write consumer");

    let status = Command::new("clang++")
        .args(["-std=c++17", "-c", "-o"])
        .arg(&obj_path)
        .arg(&consumer_path)
        .current_dir(&dir)
        .status()
        .expect("spawn clang++ (is it on PATH?)");
    assert!(
        status.success(),
        "consumer failed to compile against generated header. hpp:\n{hpp}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn record_fields_compose_correctly_in_generated_header() {
    let rust_source = r#"
        #[repr(cpp)]
        pub struct Point { pub x: i32, pub y: i32 }

        #[repr(cpp)]
        pub struct Segment {
            pub start: Point,
            pub end: Point,
        }
    "#;

    let parsed = parse_source(rust_source).expect("parse ok");
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let _ = lower_into_ctx(&parsed, &mut ctx).expect("lower ok");
    let hpp = generate_hpp_for_rust_types(&ctx).expect("hpp emission");

    let dir = tmpdir("segment");
    let hpp_path = dir.join("segment-cxx.hpp");
    let consumer_path = dir.join("consumer.cpp");
    let obj_path = dir.join("consumer.o");
    std::fs::write(&hpp_path, &hpp).expect("write hpp");

    // Compose records in C++ and ensure the sizes match the Rust IR.
    // Point = 8, Segment = 16 (two Points back-to-back, no padding).
    let consumer_src = r#"
#include "segment-cxx.hpp"
static_assert(sizeof(Point) == 8);
static_assert(alignof(Point) == 4);
static_assert(sizeof(Segment) == 16);
static_assert(alignof(Segment) == 4);
"#;
    std::fs::write(&consumer_path, consumer_src).expect("write consumer");

    let status = Command::new("clang++")
        .args(["-std=c++17", "-c", "-o"])
        .arg(&obj_path)
        .arg(&consumer_path)
        .current_dir(&dir)
        .status()
        .expect("spawn clang++");
    assert!(
        status.success(),
        "record-field composition did not compile. hpp:\n{hpp}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scoped_enums_render_and_compile_as_cpp_enum_class() {
    let rust_source = r#"
        #[repr(cpp)]
        pub enum Color { Red, Green = 5, Blue }

        #[repr(cpp, i16)]
        pub enum Tiny { A, B, C }
    "#;

    let parsed = parse_source(rust_source).expect("parse ok");
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    let _ = lower_into_ctx(&parsed, &mut ctx).expect("lower ok");
    let hpp = generate_hpp_for_rust_types(&ctx).expect("hpp");

    // Sanity on the rendered shape.
    assert!(hpp.contains("enum class Color : std::int32_t {"), "body:\n{hpp}");
    assert!(hpp.contains("Red,"));
    assert!(hpp.contains("Green = 5,"));
    assert!(hpp.contains("enum class Tiny : std::int16_t {"), "tiny:\n{hpp}");

    let dir = tmpdir("enums");
    let hpp_path = dir.join("enums-cxx.hpp");
    let consumer_path = dir.join("consumer.cpp");
    let obj_path = dir.join("consumer.o");
    std::fs::write(&hpp_path, &hpp).expect("write hpp");

    let consumer = r#"
#include "enums-cxx.hpp"
static_assert(sizeof(Color) == 4, "Color default should be 4 bytes (int32)");
static_assert(sizeof(Tiny) == 2, "Tiny (#[repr(i16)]) should be 2 bytes");
static_assert(static_cast<int>(Color::Red) == 0);
static_assert(static_cast<int>(Color::Green) == 5);
static_assert(static_cast<int>(Color::Blue) == 6, "auto-increment from Green=5");
"#;
    std::fs::write(&consumer_path, consumer).unwrap();

    let status = Command::new("clang++")
        .args(["-std=c++17", "-c", "-o"])
        .arg(&obj_path)
        .arg(&consumer_path)
        .current_dir(&dir)
        .status()
        .expect("spawn clang++");
    assert!(
        status.success(),
        "enum header failed to compile. hpp:\n{hpp}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn method_signatures_land_in_generated_header() {
    let rust_source = r#"
        #[repr(cpp)]
        pub struct Counter {
            pub count: i64,
        }
        impl Counter {
            pub fn new() -> Self { Counter { count: 0 } }
            pub fn get(&self) -> i64 { self.count }
            pub fn inc(&mut self, by: i64) {}
        }
    "#;

    let parsed = parse_source(rust_source).expect("parse ok");
    let mut ctx = CxxTypeCtx::new(Target::x86_64_apple_darwin());
    lower_into_ctx(&parsed, &mut ctx).expect("lower ok");
    let hpp = generate_hpp_for_rust_types(&ctx).expect("hpp emission");

    // `new` is sugared to a ctor.
    assert!(
        hpp.contains("Counter();"),
        "default-ctor signature missing:\n{hpp}"
    );
    // Method signatures, with `&self` → const and `&mut self` → non-const.
    // i64 renders as `long long` (not `std::int64_t`) to keep the
    // C++ source in sync with rustc_abi_cxx's Itanium mangling
    // across targets where `int64_t`'s underlying type differs.
    assert!(
        hpp.contains("long long get() const;"),
        "const method missing:\n{hpp}"
    );
    assert!(
        hpp.contains("void inc(long long arg0);"),
        "mut method missing:\n{hpp}"
    );
}

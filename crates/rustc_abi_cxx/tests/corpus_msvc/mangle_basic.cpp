// @kind mangle
// @target x86_64-pc-windows-msvc
//
// Basic function and method mangling under the MSVC C++ ABI.
//
// Covers:
// - Free function with a single int parameter.
// - Member functions with overload on parameter type.
// - `const` method qualifier (encoded as `B` after `E`).
// - Constructor (mangled as `??0`).
// - Destructor (mangled as `??1`).

struct Foo {
    void bar(int x);
    void bar(double x) const;
    Foo();
    Foo(int x);
    ~Foo();
};

void Foo::bar(int x) { (void)x; }
void Foo::bar(double x) const { (void)x; }
Foo::Foo() {}
Foo::Foo(int x) { (void)x; }
Foo::~Foo() {}

void free_fn(int x) { (void)x; }

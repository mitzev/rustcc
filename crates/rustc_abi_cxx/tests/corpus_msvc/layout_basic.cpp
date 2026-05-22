// @kind layout
// @target x86_64-pc-windows-msvc
//
// Basic record layouts under MSVC. The reference output is generated
// via `clang -target x86_64-pc-windows-msvc -fms-compatibility
// -Xclang -fdump-record-layouts`.

struct S {
    int a;
    char b;
    int c;
};
S _s;

struct Empty {};
Empty _e;

struct Aligned {
    alignas(16) int x;
};
Aligned _a;

struct WithPad {
    char a;
    int b;
    char c;
    long long d;
};
WithPad _w;

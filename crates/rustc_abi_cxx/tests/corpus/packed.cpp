// @target P
//
// Itanium `__attribute__((packed))`. Packing forces every field
// to 1-byte alignment, removing the 3 bytes of padding clang would
// otherwise insert before `int b`. Result: a@0, b@1, c@5,
// sizeof=6, align=1 (vs. the unpacked sizeof=12, align=4).

struct __attribute__((packed)) P {
    char a;
    int b;
    char c;
};

P g_p;

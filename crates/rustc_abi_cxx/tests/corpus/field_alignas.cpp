// @kind layout
// @target WithAlign
//
// `alignas(N)` on an individual field raises that field's placement
// alignment and the class's overall alignment. Here `aligned` has
// `alignas(16)` so it lands at offset 16 (not 4), and `tail` follows at
// offset 20, with sizeof rounded up to a multiple of 16.

struct WithAlign {
    char small;
    alignas(16) int aligned;
    char tail;
};

WithAlign g;

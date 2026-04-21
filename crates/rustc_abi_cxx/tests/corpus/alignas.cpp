// @kind layout
// @target Aligned
//
// Explicit alignment override via `alignas`. The record's natural
// alignment from its members is 4 (single `int`), but `alignas(16)`
// raises both alignment and size: nvalign=16, sizeof rounds up to 16.

struct alignas(16) Aligned {
    int x;
};

Aligned g_aligned;

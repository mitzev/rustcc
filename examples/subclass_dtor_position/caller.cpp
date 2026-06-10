#include "cpp/cppbase.hpp"
#include <cstdio>
int g_drop = 0;
extern "C" void note_drop() { g_drop++; }
extern "C" NotFirst* make_d(int x, int extra);
extern "C" int run() {
    NotFirst* d = make_d(7, 5);
    int e = d->early();   // -> Rust override = 105
    int l = d->late();    // -> Rust override = 205
    delete d;             // dtor pair at slots 1/2 (NOT 0/1!)
    if (e != 105) { printf("FAIL early=%d\n", e); return 1; }
    if (l != 205) { printf("FAIL late=%d\n", l); return 2; }
    if (g_ctor != 1 || g_dtor != 1 || g_drop != 1) {
        printf("FAIL counters ctor=%d dtor=%d drop=%d\n", g_ctor, g_dtor, g_drop);
        return 3;
    }
    printf("early=%d late=%d ctor=%d dtor=%d drop=%d -- DTOR-NOT-FIRST cross-boundary OK\n",
           e, l, g_ctor, g_dtor, g_drop);
    return 0;
}

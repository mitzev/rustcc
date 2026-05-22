// @kind mangle
// @target x86_64-pc-windows-msvc
//
// Exercises both back-reference tables — name (digits 0-9) and
// type (digits 0-9). MSVC's tables are keyed by:
// - Name table: identifier text (each scope segment of a
//   qualified name is one slot).
// - Type table: semantic type identity, but only at the *top
//   level* of a parameter position. Nested types inside modifiers
//   bypass the type table and consult only the name table.

struct V { int x; };

// Two by-value V — type back-ref fires on the second.
void g_vv(V, V) {}

// V by value then V& — different top-level types, no type back-
// ref. But the name V inside the ref still compresses via name
// table.
void g_vr(V, V&) {}

// Two V* — same top-level type, type back-ref.
void g_pp(V*, V*) {}

// V*, V&, V — three different top-level types, no type back-refs;
// but the name V back-refs across the param list.
void g_pvr(V*, V&, V) {}

// Same builtin twice — int* repeated — type back-ref.
void g_ipp(int*, int*) {}
void g_ippp(int*, int*, int*) {}

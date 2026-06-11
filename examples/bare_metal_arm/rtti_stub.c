// Stub for libc++abi's __class_type_info vtable. On bare-metal
// ARM this symbol is normally unresolved unless the user links
// libsupc++/libc++abi. For a minimal probe we provide a weak
// definition so the linker is satisfied; RTTI at runtime
// (typeid, dynamic_cast) will NOT actually work without real
// libc++abi, but virtual dispatch through the vtable does not
// consult typeinfo, so foo() and similar calls are unaffected.

__attribute__((weak))
void* _ZTVN10__cxxabiv117__class_type_infoE[4] = { 0 };

// Same for __si_class_type_info — referenced by the _ZTI of a
// DERIVED class (single inheritance chains base typeinfo through it).
__attribute__((weak))
void* _ZTVN10__cxxabiv120__si_class_type_infoE[4] = { 0 };

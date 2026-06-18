/* Zephyr C++ interop probe (Cortex-M3, qemu_cortex_m3).
 *
 * Runs the four rustcc dispatch checks shared with
 * examples/bare_metal_arm — proving the fork's vtables, ctor vptr
 * install, and cross-compiler RTTI behave under Zephyr's build system
 * and runtime, not just a hand-rolled FreeRTOS image.
 */
#include <zephyr/kernel.h>
#include <zephyr/sys/printk.h>

/* C++ side (examples/bare_metal_arm/caller.cpp + the Rust class lib). */
extern int demo(int v);                       /* Rust base-class virtual      -> 105  */
extern int demo_subclass(int v, int scale);   /* Rust override via base ptr   -> 4000 */
extern int demo_imported_override(int id, int off); /* override imported base -> 503  */
extern int demo_imported_base(int id, int off);     /* inherited C++ slot     -> 42   */

int main(void)
{
	int got[4] = {
		demo(5),
		demo_subclass(3, 4),
		demo_imported_override(7, 3),
		demo_imported_base(7, 3),
	};
	static const int expected[4] = { 105, 4000, 503, 42 };

	for (int i = 0; i < 4; i++) {
		if (got[i] != expected[i]) {
			printk("ZEPHYR CXX PROBE: FAIL (check %d: got %d, want %d)\n",
			       i, got[i], expected[i]);
			return 0;
		}
	}
	printk("ZEPHYR CXX PROBE (Cortex-M3): PASS "
	       "(105/4000/503/42 — class, subclass, imported override + inherited)\n");
	return 0;
}

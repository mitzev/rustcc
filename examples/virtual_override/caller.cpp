#include <cstdint>
#include <cstdio>

struct Animal {
    virtual uint32_t speak();
    virtual uint32_t legs();
    uint32_t tag;
};

struct Dog : Animal {
    // speak() overrides Animal::speak
    // legs() inherits Animal::legs
    virtual uint32_t wag();          // new virtual at slot 2
    uint32_t bark;
};

extern "C" Dog* make_dog(uint32_t tag, uint32_t bark);
extern "C" Animal* make_animal(uint32_t tag);
extern "C" void free_dog(Dog* d);
extern "C" void free_animal(Animal* a);

extern "C" int run(void) {
    Dog* d = make_dog(7, 3);
    Animal* a = static_cast<Animal*>(d);

    // Through Animal*, on a Dog: should call Dog::speak (override)
    uint32_t sp_via_base = a->speak();   // expect 3 + 1000 = 1003
    // Through Animal*, on a Dog: inherited — calls Animal::legs
    uint32_t lg_via_base = a->legs();    // expect 4
    // Through Dog*: new virtual
    uint32_t wg = d->wag();              // expect 3 * 2 = 6

    // Plain Animal — should hit Animal::speak (no override here)
    Animal* plain = make_animal(5);
    uint32_t sp_plain = plain->speak();  // expect 5 * 10 = 50

    free_dog(d);
    free_animal(plain);

    if (sp_via_base != 1003) {
        std::printf("FAIL sp_via_base=%u expected 1003 (override)\n", sp_via_base);
        return 1;
    }
    if (lg_via_base != 4) {
        std::printf("FAIL lg_via_base=%u expected 4\n", lg_via_base);
        return 2;
    }
    if (wg != 6) {
        std::printf("FAIL wg=%u expected 6\n", wg);
        return 3;
    }
    if (sp_plain != 50) {
        std::printf("FAIL sp_plain=%u expected 50\n", sp_plain);
        return 4;
    }
    std::printf(
        "speak-on-Dog-via-Animal* = %u (override OK); "
        "legs-inherited = %u; wag-new = %u; "
        "speak-on-plain-Animal = %u -- OK\n",
        sp_via_base, lg_via_base, wg, sp_plain);
    return 0;
}

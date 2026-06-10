// Tiny driver: all logic lives in the Rust staticlib.
extern "C" int run_test();
int main() { return run_test(); }

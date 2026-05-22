// @kind layout
// @target x86_64-pc-windows-msvc
//
// `#pragma pack(N)` layouts. Validates:
// - pack(1): all fields placed at their natural offset (no padding
//   for alignment); each field's alignment becomes min(natural, 1).
// - pack(2): fields aligned to min(natural, 2), so int gets 2-byte
//   align instead of 4, long long gets 2-byte align instead of 8.
// - Default (no pragma): full natural alignment.

#pragma pack(push, 1)
struct Packed1 {
    char a;
    int b;
};
#pragma pack(pop)
Packed1 _p1;

#pragma pack(push, 2)
struct Packed2 {
    char a;
    int b;
    long long c;
};
#pragma pack(pop)
Packed2 _p2;

struct Default {
    char a;
    int b;
    long long c;
};
Default _d;

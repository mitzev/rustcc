// @target BF
//
// Itanium bit-field packing. Three unsigned-int bit-fields share
// storage units per the Itanium C++ ABI: `a` (4 bits) and `b`
// (20 bits) pack into the first 4-byte allocation unit (bits
// 0..3 and 4..23), `c` (8 bits) starts at byte 3 (bits 24..31 of
// the same AU). The trailing non-bitfield `char tail` lands at
// byte 4. Total sizeof = 8 (4-byte AU + 1 byte tail, padded to
// the 4-byte alignment of `unsigned int`).

struct BF {
    unsigned int a : 4;
    unsigned int b : 20;
    unsigned int c : 8;
    char tail;
};

BF g_bf;

// @kind layout
// @target WithArrays
//
// Array-of-T fields. Total array size = element_size * length; alignment
// is the element's alignment. Tests that arrays don't raise alignment
// beyond the element's, and that the trailing int still finds the right
// alignment slot after a char-sized array.

struct WithArrays {
    int items[5];
    char name[16];
    int trailer;
};

WithArrays g;

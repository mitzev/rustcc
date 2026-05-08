// Umbrella header for the FLTK Hello-World demo. Pulls every
// FLTK API the Rust side needs into a single libclang TU so the
// importer parses the full surface in one parse_all call.
//
// `Fl_Window.H` already transitively includes `Fl.H`,
// `Fl_Group.H`, and `Fl_Bitmap.H`; we add `Fl_Box.H` for the
// Hello-World text widget and `Enumerations.H` for the FL_*
// constants (FL_UP_BOX, FL_RED, etc.).

#pragma once

#include <FL/Fl.H>
#include <FL/Fl_Window.H>
#include <FL/Fl_Box.H>
#include <FL/Enumerations.H>

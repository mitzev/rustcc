// Umbrella header for the ADVANCED FLTK editor example. Lean on
// purpose — only the classes the Rust subclasses and the GUI shell
// need, so binding generation stays fast and the surface predictable.
//
//   - Fl: event statics (event_key/event_state) + run loop
//   - Fl_Window -> Fl_Group -> Fl_Widget: the window shell
//   - Fl_Text_Editor -> Fl_Text_Display -> Fl_Group -> Fl_Widget:
//     the 4-level chain the Rust `class RustEditor` subclasses
//   - Fl_Text_Buffer: backing storage (line ops for Ctrl+D)
//   - Fl_Box: the 2-level chain the Rust `class StatusBox` subclasses
//   - Enumerations.H: FL_* event + key constants

#pragma once

#include <FL/Fl.H>
#include <FL/Fl_Window.H>
#include <FL/Fl_Text_Editor.H>
#include <FL/Fl_Text_Buffer.H>
#include <FL/Fl_Box.H>
#include <FL/Enumerations.H>
#include <FL/fl_draw.H>

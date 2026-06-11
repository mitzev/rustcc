// Umbrella header for the FLTK text-editor example. Pulls every
// FLTK API the Rust side needs into a single libclang TU so the
// importer parses the full surface in one parse_all call.
//
// Includes:
//   - Fl_Window: the top-level window (multi-inh through M22)
//   - Fl_Text_Editor: the editor widget (templates not needed; just
//     subclass chains)
//   - Fl_Text_Buffer: the editor's backing storage
//   - Fl_Menu_Bar / Fl_Menu_Item: File > Open / Save / Quit
//   - Fl_Native_File_Chooser: open/save dialogs that use the OS
//     native chooser (avoids importing FLTK's own file widgets)
//   - Enumerations.H: FL_* constants

#pragma once

#include <FL/Fl.H>
#include <FL/Fl_Window.H>
#include <FL/Fl_Text_Editor.H>
#include <FL/Fl_Text_Buffer.H>
#include <FL/Fl_Menu_Bar.H>
#include <FL/Fl_Menu_Item.H>
#include <FL/Fl_Native_File_Chooser.H>
#include <FL/Fl_Hold_Browser.H>
#include <FL/Fl_Tabs.H>
#include <FL/Fl_Tile.H>
#include <FL/Enumerations.H>

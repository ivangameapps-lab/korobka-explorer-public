# r/rust post

**Title:** I built a Windows file manager in Rust + egui with indexed search, a trash window, and long-path support

**Body:**

Over the last weeks I built **Korobka Explorer** — a native Windows file manager in Rust 2021 with eframe/egui 0.31.

It started as an experiment: can a Rust/egui app handle a real file manager workload without wrapping Explorer or shipping Electron? The result is a single ~5 MB executable that talks to Win32 directly.

What it does:

- virtualized list and tile views, tabs, breadcrumbs, quick access;
- instant filter of the current folder, recursive folder search, and whole-volume search;
- compact on-disk index (directory path once per block; 2.25M entries ≈ 90 MB, cached read ≈ 0.13 s);
- copy/move with replace/keep-both/skip conflicts, progress, cancel, undo;
- a trash window with original paths and restore;
- long paths via `\\?\` for enumeration, copy, move, delete and ZIP extraction;
- Restart Manager integration for locked files;
- matte/glass themes and English/Russian UI.

The most interesting Rust parts were the parallel directory walk with a streaming cache writer, the compact block format, and the long-path wrapper around Win32 calls. I also spent a lot of time on the “boring” parts: focus handling, conflict dialogs, and making sure Delete/Ctrl+C actually work after clicking a row.

Repository: https://github.com/ivangameapps-lab/korobka-explorer-public

Demo GIF and screenshots are in the README. Release exe is unsigned, so SmartScreen may warn.

Known limitations: Windows only, custom trash instead of the Windows Recycle Bin, no incremental index yet, no single-instance guard. Feedback on the search/index design and long-path edge cases is very welcome.

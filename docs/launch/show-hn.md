# Show HN: Korobka Explorer – a Rust file manager for Windows

I built a native Windows file manager in Rust with egui. It is not a wrapper around Explorer and not Electron: one ~5 MB executable, Win32 file enumeration, a compact on-disk search index, a restoreable trash, and long-path support.

Repository: https://github.com/ivangameapps-lab/korobka-explorer-public

What is inside:

- Tabs, list/tile views, breadcrumbs, quick access, properties panel.
- Instant filter of the current folder; Enter searches the folder recursively, the magnifier searches the whole volume.
- Compact index: the directory path is stored once per block; a 2.25M-entry C:\ index is ~90 MB and reads in ~0.13 s.
- Exact/partial name modes, masks (`*.txt`, `отч?т`), quoted phrases, path queries.
- Copy/move with conflict handling, progress, cancel, undo, and a trash window with restore to the original path.
- `\\?\` long-path handling for enumeration, copy, move, delete and ZIP extraction.
- Matte and glass themes; Russian and English UI.
- Restart Manager integration for locked files.

Known limitations, because they matter:

- Windows only.
- The trash is internal, not the Windows Recycle Bin.
- The index is rebuilt after file operations; there is no incremental update yet. The first search after changes is the CPU-heavy one.
- The release executable is unsigned, so SmartScreen may warn.
- No installer and no code signing yet.

Build:

```powershell
git clone https://github.com/ivangameapps-lab/korobka-explorer-public.git
cd korobka-explorer-public
cargo build --release
```

I would especially appreciate feedback on search behavior on large volumes, long paths, and the trash workflow.

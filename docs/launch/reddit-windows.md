# r/Windows post

**Title:** I made a lightweight Windows file manager in Rust — indexed search, trash with restore, long paths

**Body:**

I was tired of some Explorer limitations and built a small native file manager called **Korobka Explorer**. It is a single ~5 MB exe, no Electron, no installer.

What you get:

- tabs, list/tile views, quick access, properties;
- instant filtering of the current folder;
- Enter searches the current folder recursively; the magnifier searches the whole drive;
- exact-name and partial-name search, masks like `*.txt`;
- copy/move with conflict handling and undo;
- Delete goes to the app's own trash, where you can restore an item to its original folder;
- long paths over 260 characters work inside the app;
- matte and glass themes; Russian and English UI.

It is not a replacement for Explorer and not the Windows Recycle Bin — the trash is internal. The exe is unsigned, so Windows may show a SmartScreen warning.

Download / screenshots:
https://github.com/ivangameapps-lab/korobka-explorer-public/releases/tag/v0.5.0

I would be glad to hear what Explorer features are missing for daily use.

# Changelog

## 0.5.1

- Fixed a crash when switching drives repeatedly (`C:` → `D:` → `C:` → `D:`): a cached directory could be installed after the filter rebuild, leaving table row indices from the previous drive. Cached directories now rebuild the filter immediately, and list/tile rendering is bounds-checked.
- Verified with 16 consecutive `C:`/`D:` switches without a crash.

## 0.5.0

- Initial public release: indexed search, compact cache, trash window with restore, long-path support, RU/EN UI, matte/glass themes, file operations, locks and archives.

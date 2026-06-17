# phantom (v1 acceptance fixture)

Phantom is the **v1 acceptance target** for `tauri-plugin-extensions`
(see `../../../docs/DECISIONS.md` D-005). The unpacked extension contents
are distributed under Phantom's own license and are **not committed** to
this repository — only this README is tracked; everything else in this
directory is `.gitignore`d.

## Populate

From the repo root, on Windows PowerShell:

```powershell
pwsh -File scripts/fetch-phantom.ps1
```

The script pulls the latest Chrome Web Store build for extension id
`bfnaelmomeimhlpmgjnjophhpkkoljpa`, strips the CRX header, and unpacks
the underlying ZIP into this directory.

After a successful fetch you should see roughly:

```
phantom/
├── README.md                (this file, committed)
├── manifest.json            (from Phantom's build)
├── background.js            (or similarly-named service worker)
├── content.js
├── ... every other asset Phantom's ZIP contained
```

Verify `manifest.json`'s `manifest_version` is `3`.

## Re-fetch

The script overwrites every file in this directory except `README.md`
on re-run, so just run it again to pick up a new Phantom release.

## Why not commit the extension

- Phantom's code is under Phantom's license; redistributing their
  compiled build in this repository is not ours to do.
- Every developer should pull the current Chrome Web Store release,
  which is the one end-users actually run, rather than pinning to a
  snapshot that drifts.

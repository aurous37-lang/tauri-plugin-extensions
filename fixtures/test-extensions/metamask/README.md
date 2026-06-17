# metamask (v2 acceptance fixture)

MetaMask is a **v2 acceptance target** for `tauri-plugin-extensions`
(see `../../../docs/DECISIONS.md` D-005). The unpacked extension
contents are distributed under MetaMask's license and are **not
committed** here; only this README is tracked.

## Populate

From the repo root, on Windows PowerShell:

```powershell
pwsh -File scripts/fetch-metamask.ps1
```

CWS id: `nkbihfbeogaeaoehlefnkodbefgpgknn`.

## Notes

MetaMask's manifest is the most complex of the three acceptance targets
(larger `web_accessible_resources` list, content scripts that inject on
specific `https://*/*` subsets rather than `<all_urls>`). When Phantom
passes v1 and we pivot to v2, use this fixture to stress the matcher +
script-injection timing paths.

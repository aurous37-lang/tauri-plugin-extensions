# fetch-phantom.ps1 - Download and unpack Phantom extension into fixtures.
#
# Phantom's Chrome Web Store extension id is bfnaelmomeimhlpmgjnjophhpkkoljpa.
# The CRX download endpoint is the Chrome Web Store update service; specifying
# `response=redirect` asks it to 302 to a signed CRX artifact on
# clients2.googleusercontent.com.
#
# A CRX file is a small header followed by a standard ZIP. Header layout
# (CRX3, in little-endian):
#   magic    : u8[4]   "Cr24"
#   version  : u32     3
#   hdr_size : u32     length of the protobuf header that follows
#   hdr      : bytes   protobuf (signatures, public keys)
#   zip      : bytes   until EOF - the extension's manifest + files
#
# We slice off the (magic + version + hdr_size + hdr) prefix and feed the
# remainder to Expand-Archive. The result is the same layout you'd get from a
# Chrome "Pack extension" dialog.
#
# Non-network failures (corrupt download, manifest missing, MV2 extension)
# exit 1 with a human-readable explanation. Network failures also exit 1 but
# are self-explanatory (WebException thrown to the console).

[CmdletBinding()]
param(
    [string] $ExtensionId = "bfnaelmomeimhlpmgjnjophhpkkoljpa",
    [string] $DisplayName = "Phantom",
    [string] $TargetDirName = "phantom"
)

$ErrorActionPreference = "Stop"

function Fetch-CrxExtension {
    param(
        [Parameter(Mandatory)] [string] $ExtensionId,
        [Parameter(Mandatory)] [string] $TargetDir,
        [Parameter(Mandatory)] [string] $DisplayName
    )

    $repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
    $fixturesRoot = Join-Path $repoRoot "fixtures/test-extensions"
    $targetPath = Join-Path $fixturesRoot $TargetDir

    Write-Host ""
    Write-Host ("Fetching {0} ({1}) into {2}" -f $DisplayName, $ExtensionId, $targetPath)

    $crxUrl = ("https://clients2.google.com/service/update2/crx" +
        "?response=redirect" +
        "&acceptformat=crx2,crx3" +
        "&prodversion=120" +
        "&x=id%3D$ExtensionId%26installsource%3Dondemand%26uc")

    $tempCrx = Join-Path ([System.IO.Path]::GetTempPath()) ("$ExtensionId.crx")
    $tempZip = Join-Path ([System.IO.Path]::GetTempPath()) ("$ExtensionId.zip")
    $tempExtract = Join-Path ([System.IO.Path]::GetTempPath()) ("$ExtensionId-extract")

    # Clean up any leftover temp artifacts from a previous run.
    foreach ($p in @($tempCrx, $tempZip)) {
        if (Test-Path $p) { Remove-Item $p -Force }
    }
    if (Test-Path $tempExtract) {
        Remove-Item $tempExtract -Recurse -Force
    }

    Write-Host "  -> downloading CRX ..."
    try {
        Invoke-WebRequest -Uri $crxUrl -OutFile $tempCrx -UseBasicParsing
    } catch {
        Write-Error "CRX download failed: $($_.Exception.Message)"
        exit 1
    }

    $bytes = [System.IO.File]::ReadAllBytes($tempCrx)
    if ($bytes.Length -lt 16) {
        Write-Error "CRX file too small ($($bytes.Length) bytes); aborting."
        exit 1
    }

    # Magic "Cr24"
    if (-not ($bytes[0] -eq 0x43 -and $bytes[1] -eq 0x72 -and $bytes[2] -eq 0x32 -and $bytes[3] -eq 0x34)) {
        Write-Error ("CRX magic missing (got 0x{0:X2}{1:X2}{2:X2}{3:X2}); Chrome Web Store may have returned an HTML page." -f $bytes[0], $bytes[1], $bytes[2], $bytes[3])
        exit 1
    }

    $version  = [BitConverter]::ToUInt32($bytes, 4)

    if ($version -eq 2) {
        # CRX2 header: magic(4) + version(4) + pkLen(4) + sigLen(4) + pk + sig
        $pkLen  = [BitConverter]::ToUInt32($bytes, 8)
        $sigLen = [BitConverter]::ToUInt32($bytes, 12)
        $zipStart = 16 + $pkLen + $sigLen
    } elseif ($version -eq 3) {
        # CRX3 header: magic(4) + version(4) + hdrLen(4) + hdr
        $hdrLen   = [BitConverter]::ToUInt32($bytes, 8)
        $zipStart = 12 + $hdrLen
    } else {
        Write-Error "Unsupported CRX version $version"
        exit 1
    }

    if ($zipStart -ge $bytes.Length) {
        Write-Error ("CRX header claims ZIP starts at offset $zipStart but file is only $($bytes.Length) bytes.")
        exit 1
    }

    $zipBytes = New-Object byte[] ($bytes.Length - $zipStart)
    [Array]::Copy($bytes, $zipStart, $zipBytes, 0, $zipBytes.Length)

    # Sanity-check: ZIP files start with PK\x03\x04.
    if (-not ($zipBytes[0] -eq 0x50 -and $zipBytes[1] -eq 0x4B -and $zipBytes[2] -eq 0x03 -and $zipBytes[3] -eq 0x04)) {
        Write-Error ("Stripped CRX header but next bytes are not a ZIP signature: 0x{0:X2}{1:X2}{2:X2}{3:X2}" -f $zipBytes[0], $zipBytes[1], $zipBytes[2], $zipBytes[3])
        exit 1
    }

    [System.IO.File]::WriteAllBytes($tempZip, $zipBytes)

    Write-Host "  -> unpacking ZIP ..."
    Expand-Archive -Path $tempZip -DestinationPath $tempExtract -Force

    $manifestPath = Join-Path $tempExtract "manifest.json"
    if (-not (Test-Path $manifestPath)) {
        Write-Error "Unpacked archive has no manifest.json at its root."
        exit 1
    }

    $manifestJson = Get-Content $manifestPath -Raw | ConvertFrom-Json
    $mv = $manifestJson.manifest_version
    if ($mv -ne 3) {
        Write-Error ("Expected manifest_version 3; got $mv. This build is not MV3 and will not load.")
        exit 1
    }

    # Replace the target directory atomically: clear everything except README.md,
    # then move the contents of $tempExtract in.
    if (-not (Test-Path $targetPath)) {
        New-Item -ItemType Directory -Path $targetPath -Force | Out-Null
    }

    Get-ChildItem -Path $targetPath -Force | Where-Object { $_.Name -ne "README.md" } | ForEach-Object {
        Remove-Item $_.FullName -Recurse -Force
    }

    Get-ChildItem -Path $tempExtract -Force | ForEach-Object {
        Move-Item -Path $_.FullName -Destination (Join-Path $targetPath $_.Name) -Force
    }

    Remove-Item $tempCrx -Force -ErrorAction SilentlyContinue
    Remove-Item $tempZip -Force -ErrorAction SilentlyContinue
    if (Test-Path $tempExtract) {
        Remove-Item $tempExtract -Recurse -Force -ErrorAction SilentlyContinue
    }

    Write-Host ""
    Write-Host ("{0} {1} fetched." -f $manifestJson.name, $manifestJson.version)
    Write-Host ("   target: {0}" -f $targetPath)
    Write-Host ""
}

Fetch-CrxExtension -ExtensionId $ExtensionId -TargetDir $TargetDirName -DisplayName $DisplayName

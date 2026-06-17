# fetch-rabby.ps1 - Download and unpack Rabby extension into fixtures.
#
# Thin wrapper around fetch-phantom.ps1 reusing the same CRX-unpack path.
# Rabby's CWS id is acmacodkjbdgmoleebolmdjonilkdbch.

[CmdletBinding()] param()
$ErrorActionPreference = "Stop"

& (Join-Path $PSScriptRoot "fetch-phantom.ps1") `
    -ExtensionId "acmacodkjbdgmoleebolmdjonilkdbch" `
    -DisplayName "Rabby" `
    -TargetDirName "rabby"

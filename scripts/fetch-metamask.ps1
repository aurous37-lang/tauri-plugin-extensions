# fetch-metamask.ps1 - Download and unpack MetaMask extension into fixtures.
#
# Thin wrapper around fetch-phantom.ps1 reusing the same CRX-unpack path.
# MetaMask's CWS id is nkbihfbeogaeaoehlefnkodbefgpgknn.

[CmdletBinding()] param()
$ErrorActionPreference = "Stop"

& (Join-Path $PSScriptRoot "fetch-phantom.ps1") `
    -ExtensionId "nkbihfbeogaeaoehlefnkodbefgpgknn" `
    -DisplayName "MetaMask" `
    -TargetDirName "metamask"

# Builds the Windows installer, kite-VERSION-x86_64.msi, with WiX 3.14.1 (downloaded from the
# project's release page and checked against the hash below). Used by CI and the release workflow;
# by hand:
#
#   cargo build --release -p kite-cli -p kite-desktop
#   scripts\fetch-wintun.ps1 target\wintun
#   scripts\build-msi.ps1 -Version 0.1.0 -Bin target\release -Wintun target\wintun -Out dist
#
# The installer is not signed yet, see docs/windows.md.
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$Bin,
    [Parameter(Mandatory = $true)][string]$Wintun,
    [string]$Out = 'dist'
)

$ErrorActionPreference = 'Stop'

$wixUrl = 'https://github.com/wixtoolset/wix3/releases/download/wix3141rtm/wix314-binaries.zip'
$wixSha256 = '6ac824e1642d6f7277d0ed7ea09411a508f6116ba6fae0aa5f2c7daa2ff43d31'

# Windows Installer wants a.b.c with each part under 65536 and ignores a fourth. The third part is
# PATCH*100 + R, with R the release candidate number or 99 for the release, as the Android
# versionCode is made (docs/releasing.md): 0.1.0-rc.1 is 0.1.1, 0.1.0 is 0.1.99, 0.1.1-rc.1 is
# 0.1.101, so the number only goes up.
if ($Version -notmatch '^(\d+)\.(\d+)\.(\d+)(?:-rc\.(\d+))?$') {
    throw "the version $Version is not MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-rc.N"
}
$rc = if ($Matches[4]) { [int]$Matches[4] } else { 99 }
if ($rc -gt 99) { throw 'a release candidate number above 99 does not fit' }
$third = [int]$Matches[3] * 100 + $rc
if ([int]$Matches[1] -gt 255 -or [int]$Matches[2] -gt 255 -or $third -gt 65535) {
    throw "the version $Version does not fit a Windows Installer version"
}
$productVersion = "$($Matches[1]).$($Matches[2]).$third"

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$bin = (Resolve-Path $Bin).Path
$wintun = (Resolve-Path $Wintun).Path
foreach ($f in @("$bin\kite.exe", "$bin\kite-desktop.exe", "$wintun\wintun.dll", "$wintun\wintun-LICENSE.txt")) {
    if (-not (Test-Path $f)) { throw "$f is missing" }
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("kite-msi-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $zip = Join-Path $work 'wix.zip'
    Invoke-WebRequest -Uri $wixUrl -OutFile $zip -UseBasicParsing
    $actual = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $wixSha256) { throw "WiX has SHA-256 $actual, expected $wixSha256" }
    $wix = Join-Path $work 'wix'
    Expand-Archive -Path $zip -DestinationPath $wix

    $obj = Join-Path $work 'obj'
    New-Item -ItemType Directory -Path $obj | Out-Null
    & "$wix\candle.exe" -nologo -arch x64 -ext "$wix\WixUtilExtension.dll" `
        "-dProductVersion=$productVersion" "-dBin=$bin" "-dWintun=$wintun" "-dRoot=$root" `
        -out "$obj\" "$root\packaging\windows\kite.wxs"
    if ($LASTEXITCODE -ne 0) { throw "candle failed with $LASTEXITCODE" }

    New-Item -ItemType Directory -Path $Out -Force | Out-Null
    $msi = Join-Path (Resolve-Path $Out).Path "kite-$Version-x86_64.msi"
    & "$wix\light.exe" -nologo -sw1076 -ext "$wix\WixUtilExtension.dll" -out $msi "$obj\kite.wixobj"
    if ($LASTEXITCODE -ne 0) { throw "light failed with $LASTEXITCODE" }
    Write-Host "built $msi (installer version $productVersion)"
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

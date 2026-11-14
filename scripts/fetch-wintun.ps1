# Downloads Wintun from wintun.net, checks it against the hashes below and puts wintun.dll and its
# license into a directory. Used by CI for the installer, and by hand for a build from source:
#
#   scripts\fetch-wintun.ps1 target\debug
#
# kite.exe loads wintun.dll from its own directory and nowhere else, so put it next to kite.exe.
# The DLL is never committed: its license lets us hand it out unmodified, next to software that
# uses it only through its public API, which is how kite uses it.
param(
    [Parameter(Mandatory = $true)][string]$Out
)

$ErrorActionPreference = 'Stop'

$version = '0.14.1'
$url = "https://www.wintun.net/builds/wintun-$version.zip"
$zipSha256 = '07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51'
# amd64\wintun.dll inside the zip, signed by WireGuard LLC.
$dllSha256 = 'e5da8447dc2c320edc0fc52fa01885c103de8c118481f683643cacc3220dafce'

function Assert-Hash([string]$Path, [string]$Expected) {
    $actual = (Get-FileHash -Path $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $Expected) {
        throw "$Path has SHA-256 $actual, expected $Expected"
    }
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("kite-wintun-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $zip = Join-Path $work "wintun-$version.zip"
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    Assert-Hash $zip $zipSha256
    Expand-Archive -Path $zip -DestinationPath (Join-Path $work 'x')
    $dll = Join-Path $work 'x\wintun\bin\amd64\wintun.dll'
    Assert-Hash $dll $dllSha256

    New-Item -ItemType Directory -Path $Out -Force | Out-Null
    Copy-Item $dll (Join-Path $Out 'wintun.dll') -Force
    Copy-Item (Join-Path $work 'x\wintun\LICENSE.txt') (Join-Path $Out 'wintun-LICENSE.txt') -Force
    Write-Host "wintun $version (amd64) is in $Out"
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

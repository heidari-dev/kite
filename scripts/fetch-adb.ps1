# Downloads Google's platform-tools for Windows, checks the zip against the hash below and the
# signature of each file we keep, and puts adb.exe, its two DLLs and Google's NOTICE into a
# directory. Used by CI for the installer, and by hand for a build from source:
#
#   scripts\fetch-adb.ps1 target\adb
#
# adb is Apache-2.0 (the NOTICE has the text and the licenses of what it links); handing it out
# unmodified, with the NOTICE, is what scrcpy does too. Never committed. To move to a newer
# version, take the file name from https://dl.google.com/android/repository/repository2-3.xml,
# check the SHA-1 there against the download, and put its SHA-256 below.
param(
    [Parameter(Mandatory = $true)][string]$Out
)

$ErrorActionPreference = 'Stop'

$version = '37.0.3'
$url = "https://dl.google.com/android/repository/platform-tools_r$version-win.zip"
$zipSha256 = '89d108360a3bf2924d3bea837239edaccc22fd6ff5befd3787c4348a9fe4f09d'

$work = Join-Path ([IO.Path]::GetTempPath()) ("kite-adb-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $zip = Join-Path $work 'platform-tools.zip'
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    $actual = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $zipSha256) { throw "platform-tools has SHA-256 $actual, expected $zipSha256" }
    Expand-Archive -Path $zip -DestinationPath (Join-Path $work 'x')
    $tools = Join-Path $work 'x\platform-tools'

    # The service runs adb.exe as SYSTEM, so it must be Google's, unaltered.
    foreach ($f in 'adb.exe', 'AdbWinApi.dll', 'AdbWinUsbApi.dll') {
        $sig = Get-AuthenticodeSignature (Join-Path $tools $f)
        if ($sig.Status -ne 'Valid') { throw "$f has no valid signature: $($sig.Status)" }
        if ($sig.SignerCertificate.GetNameInfo('SimpleName', $false) -ne 'Google LLC') {
            throw "$f is not signed by Google LLC"
        }
    }

    New-Item -ItemType Directory -Path $Out -Force | Out-Null
    foreach ($f in 'adb.exe', 'AdbWinApi.dll', 'AdbWinUsbApi.dll') {
        Copy-Item (Join-Path $tools $f) (Join-Path $Out $f) -Force
    }
    Copy-Item (Join-Path $tools 'NOTICE.txt') (Join-Path $Out 'adb-NOTICE.txt') -Force
    Write-Host "adb (platform-tools $version) is in $Out"
}
finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

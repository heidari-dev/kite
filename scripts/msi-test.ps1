# Installs the MSI on this machine, checks what it did, runs the TUN smoke test against the
# installed service, uninstalls it and checks that nothing is left. CI runs it on a Windows runner;
# by hand it needs an administrator prompt and changes this computer (a service, a local group,
# routes and DNS settings for a moment), then puts it all back:
#
#   scripts\msi-test.ps1 dist\kite-0.1.0-x86_64.msi target\release\examples\relay-dev.exe
param(
    [Parameter(Mandatory = $true)][string]$Msi,
    [Parameter(Mandatory = $true)][string]$RelayDev
)

$ErrorActionPreference = 'Stop'
$Msi = (Resolve-Path $Msi).Path
$RelayDev = (Resolve-Path $RelayDev).Path
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$log = Join-Path ([IO.Path]::GetTempPath()) 'kite-msi'
New-Item -ItemType Directory -Path $log -Force | Out-Null
$app = Join-Path $env:ProgramFiles 'Kite'
$data = Join-Path $env:ProgramData 'Kite'
$menu = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\Kite\Kite.lnk'
$me = [Security.Principal.WindowsIdentity]::GetCurrent()

function Fail([string]$Message) {
    foreach ($f in Get-ChildItem $log -Filter *.log -ErrorAction SilentlyContinue) {
        Write-Host "--- $($f.Name) (tail)"
        Get-Content $f.FullName -Tail 60
    }
    if (Test-Path (Join-Path $data 'kite.log')) {
        Write-Host '--- kite.log'
        Get-Content (Join-Path $data 'kite.log') -Tail 60
    }
    throw "FAIL: $Message"
}

function Msiexec([string]$What, [string]$Name) {
    $p = Start-Process msiexec.exe -ArgumentList $What, "`"$Msi`"", '/qn', '/l*v', "`"$log\$Name.log`"" -Wait -PassThru
    if ($p.ExitCode -ne 0) { Fail "msiexec $What exited with $($p.ExitCode)" }
}

function Check([bool]$Condition, [string]$Message) { if (-not $Condition) { Fail $Message } }

Check (-not (Get-Service Kite -ErrorAction SilentlyContinue)) 'the service Kite exists before the install'
Check (-not (Test-Path $app)) 'Program Files\Kite exists before the install'
Check (-not (Get-LocalGroup -Name kite -ErrorAction SilentlyContinue)) 'the group kite exists before the install'

Msiexec '/i' 'install'

# What the install did.
foreach ($f in 'kite.exe', 'kite-desktop.exe', 'wintun.dll', 'wintun-LICENSE.txt', 'LICENSE.txt') {
    Check (Test-Path (Join-Path $app $f)) "$f was not installed"
}
$svc = Get-CimInstance Win32_Service -Filter "Name='Kite'"
Check ($null -ne $svc) 'the service Kite was not installed'
Check ($svc.State -eq 'Running') "the service is $($svc.State), not Running"
Check ($svc.StartMode -eq 'Auto') "the service starts as $($svc.StartMode), not Auto"
Check ($svc.StartName -eq 'LocalSystem') "the service runs as $($svc.StartName)"
# The path to the program is quoted, or a program in C:\ called Program.exe would run instead.
$expected = "`"$app\kite.exe`" daemon --service"
Check ($svc.PathName -eq $expected) "the service command is $($svc.PathName), not $expected"
$privs = (sc.exe qprivs Kite | Select-String 'Se\w+Privilege' -AllMatches).Matches.Value | Sort-Object
Write-Host "required privileges: $($privs -join ', ')"
Check ($privs.Count -ge 1 -and $privs.Count -le 4) 'the service token is not cut down'
Check (Test-Path $menu) 'no Start menu entry'
Check ((Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run').Kite -like '*kite-desktop.exe*--hidden') 'no autostart entry'
Check ($null -ne (Get-LocalGroup -Name kite -ErrorAction SilentlyContinue)) 'the group kite was not made'
$members = (Get-LocalGroupMember -Group kite).SID.Value
Check ($members -contains $me.User.Value) 'the person who installed is not in the group kite'
Check ((Get-Content (Join-Path $data 'users')) -contains $me.User.Value) 'the person who installed is not in the users file'
# The data directory is closed to ordinary users.
$acl = (icacls $data | Out-String)
foreach ($other in 'BUILTIN\Users', 'Everyone', 'Authenticated Users') {
    Check (-not $acl.Contains($other)) "$other can reach $data"
}
Write-Host 'installed as expected'

# The service answers on its pipe.
$out = & "$app\kite.exe" peers 2>&1
Check ($LASTEXITCODE -eq 0) "kite peers through the service failed: $out"

# Everything TUN mode does, with the cut-down token of the installed service.
& (Join-Path $here 'tun-smoke.ps1') "$app\kite.exe" $RelayDev -Service
if ($LASTEXITCODE -ne 0 -and $null -ne $LASTEXITCODE) { Fail 'the smoke test against the service failed' }

# Uninstall.
Msiexec '/x' 'uninstall'
Check (-not (Get-Service Kite -ErrorAction SilentlyContinue)) 'the service is still there'
Check (-not (Test-Path $app)) 'Program Files\Kite is still there'
Check (-not (Test-Path $data)) '%ProgramData%\Kite is still there'
Check (-not (Test-Path $menu)) 'the Start menu entry is still there'
Check (-not (Get-LocalGroup -Name kite -ErrorAction SilentlyContinue)) 'the group kite is still there'
Check (-not ((Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Run').PSObject.Properties.Name -contains 'Kite')) 'the autostart entry is still there'
Check (-not (Get-NetAdapter -Name kite0 -ErrorAction SilentlyContinue)) 'the adapter is still there'
Check (@(Get-DnsClientNrptRule | Where-Object { $_.Comment -eq 'kite' }).Count -eq 0) 'the DNS rule is still there'
Write-Host 'uninstalled, nothing left'

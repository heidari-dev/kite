# Smoke test of TUN mode on Windows, with relay-dev standing in for the phone on the loopback
# address. It checks what kite changes on the system and that all of it is gone again, after a
# clean disconnect and after the daemon was killed. It does not move data through the tunnel: the
# relay runs on this machine, so its own connections would enter the tunnel and loop back to it;
# relay-dev refuses every target but the loopback for that reason, and anything this machine sends
# meanwhile reaches the tunnel, is refused and closed.
#
# Run it from an administrator prompt with wintun.dll next to kite.exe (scripts\fetch-wintun.ps1):
#
#   scripts\tun-smoke.ps1 target\release\kite.exe target\release\examples\relay-dev.exe
#
# It changes this machine's routes and DNS settings while it runs, and puts them back.
param(
    [Parameter(Mandatory = $true)][string]$Kite,
    [Parameter(Mandatory = $true)][string]$RelayDev
)

$ErrorActionPreference = 'Stop'
$Kite = (Resolve-Path $Kite).Path
$RelayDev = (Resolve-Path $RelayDev).Path

$admin = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $admin.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'run this from an administrator prompt'
}

$work = Join-Path ([IO.Path]::GetTempPath()) ("kite-smoke-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
$pipe = '\\.\pipe\kite-smoke'
$relay = '127.0.0.1:7000'
$procs = @()

function Fail([string]$Message) {
    foreach ($log in Get-ChildItem $work -Filter *.log -ErrorAction SilentlyContinue) {
        Write-Host "--- $($log.Name)"
        Get-Content $log.FullName
    }
    throw "FAIL: $Message"
}

function Start-Logged([string]$Exe, [string[]]$Arguments, [string]$Name) {
    $p = Start-Process -FilePath $Exe -ArgumentList $Arguments -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $work "$Name.log") -RedirectStandardError (Join-Path $work "$Name.err.log")
    $script:procs += $p
    $p
}

function Wait-For([string]$What, [scriptblock]$Condition, [int]$Seconds = 60) {
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 500
    }
    Fail "timed out waiting for $What"
}

# What TUN mode changes, as the system reports it.
function Adapter { Get-NetAdapter -Name kite0 -ErrorAction SilentlyContinue }
function Halves { @(Get-NetRoute -InterfaceAlias kite0 -ErrorAction SilentlyContinue | Where-Object { $_.DestinationPrefix -in '0.0.0.0/1', '128.0.0.0/1' }) }
function Rules { @(Get-DnsClientNrptRule | Where-Object { $_.Comment -eq 'kite' }) }
function Assert-Clean([string]$When) {
    if (Adapter) { Fail "${When}: the adapter kite0 is still there" }
    if ((Halves).Count -ne 0) { Fail "${When}: routes of the tunnel are still there" }
    if ((Rules).Count -ne 0) { Fail "${When}: the DNS rule is still there" }
    $to = (Find-NetRoute -RemoteIPAddress 1.1.1.1 | Where-Object { $_.InterfaceAlias } | Select-Object -First 1).InterfaceAlias
    if ($to -eq 'kite0') { Fail "${When}: traffic still goes to the tunnel" }
}
function Assert-Up {
    Wait-For 'the tunnel routes' { (Halves).Count -eq 2 -and (Rules).Count -eq 1 }
    if (-not (Adapter)) { Fail 'no adapter kite0' }
    if (-not (Get-NetIPAddress -InterfaceAlias kite0 -AddressFamily IPv4 | Where-Object IPAddress -eq '198.18.0.1')) { Fail 'kite0 has no address' }
    $to = (Find-NetRoute -RemoteIPAddress 1.1.1.1 | Where-Object { $_.InterfaceAlias } | Select-Object -First 1).InterfaceAlias
    if ($to -ne 'kite0') { Fail "1.1.1.1 is reached through $to, not through the tunnel" }
    $dns = (Get-DnsClientServerAddress -InterfaceAlias kite0 -AddressFamily IPv4).ServerAddresses
    if ($dns -notcontains '1.1.1.1') { Fail 'kite0 has no DNS server' }
}

function Kite([string[]]$Arguments) {
    & $Kite @Arguments --socket $pipe
    if ($LASTEXITCODE -ne 0) { Fail "kite $($Arguments -join ' ') exited with $LASTEXITCODE" }
}

try {
    Assert-Clean 'before the test'

    $relayProc = Start-Logged $RelayDev @($relay, '--refuse-external') 'relay'
    Wait-For 'relay-dev' { (Get-Content (Join-Path $work 'relay.log') -ErrorAction SilentlyContinue) -match 'listening' }
    $daemon = Start-Logged $Kite @('daemon', '--socket', $pipe, '--state-dir', (Join-Path $work 'state')) 'daemon'
    Wait-For 'the daemon' { (Get-Content (Join-Path $work 'daemon.err.log') -ErrorAction SilentlyContinue) -match 'daemon ready' }

    # Pairing: kite pair asks for the code the phone would show.
    $pair = [Diagnostics.Process]::new()
    $pair.StartInfo.FileName = $Kite
    $pair.StartInfo.Arguments = "pair --socket $pipe --tcp $relay"
    $pair.StartInfo.RedirectStandardInput = $true
    $pair.StartInfo.RedirectStandardOutput = $true
    $pair.StartInfo.UseShellExecute = $false
    $null = $pair.Start()
    $code = $null
    Wait-For 'the pairing code' {
        $m = [regex]::Match((Get-Content (Join-Path $work 'relay.log') -Raw), 'pairing code (\d{6})')
        if ($m.Success) { $script:code = $m.Groups[1].Value }
        $script:code
    }
    $pair.StandardInput.WriteLine($code)
    if (-not $pair.WaitForExit(30000) -or $pair.ExitCode -ne 0) { Fail 'pairing failed' }

    # A clean disconnect takes everything away.
    Kite @('connect', '--tcp', $relay, '--tun')
    Assert-Up
    Write-Host 'tunnel is up: routes, DNS rule, address and the route to 1.1.1.1 are as expected'
    Kite @('status')
    Kite @('disconnect')
    Wait-For 'the adapter to go' { -not (Adapter) -and (Halves).Count -eq 0 -and (Rules).Count -eq 0 } 30
    Assert-Clean 'after disconnect'
    Write-Host 'clean after disconnect'

    # A daemon that is killed leaves the DNS rule at most; restore takes the rest away.
    Kite @('connect', '--tcp', $relay, '--tun')
    Assert-Up
    Stop-Process -Id $daemon.Id -Force
    $daemon.WaitForExit()
    Start-Sleep -Seconds 2
    Write-Host ("after the kill: adapter={0} routes={1} rules={2}" -f [bool](Adapter), (Halves).Count, (Rules).Count)
    & $Kite restore
    if ($LASTEXITCODE -ne 0) { Fail "kite restore exited with $LASTEXITCODE" }
    Wait-For 'the adapter to go' { -not (Adapter) } 30
    Assert-Clean 'after restore'
    Write-Host 'clean after kill and restore'
}
finally {
    foreach ($p in $procs) { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
    & $Kite restore 2>$null
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

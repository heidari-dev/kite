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
# It also tests the kill switch: with the tunnel's routes taken away, as if the adapter were gone,
# a connection to 1.1.1.1 and a DNS query to 8.8.8.8 must fail while the kill switch is on and the
# relay is dead, and work again after disconnect and after the daemon was killed and `kite restore`
# ran. The test needs a working Internet connection for that.
#
# With -Service it tests the installed service Kite instead of a daemon of its own (the installer
# test does this): the service is killed once and must clean up after itself when it comes back.
#
# It changes this machine's routes, DNS settings and firewall filters while it runs, and puts them
# back.
param(
    [Parameter(Mandatory = $true)][string]$Kite,
    [Parameter(Mandatory = $true)][string]$RelayDev,
    [switch]$Service
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
$pipe = if ($Service) { '\\.\pipe\kite' } else { '\\.\pipe\kite-smoke' }
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

# A connection and a DNS query that do not use the tunnel's routes, to a server outside this machine.
function Reach {
    $c = [Net.Sockets.TcpClient]::new()
    try { $c.ConnectAsync('1.1.1.1', 443).Wait(5000) -and $c.Connected } catch { $false } finally { $c.Dispose() }
}
function Reach-Dns {
    [bool](Resolve-DnsName -Name example.com -Server 8.8.8.8 -DnsOnly -QuickTimeout -ErrorAction SilentlyContinue)
}
# As if the adapter were gone: the routes of the tunnel off, the rest of the tunnel as it was.
function Drop-TunnelRoutes {
    Get-NetRoute -InterfaceAlias kite0 -ErrorAction SilentlyContinue |
        Where-Object { $_.DestinationPrefix -in '0.0.0.0/1', '128.0.0.0/1', '::/1', '8000::/1' } |
        Remove-NetRoute -Confirm:$false
    Start-Sleep -Seconds 1
}
function Assert-Open([string]$When) {
    if (-not (Reach)) { Fail "${When}: a connection to 1.1.1.1 does not get out" }
    if (-not (Reach-Dns)) { Fail "${When}: a DNS query to 8.8.8.8 gets no answer" }
}
function Assert-Blocked([string]$When) {
    if (Reach) { Fail "${When}: a connection to 1.1.1.1 got out of the computer" }
    if (Reach-Dns) { Fail "${When}: a DNS query to 8.8.8.8 got out of the computer" }
}

function Start-Relay([string]$Name) {
    $p = Start-Logged $RelayDev @($relay, '--refuse-external') $Name
    Wait-For 'relay-dev' { (Get-Content (Join-Path $work "$Name.log") -ErrorAction SilentlyContinue) -match 'listening' }
    $p
}

# Pairing: kite pair asks for the code the phone would show.
function Pair([string]$Name) {
    $pair = [Diagnostics.Process]::new()
    $pair.StartInfo.FileName = $Kite
    $pair.StartInfo.Arguments = "pair --socket $pipe --tcp $relay"
    $pair.StartInfo.RedirectStandardInput = $true
    $pair.StartInfo.RedirectStandardOutput = $true
    $pair.StartInfo.UseShellExecute = $false
    $null = $pair.Start()
    $script:code = $null
    Wait-For 'the pairing code' {
        $m = [regex]::Match((Get-Content (Join-Path $work "$Name.log") -Raw), 'pairing code (\d{6})')
        if ($m.Success) { $script:code = $m.Groups[1].Value }
        $script:code
    }
    $pair.StandardInput.WriteLine($script:code)
    if (-not $pair.WaitForExit(30000) -or $pair.ExitCode -ne 0) { Fail 'pairing failed' }
}

function Kite([string[]]$Arguments) {
    & $Kite @Arguments --socket $pipe
    if ($LASTEXITCODE -ne 0) { Fail "kite $($Arguments -join ' ') exited with $LASTEXITCODE" }
}

try {
    Assert-Clean 'before the test'

    $relayProc = Start-Relay 'relay'
    if (-not $Service) {
        $daemon = Start-Logged $Kite @('daemon', '--socket', $pipe, '--state-dir', (Join-Path $work 'state')) 'daemon'
        Wait-For 'the daemon' { (Get-Content (Join-Path $work 'daemon.err.log') -ErrorAction SilentlyContinue) -match 'daemon ready' }
    }

    Pair 'relay'

    # A clean disconnect takes everything away.
    Kite @('connect', '--tcp', $relay, '--tun')
    Assert-Up
    Write-Host 'tunnel is up: routes, DNS rule, address and the route to 1.1.1.1 are as expected'
    Kite @('status')
    # Without the kill switch nothing stops a connection that does not use the tunnel's routes; this
    # is what the next part must see blocked.
    Drop-TunnelRoutes
    Assert-Open 'without the kill switch, tunnel routes gone'
    Kite @('disconnect')
    Wait-For 'the adapter to go' { -not (Adapter) -and (Halves).Count -eq 0 -and (Rules).Count -eq 0 } 30
    Assert-Clean 'after disconnect'
    Write-Host 'clean after disconnect'

    # The kill switch: the phone goes away, the tunnel's routes go too, and nothing may leave.
    Kite @('connect', '--tcp', $relay, '--tun', '--kill-switch')
    Assert-Up
    if (-not ((& $Kite status --socket $pipe) -match 'kill switch: on')) { Fail 'status does not show the kill switch' }
    Stop-Process -Id $relayProc.Id -Force
    Wait-For 'the tunnel to notice the lost relay' { (& $Kite status --socket $pipe) -match 'state: reconnecting' }
    Drop-TunnelRoutes
    Assert-Blocked 'kill switch on, relay gone'
    Write-Host 'kill switch: nothing gets out while the relay is gone'
    Kite @('disconnect')
    Wait-For 'the adapter to go' { -not (Adapter) -and (Halves).Count -eq 0 -and (Rules).Count -eq 0 } 30
    Assert-Clean 'after disconnect with the kill switch'
    Assert-Open 'after disconnect with the kill switch'
    Write-Host 'kill switch: lifted by disconnect'

    # A new relay, a new pairing.
    $relayProc = Start-Relay 'relay2'
    Pair 'relay2'

    # A daemon that is killed leaves the DNS rule at most; restore takes the rest away. The kill
    # switch's filters go with the process. The service is started again by the service manager, and
    # cleans up when it starts.
    Kite @('connect', '--tcp', $relay, '--tun', '--kill-switch')
    Assert-Up
    if ($Service) {
        $id = (Get-CimInstance Win32_Service -Filter "Name='Kite'").ProcessId
        Stop-Process -Id $id -Force
        Wait-For 'the service to run again' {
            $now = (Get-CimInstance Win32_Service -Filter "Name='Kite'").ProcessId
            $now -ne 0 -and $now -ne $id
        } 60
        Wait-For 'the service to clean up' { -not (Adapter) -and (Halves).Count -eq 0 -and (Rules).Count -eq 0 } 60
        Assert-Clean 'after the service restarted'
        Assert-Open 'after the service restarted'
        Write-Host 'clean after the service was killed and started again'
    }
    else {
        Stop-Process -Id $daemon.Id -Force
        $daemon.WaitForExit()
        Start-Sleep -Seconds 2
        Write-Host ("after the kill: adapter={0} routes={1} rules={2}" -f [bool](Adapter), (Halves).Count, (Rules).Count)
        & $Kite restore
        if ($LASTEXITCODE -ne 0) { Fail "kite restore exited with $LASTEXITCODE" }
        Wait-For 'the adapter to go' { -not (Adapter) } 30
        Assert-Clean 'after restore'
        Assert-Open 'after restore'
        Write-Host 'clean after kill and restore'
    }
}
finally {
    foreach ($p in $procs) { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
    & $Kite restore 2>$null
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

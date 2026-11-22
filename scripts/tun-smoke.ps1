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
# relay is dead, and work again after disconnect. A daemon that is killed leaves the block up (that
# is what it is for) until `kite restore` runs. Without the kill switch the same connection gets out,
# but the DNS query must not: the DNS guard blocks DNS out of a network card while TUN mode runs.
# The test needs a working Internet connection for that. Every filter Kite adds is checked to be no
# hard permit, which would override another program's block.
#
# With -FakeVpn (the fake-vpn example) it starts a stand-in for a second VPN on top of the tunnel,
# with its own default route, metric 0 and DNS server, as WireGuard has, and checks that the VPN
# wins the default route, that what a VPN client takes for its own way out is the tunnel, and that
# the tunnel's DNS server steps aside while the VPN is up and comes back when it goes.
#
# With -Service it tests the installed service Kite instead of a daemon of its own (the installer
# test does this): the service is killed once and must clean up after itself when it comes back.
#
# It changes this machine's routes, DNS settings and firewall filters while it runs, and puts them
# back.
param(
    [Parameter(Mandatory = $true)][string]$Kite,
    [Parameter(Mandatory = $true)][string]$RelayDev,
    [string]$FakeVpn,
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
function Defaults { @(Get-NetRoute -InterfaceAlias kite0 -ErrorAction SilentlyContinue | Where-Object { $_.DestinationPrefix -in '0.0.0.0/0', '::/0' }) }
function Defaults4 { @(Defaults | Where-Object { $_.DestinationPrefix -eq '0.0.0.0/0' }) }
# The catch-all DNS rule of versions up to 0.1.0-rc.2; this one never adds it.
function Rules { @(Get-DnsClientNrptRule | Where-Object { $_.Comment -eq 'kite' }) }
function Dns-Of([string]$Alias) { @((Get-DnsClientServerAddress -InterfaceAlias $Alias -AddressFamily IPv4 -ErrorAction SilentlyContinue).ServerAddresses) }
function Route-To([string]$Ip) { (Find-NetRoute -RemoteIPAddress $Ip | Where-Object { $_.InterfaceAlias } | Select-Object -First 1).InterfaceAlias }
# Kite's filters, as the filtering engine lists them.
function Kite-Filters {
    $file = Join-Path $work 'wfp.xml'
    netsh wfp show filters file=$file | Out-Null
    if ($LASTEXITCODE -ne 0) { Fail 'netsh wfp show filters failed' }
    $xml = [xml](Get-Content -Raw $file)
    @($xml.SelectNodes("//filters/item[starts-with(displayData/name, 'Kite ')]"))
}
function Assert-Filters([bool]$KillSwitch) {
    $filters = Kite-Filters
    $hard = @($filters | Where-Object { $_.SelectNodes('flags/item') | Where-Object { $_.InnerText -eq 'FWPM_FILTER_FLAG_CLEAR_ACTION_RIGHT' } })
    if ($hard.Count -ne 0) { Fail "Kite added $($hard.Count) hard permits, which override other programs' blocks" }
    $guard = @($filters | Where-Object { $_.displayData.name -eq 'Kite DNS guard' })
    if ($guard.Count -ne 2) { Fail "the DNS guard has $($guard.Count) filters, not 2" }
    if (@($guard | Where-Object { $_.action.type -ne 'FWP_ACTION_BLOCK' }).Count -ne 0) { Fail 'the DNS guard permits something' }
    $kill = @($filters | Where-Object { $_.displayData.name -eq 'Kite kill switch' })
    if ($KillSwitch -ne ($kill.Count -gt 0)) { Fail "kill switch filters: $($kill.Count)" }
}
function Assert-Clean([string]$When) {
    if (Adapter) { Fail "${When}: the adapter kite0 is still there" }
    if ((Defaults).Count -ne 0) { Fail "${When}: routes of the tunnel are still there" }
    if ((Rules).Count -ne 0) { Fail "${When}: a DNS rule is still there" }
    if ((Route-To 1.1.1.1) -eq 'kite0') { Fail "${When}: traffic still goes to the tunnel" }
}
function Assert-Up([bool]$KillSwitch) {
    Wait-For 'the tunnel routes' { (Defaults4).Count -eq 1 -and (Dns-Of kite0) -contains '1.1.1.1' }
    if (-not (Adapter)) { Fail 'no adapter kite0' }
    if (-not (Get-NetIPAddress -InterfaceAlias kite0 -AddressFamily IPv4 | Where-Object IPAddress -eq '198.18.0.1')) { Fail 'kite0 has no address' }
    # A VPN that copies the default gateway for the host route to its server needs one on kite0's /30.
    if ((Defaults4)[0].NextHop -ne '198.18.0.2') { Fail "the default route goes through $((Defaults4)[0].NextHop), not 198.18.0.2" }
    if ((Rules).Count -ne 0) { Fail 'kite added a catch-all DNS rule' }
    $to = Route-To 1.1.1.1
    if ($to -ne 'kite0') { Fail "1.1.1.1 is reached through $to, not through the tunnel" }
    Assert-Filters $KillSwitch
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
    Defaults | Remove-NetRoute -Confirm:$false
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

# The daemon's memory, for the pull request; the service is not measured.
function Write-Rss([string]$When) {
    if ($Service) { return }
    $p = Get-Process -Id $daemon.Id
    Write-Host ("daemon memory, {0}: working set {1:N0} KiB, private {2:N0} KiB" -f $When, ($p.WorkingSet64 / 1KB), ($p.PrivateMemorySize64 / 1KB))
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
    Assert-Up $false
    Write-Host 'tunnel is up: routes, DNS server, filters, address and the route to 1.1.1.1 are as expected'
    Kite @('status')
    Write-Rss 'tunnel up'

    if ($FakeVpn) {
        $vpn = Start-Logged (Resolve-Path $FakeVpn).Path @((Join-Path (Split-Path $Kite) 'wintun.dll')) 'vpn2'
        Wait-For 'the second VPN' { (Get-NetAdapter -Name vpn2 -ErrorAction SilentlyContinue) -and (Dns-Of vpn2) -contains '10.99.0.1' }
        Wait-For 'traffic to take the second VPN' { (Route-To 1.1.1.1) -eq 'vpn2' } 15
        # What WireGuard, OpenVPN and sing-box take for their own way out: the default route with
        # the lowest metric, their own adapter left out.
        $uplink = Get-NetRoute -AddressFamily IPv4 -DestinationPrefix 0.0.0.0/0 |
            Where-Object { $_.InterfaceAlias -ne 'vpn2' } |
            Sort-Object { $_.RouteMetric + (Get-NetIPInterface -InterfaceIndex $_.ifIndex -AddressFamily IPv4).InterfaceMetric } |
            Select-Object -First 1
        if ($uplink.InterfaceAlias -ne 'kite0' -or $uplink.NextHop -ne '198.18.0.2') {
            Fail "a VPN would leave through $($uplink.InterfaceAlias) via $($uplink.NextHop), not through the tunnel"
        }
        Wait-For 'kite0 to leave DNS to the second VPN' { (Dns-Of kite0).Count -eq 0 } 15
        Start-Sleep -Seconds 6
        Write-Rss 'second VPN up'
        Stop-Process -Id $vpn.Id -Force
        Wait-For 'kite0 to take DNS back' { (Dns-Of kite0) -contains '1.1.1.1' } 15
        Wait-For 'traffic to come back to the tunnel' { (Route-To 1.1.1.1) -eq 'kite0' } 15
        Write-Host 'second VPN: it carries the traffic, its way out is the tunnel, and DNS follows it'
    }

    # Without the kill switch nothing stops a connection that does not use the tunnel's routes; this
    # is what the next part must see blocked. DNS out of the network card stays blocked by the guard.
    Drop-TunnelRoutes
    if (-not (Reach)) { Fail 'without the kill switch, tunnel routes gone: a connection to 1.1.1.1 does not get out' }
    if (Reach-Dns) { Fail 'the DNS guard let a query to 8.8.8.8 out of the network card' }
    Kite @('disconnect')
    Wait-For 'the adapter to go' { -not (Adapter) -and (Defaults).Count -eq 0 } 30
    Assert-Clean 'after disconnect'
    if ((Kite-Filters).Count -ne 0) { Fail 'filters are left after disconnect' }
    Assert-Open 'after disconnect'
    Write-Host 'clean after disconnect'

    # The kill switch: the phone goes away, the tunnel's routes go too, and nothing may leave.
    Kite @('connect', '--tcp', $relay, '--tun', '--kill-switch')
    Assert-Up $true
    if (-not ((& $Kite status --socket $pipe) -match 'kill switch: on')) { Fail 'status does not show the kill switch' }
    Stop-Process -Id $relayProc.Id -Force
    Wait-For 'the tunnel to notice the lost relay' { (& $Kite status --socket $pipe) -match 'state: reconnecting' }
    Drop-TunnelRoutes
    Assert-Blocked 'kill switch on, relay gone'
    Write-Host 'kill switch: nothing gets out while the relay is gone'
    Kite @('disconnect')
    Wait-For 'the adapter to go' { -not (Adapter) -and (Defaults).Count -eq 0 } 30
    Assert-Clean 'after disconnect with the kill switch'
    Assert-Open 'after disconnect with the kill switch'
    Write-Host 'kill switch: lifted by disconnect'

    # A new relay, a new pairing.
    $relayProc = Start-Relay 'relay2'
    Pair 'relay2'

    # A daemon that is killed leaves the DNS guard and the kill switch's block behind; restore takes
    # them away. The service is started again by the service manager, and cleans up when it starts,
    # except for the block.
    Kite @('connect', '--tcp', $relay, '--tun', '--kill-switch')
    Assert-Up $true
    if ($Service) {
        $id = (Get-CimInstance Win32_Service -Filter "Name='Kite'").ProcessId
        Stop-Process -Id $id -Force
        Wait-For 'the service to run again' {
            $now = (Get-CimInstance Win32_Service -Filter "Name='Kite'").ProcessId
            $now -ne 0 -and $now -ne $id
        } 60
        Wait-For 'the service to clean up' { -not (Adapter) -and (Defaults).Count -eq 0 } 60
        Assert-Clean 'after the service restarted'
        Assert-Blocked 'after the service was killed with the kill switch on'
        & $Kite restore
        if ($LASTEXITCODE -ne 0) { Fail "kite restore exited with $LASTEXITCODE" }
        Assert-Open 'after restore'
        Write-Host 'clean after the service was killed and started again'
    }
    else {
        Stop-Process -Id $daemon.Id -Force
        $daemon.WaitForExit()
        Start-Sleep -Seconds 2
        Write-Host ("after the kill: adapter={0} routes={1}" -f [bool](Adapter), (Defaults).Count)
        Assert-Blocked 'after the daemon was killed with the kill switch on'
        & $Kite restore
        if ($LASTEXITCODE -ne 0) { Fail "kite restore exited with $LASTEXITCODE" }
        Wait-For 'the adapter to go' { -not (Adapter) } 30
        Assert-Clean 'after restore'
        Assert-Open 'after restore'
        Write-Host 'clean after kill and restore'
    }
    if ((Kite-Filters).Count -ne 0) { Fail 'filters are left after restore' }

    # What 0.1.0-rc.2 left when it was killed: its state file and its catch-all DNS rule. The next
    # start or restore takes the rule away.
    Set-Content -Path (Join-Path $env:ProgramData 'Kite\tun.state') -Value '' -NoNewline
    Add-DnsClientNrptRule -Namespace '.' -NameServers '1.1.1.1' -Comment 'kite'
    & $Kite restore
    if ($LASTEXITCODE -ne 0) { Fail "kite restore exited with $LASTEXITCODE" }
    if ((Rules).Count -ne 0) { Fail 'restore left the DNS rule of an earlier version' }
    if (Test-Path (Join-Path $env:ProgramData 'Kite\tun.state')) { Fail 'restore left the state of an earlier version' }
    Write-Host 'clean after what an earlier version left'
}
finally {
    foreach ($p in $procs) { if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } }
    & $Kite restore 2>$null
    Get-DnsClientNrptRule | Where-Object { $_.Comment -eq 'kite' } | Remove-DnsClientNrptRule -Force
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

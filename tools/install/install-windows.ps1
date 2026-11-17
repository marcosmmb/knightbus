# Install or upgrade KnightBus and its bundled signed USB/IP driver.
param([switch]$StartUiOnLogin)
$ErrorActionPreference='Stop'
$ProgressPreference='SilentlyContinue'
. (Join-Path $PSScriptRoot 'windows-installation.ps1')
$shutdownTimeoutSeconds=30
$startupTimeoutSeconds=15
$servicePollMs=250
$mdnsPort=5353
$taskName='KnightBus daemon'
$firewallRule='KnightBus-LAN-discovery'
$pairingFirewallRule='KnightBus-PIN-pairing'
$endpoint='\\.\pipe\knightbus-control'
$install=Join-Path $env:ProgramFiles 'KnightBus'
$state=Join-Path $env:LOCALAPPDATA 'KnightBus'
$ownerFile=Join-Path $install 'owner-sid.txt'
$cli=Join-Path $install 'knightbus.exe'
$daemonPath=Join-Path $install 'knightbusd.exe'
$daemonArguments="--desktop `"$state\identity`" $endpoint"
$files=@(Get-KnightBusApplicationFiles)

function Invoke-Control([string[]]$Arguments) {
 $result=& $cli manage $endpoint @Arguments
 if ($LASTEXITCODE -ne 0) {throw "Cannot query the existing service ($Arguments). Preserve this installation and inspect its status."}
 $result
}
function Get-InstalledDaemon {
 Get-Process knightbusd -ErrorAction SilentlyContinue | Where-Object {$_.Path -eq $daemonPath}
}

if (![Environment]::Is64BitProcess) {throw 'Use 64-bit PowerShell for this x64 package.'}
$identity=[Security.Principal.WindowsIdentity]::GetCurrent()
if (!([Security.Principal.WindowsPrincipal]$identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
 throw 'Approve setup with the same administrator account that uses KnightBus.'
}
foreach ($library in @('vcruntime140.dll','vcruntime140_1.dll')) {
 if (!(Test-Path (Join-Path $env:WINDIR "System32\$library"))) {throw 'Install Microsoft Visual C++ v14 Redistributable (x64), then rerun setup. See INSTALL.md.'}
}
foreach ($line in Get-Content (Join-Path $PSScriptRoot 'SHA256SUMS')) {
 $hash,$relative=$line -split '  ',2
 if ((Get-FileHash -LiteralPath (Join-Path $PSScriptRoot $relative) -Algorithm SHA256).Hash -ine $hash) {throw "Package integrity failed: $relative"}
}
$webview=Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\*','HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\*' -ErrorAction SilentlyContinue | Where-Object {$_.name -eq 'Microsoft Edge WebView2 Runtime'}
if (!$webview) {throw 'Install Microsoft Edge WebView2 Runtime, then rerun setup. See INSTALL.md.'}

$task=Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
$upgrading=(Test-Path $install) -or ($null -ne $task)
if (Test-Path $install) {
 if ((Get-Item $install).Attributes -band [IO.FileAttributes]::ReparsePoint) {throw 'The installation directory must not be a link.'}
 if (!(Test-Path $ownerFile) -and !$task -and (Get-ChildItem $install -Force)) {
  if (!(Test-RetainedDriverDirectory -Install $install -PackageDriver (Join-Path $PSScriptRoot 'driver'))) {
   throw "The existing folder contains files that cannot be identified as a previous KnightBus installation: $install. Its contents were preserved."
  }
 }
}
if ((Test-Path $ownerFile) -and (Get-Content $ownerFile -Raw).Trim() -ne $identity.User.Value) {
 throw 'Run this upgrade using the Windows account that originally installed KnightBus.'
}
if ($task) {
 Assert-KnightBusTask $task $identity.User.Value $daemonPath $daemonArguments
}
$gui=Get-Process knightbus-desktop -ErrorAction SilentlyContinue | Where-Object {$_.Path -eq "$install\knightbus-desktop.exe"}
if ($gui) {throw 'Choose Quit UI from the KnightBus tray menu, then run this installer again. You do not need to uninstall.'}

$wasRunning=($null -ne (Get-InstalledDaemon)) -or ($task -and $task.State -eq 'Running')
if ($wasRunning) {
 # Never replace a service still holding native USB resources.
 foreach ($session in @(Invoke-Control -Arguments @('sessions'))) {
  if (!$session.Trim()) {continue}
  if ($session -notmatch '^epoch=(\d+) serial=(\d+)$') {throw 'Cannot verify existing USB sessions. Installation was not changed.'}
  $sessionStatus=(Invoke-Control -Arguments @('status',$Matches[1],$Matches[2])) -join "`n"
  if ($sessionStatus -notmatch '\bstate=Closed\b') {
   throw 'Disconnect USB devices and wait for Disconnected in Computers, then quit the tray app and rerun setup.'
  }
 }
}

$taskDisabled=$false
$backup=$null
$installed=$false
try {
 if ($task) {Disable-ScheduledTask -TaskName $taskName | Out-Null; $taskDisabled=$true}
 if ($wasRunning) {
  Invoke-Control -Arguments @('shutdown') | Out-Null
  $deadline=(Get-Date).AddSeconds($shutdownTimeoutSeconds)
  while ((Get-InstalledDaemon) -or ($task -and (Get-ScheduledTask -TaskName $taskName).State -eq 'Running')) {
   if ((Get-Date) -ge $deadline) {throw 'Service is still releasing devices. No files were replaced; wait before retrying.'}
   Start-Sleep -Milliseconds $servicePollMs
  }
 }
 New-Item -ItemType Directory -Force $install | Out-Null
 # Record validated ownership before setup can leave a partial installation.
 $identity.User.Value | Set-Content $ownerFile
 # The original signed package is required; setup never changes signing policy.
 & (Join-Path $PSScriptRoot 'install-driver.ps1')
 # Backups inherit Program Files permissions; pairing state stays outside this folder.
 $backup=Join-Path $install ('.upgrade-backup-'+[Guid]::NewGuid().ToString('N'))
 New-Item -ItemType Directory $backup | Out-Null
 foreach ($file in $files) {
  if (Test-Path "$install\$file") {Copy-Item -LiteralPath "$install\$file" -Destination $backup}
 }
 try {
  foreach ($file in $files) {Copy-Item -LiteralPath "$PSScriptRoot\$file" -Destination "$install\$file" -Force}
 } catch {
  # Restore the previous binaries if a file could not be replaced.
  foreach ($file in $files) {
   if (Test-Path "$backup\$file") {Copy-Item -LiteralPath "$backup\$file" -Destination "$install\$file" -Force}
   elseif (Test-Path "$install\$file") {Remove-Item -LiteralPath "$install\$file"}
  }
  if ($task -and $wasRunning) {Enable-ScheduledTask -TaskName $taskName | Out-Null; Start-ScheduledTask -TaskName $taskName}
  throw
 }
 # Keep notices and corresponding source with the installed driver dependency.
 $installedDriver=Join-Path $install 'driver'
 New-Item -ItemType Directory -Force $installedDriver | Out-Null
 Get-ChildItem -LiteralPath (Join-Path $PSScriptRoot 'driver') -File | Copy-Item -Destination $installedDriver -Force
 New-Item -ItemType Directory -Force $state | Out-Null
 $action=New-ScheduledTaskAction -Execute $daemonPath -Argument $daemonArguments
 $trigger=New-ScheduledTaskTrigger -AtLogOn -User $identity.User.Value
 $principal=New-KnightBusTaskPrincipal -UserSid $identity.User.Value
 $settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
 Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force -Description 'KnightBus native service; independent of the optional desktop UI.' | Out-Null
 $taskDisabled=$false
 $shell=New-Object -ComObject WScript.Shell
 $shortcutPath=Join-Path ([Environment]::GetFolderPath('Programs')) 'KnightBus.lnk'
 $shortcut=$shell.CreateShortcut($shortcutPath)
 $shortcut.TargetPath="$install\knightbus-desktop.exe"
 $shortcut.WorkingDirectory=$install
 $shortcut.Save()
 if ($StartUiOnLogin) {Copy-Item $shortcutPath ([Environment]::GetFolderPath('Startup')) -Force}
 Get-NetFirewallRule -Name $firewallRule -ErrorAction SilentlyContinue | Remove-NetFirewallRule
 New-NetFirewallRule -Name $firewallRule -DisplayName 'KnightBus LAN discovery' -Direction Inbound -Action Allow -Program $daemonPath -Protocol UDP -LocalPort $mdnsPort -RemoteAddress LocalSubnet -Profile Private | Out-Null
 Get-NetFirewallRule -Name $pairingFirewallRule -ErrorAction SilentlyContinue | Remove-NetFirewallRule
 # The ephemeral listener exists only during an explicit PIN pairing window.
 New-NetFirewallRule -Name $pairingFirewallRule -DisplayName 'KnightBus PIN pairing' -Direction Inbound -Action Allow -Program $daemonPath -Protocol TCP -RemoteAddress LocalSubnet -Profile Private | Out-Null
 Start-ScheduledTask -TaskName $taskName
 $deadline=(Get-Date).AddSeconds($startupTimeoutSeconds)
 do {
  $ready=$false
  try {Invoke-Control -Arguments @('host-status') | Out-Null; $ready=$true} catch { }
  if ($ready) {break}
  if ((Get-Date) -ge $deadline) {throw "The installed service did not become ready. See INSTALL.md. Previous files are retained in $backup."}
  Start-Sleep -Milliseconds $servicePollMs
 } while ($true)
 $installed=$true
} finally {
 if ($taskDisabled) {Enable-ScheduledTask -TaskName $taskName | Out-Null}
 if ($installed -and $backup) {Remove-Item -LiteralPath $backup -Recurse}
}
if ($upgrading) {'KnightBus updated. Identity and paired computers were retained.'}
else {'KnightBus installed.'}
'Open KnightBus from Start. The signed USB/IP driver is installed.'

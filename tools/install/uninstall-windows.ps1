$ErrorActionPreference='Stop'
$ProgressPreference='SilentlyContinue'
. (Join-Path $PSScriptRoot 'windows-installation.ps1')
$install=Join-Path $env:ProgramFiles 'KnightBus'
$taskName='KnightBus daemon'
$endpoint='\\.\pipe\knightbus-control'
$state=Join-Path $env:LOCALAPPDATA 'KnightBus'
$daemonPath=Join-Path $install 'knightbusd.exe'
$daemonArguments="--desktop `"$state\identity`" $endpoint"
$owner=(Get-Content (Join-Path $install 'owner-sid.txt') -Raw).Trim()
if ([Security.Principal.WindowsIdentity]::GetCurrent().User.Value -ne $owner) {
 throw 'Uninstall using the Windows account that installed KnightBus.'
}
$gui=Get-Process knightbus-desktop -ErrorAction SilentlyContinue | Where-Object {$_.Path -eq "$install\knightbus-desktop.exe"}
if ($gui) {throw 'Choose Quit UI from the KnightBus tray menu, then retry uninstall.'}
$task=Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($task) {Assert-KnightBusTask $task $owner $daemonPath $daemonArguments}
if ($task -and $task.State -eq 'Running') {
 # Successful shutdown must retire native resources before removing executable files.
 & "$install\knightbus.exe" manage '\\.\pipe\knightbus-control' shutdown
 if ($LASTEXITCODE -ne 0) {throw 'Could not confirm service shutdown. Preserve the installation and inspect connections.'}
 $shutdownWaitSeconds=30
 $shutdownPollMs=250
 $deadline=(Get-Date).AddSeconds($shutdownWaitSeconds)
 while ((Get-ScheduledTask -TaskName $taskName).State -eq 'Running') {
  if ((Get-Date) -ge $deadline) {throw 'Service is still releasing devices. Wait before retrying uninstall.'}
  Start-Sleep -Milliseconds $shutdownPollMs
 }
}
$daemon=Get-Process knightbusd -ErrorAction SilentlyContinue | Where-Object {$_.Path -eq "$install\knightbusd.exe"}
if ($daemon) {throw 'An installed companion is still running. Shut it down gracefully before uninstalling.'}
$task=Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if ($task) {
 Assert-KnightBusTask $task $owner $daemonPath $daemonArguments
 Unregister-ScheduledTask -TaskName $taskName -TaskPath $task.TaskPath -Confirm:$false
}
Get-NetFirewallRule -Name 'KnightBus-LAN-discovery','KnightBus-PIN-pairing' -ErrorAction SilentlyContinue | Remove-NetFirewallRule
Remove-Item (Join-Path ([Environment]::GetFolderPath('Programs')) 'KnightBus.lnk') -ErrorAction SilentlyContinue
Remove-Item (Join-Path ([Environment]::GetFolderPath('Startup')) 'KnightBus.lnk') -ErrorAction SilentlyContinue
# Retain the account marker with the shared driver so reinstall can identify it.
# NSIS removes its own executable after this script exits.
Remove-KnightBusApplicationFiles $install
'Applications removed. Pairing state and the separate USB driver were retained.'

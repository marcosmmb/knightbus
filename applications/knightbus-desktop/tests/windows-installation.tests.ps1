# Filesystem and task-owner checks; no driver, elevation or prepared machine.
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot '../../../tools/install/windows-installation.ps1')
$root=Join-Path ([IO.Path]::GetTempPath()) ('knightbus-installer-test-'+[Guid]::NewGuid().ToString('N'))
$install=Join-Path $root 'install'
$package=Join-Path $root 'package'
try {
 New-Item -ItemType Directory -Path "$install\driver",$package -Force | Out-Null
 foreach ($name in @('usbip2_ude.sys','usbip2_ude.inf','usbip2_ude.cat','usbip2_filter.sys','usbip2_filter.inf','usbip2_filter.cat','COPYING','NOTICE.txt','usbip-win2-source.tar.gz')) {
  [IO.File]::WriteAllText((Join-Path $package $name),"fixture $name")
  Copy-Item (Join-Path $package $name) "$install\driver"
 }
 if (!(Test-RetainedDriverDirectory $install $package)) {throw 'Matching retained driver was rejected'}
 [IO.File]::WriteAllText("$install\notes.txt",'Keep this file')
 if (Test-RetainedDriverDirectory $install $package) {throw 'Unknown folder contents were accepted'}
 Remove-Item "$install\notes.txt"
 [IO.File]::WriteAllText("$install\driver\usbip2_ude.sys",'modified')
 if (Test-RetainedDriverDirectory $install $package) {throw 'Modified driver was accepted'}
 Copy-Item "$package\usbip2_ude.sys" "$install\driver" -Force
 Remove-Item "$install\driver\NOTICE.txt"
 if (Test-RetainedDriverDirectory $install $package) {throw 'Incomplete driver package was accepted'}
 Copy-Item "$package\NOTICE.txt" "$install\driver"
 # A junction must never let setup adopt dependency files outside its folder.
 Rename-Item "$install\driver" 'original-driver'
 $outside=Join-Path $root 'outside'
 Move-Item "$install\original-driver" $outside
 New-Item -ItemType Junction -Path "$install\driver" -Target $outside | Out-Null
 if (Test-RetainedDriverDirectory $install $package) {throw 'Linked dependency directory was accepted'}
 [IO.Directory]::Delete("$install\driver")
 'PASS retained driver recovery, unknown files, modified/missing dependencies and junction rejection.'
} finally {
 Remove-Item -LiteralPath $root -Recurse -Force
}

# Exported task identity must win over an unresolvable display name.
$identity=[Security.Principal.WindowsIdentity]::GetCurrent()
$missingAccount="$env:COMPUTERNAME\KnightBus-missing-$([Guid]::NewGuid().ToString('N'))"
try {
 ([Security.Principal.NTAccount]$missingAccount).Translate([Security.Principal.SecurityIdentifier]) | Out-Null
 throw 'Fixture account unexpectedly exists'
} catch {
 if ($_.Exception.GetBaseException() -isnot [Security.Principal.IdentityNotMappedException]) {throw}
}
$task=[pscustomobject]@{TaskName='KnightBus fixture';TaskPath='\';Principal=[pscustomobject]@{UserId=$missingAccount}}
function Export-ScheduledTask {
 [CmdletBinding()]
 param([string]$TaskName,[string]$TaskPath)
 if ($TaskName -ne $task.TaskName -or $TaskPath -ne $task.TaskPath) {throw 'Wrong task exported'}
 $script:taskDefinition
}
function Set-TaskFixture([string]$Principals) {
 $script:taskDefinition='<Task xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task"><Principals>'+ $Principals +'</Principals></Task>'
}
function Get-FixtureOwner {Get-KnightBusTaskOwnerSid -TaskName $task.TaskName -TaskPath $task.TaskPath}
try {
 Set-TaskFixture "<Principal><UserId>$($identity.User.Value)</UserId></Principal>"
 if ((Get-FixtureOwner) -ne $identity.User.Value) {throw 'Canonical task SID was not retained'}
 Set-TaskFixture '<Principal><UserId>S-1-5-18</UserId></Principal>'
 if ((Get-FixtureOwner) -eq $identity.User.Value) {throw 'Foreign task owner accepted as the current user'}
 foreach ($principals in @(
  '<Principal><GroupId>S-1-5-32-544</GroupId></Principal>',
  '<Principal><UserId></UserId></Principal>',
  '<Principal><UserId>S-1-invalid</UserId></Principal>',
  "<Principal><UserId>$missingAccount</UserId></Principal>",
  "<Principal><UserId>$($identity.User.Value)</UserId></Principal><Principal><UserId>S-1-5-18</UserId></Principal>"
 )) {
  Set-TaskFixture $principals
  $rejected=$false
  try {Get-FixtureOwner | Out-Null} catch {$rejected=$true}
  if (!$rejected) {throw 'Unverifiable task owner was accepted'}
 }
 Set-TaskFixture "<Principal><UserId>$($identity.User.Value)</UserId></Principal>"
 $daemonPath='C:\Program Files\KnightBus\knightbusd.exe'
 $arguments='--desktop "C:\Users\fixture\identity" \\.\pipe\knightbus-control'
 $validAction=[pscustomobject]@{Execute=$daemonPath;Arguments=$arguments}
 $task | Add-Member Actions @($validAction)
 Assert-KnightBusTask $task $identity.User.Value $daemonPath $arguments
 foreach ($actions in @(
  @([pscustomobject]@{Execute='C:\another.exe';Arguments=$arguments}),
  @([pscustomobject]@{Execute=$daemonPath;Arguments='--another-installation'}),
  @($validAction,$validAction)
 )) {
  $task.Actions=$actions
  $rejected=$false
  try {Assert-KnightBusTask $task $identity.User.Value $daemonPath $arguments} catch {$rejected=$true}
  if (!$rejected) {throw 'Reassigned task payload was accepted'}
 }
 $task.Actions=@($validAction)
 Set-TaskFixture '<Principal><UserId>S-1-5-18</UserId></Principal>'
 $rejected=$false
 try {Assert-KnightBusTask $task $identity.User.Value $daemonPath $arguments} catch {$rejected=$true}
 if (!$rejected) {throw 'Reassigned task owner was accepted'}
 'PASS task owner, executable, arguments and action count verified before removal.'
 'PASS canonical SID with unresolvable display name, foreign SID, missing/group/invalid/ambiguous owners.'
} finally {Remove-Item Function:\Export-ScheduledTask}

# Exercise the real CIM constructor: it can normalize a SID back to a short name.
# Registration must receive the unambiguous SID, without needing elevation.
$principal=New-KnightBusTaskPrincipal -UserSid $identity.User.Value
if ($principal.UserId -cne $identity.User.Value) {throw 'Task principal lost its canonical SID'}
if ($principal.LogonType -ne 'Interactive' -or $principal.RunLevel -ne 'Highest') {
 throw 'Task principal changed the service logon policy'
}
'PASS task registration preserves the SID and interactive elevated logon policy.'

$root=Join-Path ([IO.Path]::GetTempPath()) ('knightbus-uninstall-test-'+[Guid]::NewGuid().ToString('N'))
try {
 New-Item -ItemType Directory -Path $root | Out-Null
 foreach ($name in @(Get-KnightBusApplicationFiles)+@('custom.conf','diagnostics.json','owner-sid.txt','Uninstall.exe')) {
  [IO.File]::WriteAllText((Join-Path $root $name),"fixture $name")
 }
 Remove-KnightBusApplicationFiles $root
 $retained=@(Get-ChildItem -LiteralPath $root -File | Select-Object -ExpandProperty Name | Sort-Object)
 $expected=@('custom.conf','diagnostics.json','owner-sid.txt','Uninstall.exe') | Sort-Object
 if (Compare-Object $retained $expected) {throw 'Uninstall did not preserve unrelated files and ownership records'}
 'PASS uninstall deletes only installed application files.'
} finally {Remove-Item -LiteralPath $root -Recurse -Force}

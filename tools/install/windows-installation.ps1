# The constructor rewrites a SID to a short name, which can be ambiguous when
# the account and computer have the same name. Restore the SID before registering.
function New-KnightBusTaskPrincipal([string]$UserSid) {
 $sid=([Security.Principal.SecurityIdentifier]$UserSid).Value
 $principal=New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Highest
 $principal.UserId=$sid
 return $principal
}

# Get-ScheduledTask may expose a short display name that Windows cannot resolve.
# The exported task retains the registered principal, normally already a SID.
function Get-KnightBusTaskOwnerSid([string]$TaskName, [string]$TaskPath) {
 [xml]$definition=Export-ScheduledTask -TaskName $TaskName -TaskPath $TaskPath -ErrorAction Stop
 $namespaces=New-Object System.Xml.XmlNamespaceManager($definition.NameTable)
 $namespaces.AddNamespace('task','http://schemas.microsoft.com/windows/2004/02/mit/task')
 $principals=$definition.SelectNodes('/task:Task/task:Principals/task:Principal',$namespaces)
 if ($principals.Count -ne 1) {throw 'Cannot verify the existing KnightBus task owner. The task was not changed.'}
 $user=$principals[0].SelectSingleNode('task:UserId',$namespaces)
 if (!$user -or [string]::IsNullOrWhiteSpace($user.InnerText)) {throw 'The existing KnightBus task has no user owner. The task was not changed.'}
 $owner=$user.InnerText.Trim()
 try {
  if ($owner.StartsWith('S-1-')) {return ([Security.Principal.SecurityIdentifier]$owner).Value}
  return ([Security.Principal.NTAccount]$owner).Translate([Security.Principal.SecurityIdentifier]).Value
 } catch {
  throw 'Windows cannot resolve the account registered for the KnightBus task. Use the original Windows account or repair that task before retrying. The task was not changed.'
 }
}

# Recognize only the dependency-only folder left by older uninstallers.
function Test-RetainedDriverDirectory([string]$Install, [string]$PackageDriver) {
 $entries=@(Get-ChildItem -LiteralPath $Install -Force -ErrorAction Stop)
 if ($entries.Count -ne 1 -or $entries[0].Name -ne 'driver' -or !$entries[0].PSIsContainer) {return $false}
 $driver=$entries[0]
 if ($driver.Attributes -band [IO.FileAttributes]::ReparsePoint) {return $false}
 $retained=@(Get-ChildItem -LiteralPath $driver.FullName -Force -ErrorAction Stop)
 $expected=@(Get-ChildItem -LiteralPath $PackageDriver -Force -File -ErrorAction Stop)
 if (!$expected.Count -or $retained.Count -ne $expected.Count) {return $false}
 foreach ($file in $retained) {
  if ($file.PSIsContainer -or ($file.Attributes -band [IO.FileAttributes]::ReparsePoint)) {return $false}
  $match=$expected | Where-Object {$_.Name -ceq $file.Name}
  if (!$match -or (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $match.FullName -Algorithm SHA256).Hash) {return $false}
 }
 return $true
}

# Shared allowlist: setup owns these files, never arbitrary user-added files.
function Get-KnightBusApplicationFiles {
 @('knightbus-desktop.exe','knightbus.exe','knightbusd.exe','LICENSE','INSTALL.md','standalone.md','architecture.md','windows-installation.ps1','uninstall-windows.ps1')
}
function Remove-KnightBusApplicationFiles([string]$Install) {
 foreach ($name in Get-KnightBusApplicationFiles) {
  $path=Join-Path $Install $name
  if (Test-Path -LiteralPath $path -PathType Leaf) {Remove-Item -LiteralPath $path}
 }
}
function Assert-KnightBusTask($Task,[string]$OwnerSid,[string]$DaemonPath,[string]$DaemonArguments) {
 $owner=Get-KnightBusTaskOwnerSid -TaskName $Task.TaskName -TaskPath $Task.TaskPath
 if ($owner -ne $OwnerSid -or @($Task.Actions).Count -ne 1 -or $Task.Actions[0].Execute -ne $DaemonPath -or $Task.Actions[0].Arguments -ne $DaemonArguments) {
  throw 'The existing KnightBus task belongs to another account or installation. It was not changed.'
 }
}

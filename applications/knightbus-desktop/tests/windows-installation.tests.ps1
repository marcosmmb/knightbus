# Pure filesystem checks; no driver, elevated privileges or prepared machine.
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot '../../../tools/install/windows-installation.ps1')
$root=Join-Path ([IO.Path]::GetTempPath()) ('knightbus-installer-test-'+[Guid]::NewGuid().ToString('N'))
$install=Join-Path $root 'install'
$package=Join-Path $root 'package'
try {
 New-Item -ItemType Directory -Path "$install\driver",$package -Force | Out-Null
 foreach ($name in @('usbip_vhci_ude.sys','usbip_vhci_ude.inf','usbip_vhci_ude.cat','COPYING','NOTICE.txt','usbip-win-source.tar.gz')) {
  [IO.File]::WriteAllText((Join-Path $package $name),"fixture $name")
  Copy-Item (Join-Path $package $name) "$install\driver"
 }
 if (!(Test-RetainedDriverDirectory $install $package)) {throw 'Matching retained driver was rejected'}
 [IO.File]::WriteAllText("$install\notes.txt",'Keep this file')
 if (Test-RetainedDriverDirectory $install $package) {throw 'Unknown folder contents were accepted'}
 Remove-Item "$install\notes.txt"
 [IO.File]::WriteAllText("$install\driver\usbip_vhci_ude.sys",'modified')
 if (Test-RetainedDriverDirectory $install $package) {throw 'Modified driver was accepted'}
 Copy-Item "$package\usbip_vhci_ude.sys" "$install\driver" -Force
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

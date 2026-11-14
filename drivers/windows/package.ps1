# Development signing only. Uses an existing certificate; never changes trust,
# Secure Boot, test-signing or driver installation state.
param([Parameter(Mandatory=$true)][string]$CertificateThumbprint)
$ErrorActionPreference = 'Stop'
$cert = Get-Item "Cert:\LocalMachine\My\$CertificateThumbprint"
if (!$cert.HasPrivateKey -or $cert.NotAfter -le [DateTime]::Now) { throw 'Signing certificate unavailable or expired' }
$kit = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10'
$package = Join-Path $PSScriptRoot 'target\package'
New-Item -ItemType Directory -Force $package | Out-Null
Copy-Item "$PSScriptRoot\KnightBusUde.inf" $package
Copy-Item "$PSScriptRoot\target\x86_64-pc-windows-msvc\debug\knightbus_windows_driver.dll" "$package\KnightBusUde.sys"
function Step([string]$Tool, [string[]]$Arguments) {
    & $Tool @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Tool exited $LASTEXITCODE" }
}
$sign = "$kit\bin\10.0.28000.0\x64\signtool.exe"
Step "$kit\Tools\10.0.28000.0\x64\InfVerif.exe" @('/w',"$package\KnightBusUde.inf")
Step $sign @('sign','/fd','SHA256','/sm','/s','My','/sha1',$CertificateThumbprint,"$package\KnightBusUde.sys")
Step "$kit\bin\10.0.28000.0\x86\Inf2Cat.exe" @("/driver:$package",'/os:10_NI_X64')
Step $sign @('sign','/fd','SHA256','/sm','/s','My','/sha1',$CertificateThumbprint,"$package\KnightBusUde.cat")
Step $sign @('verify','/pa',"$package\KnightBusUde.cat")
foreach ($name in @('KnightBusUde.inf','KnightBusUde.sys')) {
    Step $sign @('verify','/pa','/c',"$package\KnightBusUde.cat","$package\$name")
}
Get-ChildItem $package -File | Get-FileHash -Algorithm SHA256

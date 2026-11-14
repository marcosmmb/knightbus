# Compile/link only. Does not sign, install or load the driver.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vs = @(& $vswhere -latest -prerelease -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
if ($vs.Count -ne 1) { throw 'Expected one selected Visual C++ installation' }
& (Join-Path $vs[0] 'Common7\Tools\Launch-VsDevShell.ps1') -Arch amd64 -HostArch amd64 -SkipAutomaticLocation | Out-Host
if ($env:WindowsSDKVersion.TrimEnd('\') -ne '10.0.28000.0') { throw 'Revalidate a changed SDK before building this driver' }
$env:WDKContentRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10'
$env:LIBCLANG_PATH = Join-Path $env:ProgramFiles 'LLVM\bin'
$env:KNIGHTBUS_UDE_STUB_DIRECTORY = Join-Path $env:WDKContentRoot 'Lib\10.0.28000.0\km\x64\ude\1.1'
$env:CARGO_TARGET_DIR = Join-Path $PSScriptRoot 'target'
$env:RUSTFLAGS = '-C target-feature=+crt-static'
if ($env:CARGO_ENCODED_RUSTFLAGS) { throw 'Unexpected encoded Rust flags; review build environment' }
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
& $cargo +1.97.1 build --offline --locked --target x86_64-pc-windows-msvc --manifest-path (Join-Path $PSScriptRoot 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$image = Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc\debug\knightbus_windows_driver.dll'
Get-FileHash $image -Algorithm SHA256 | Format-List
# Kernel stack regression check. Each original function has a 4 KiB frame cap;
# native nested call-chain limits still require verifier/debugger testing.
$pdb = Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc\debug\knightbus_windows_driver.pdb'
$symbols = & (Join-Path $env:LIBCLANG_PATH 'llvm-pdbutil.exe') dump -symbols $pdb
if ($LASTEXITCODE -ne 0) { throw 'Could not inspect driver stack frames' }
$frames = [regex]::Matches(($symbols -join "`n"), '(?s)S_[LG]PROC32[^`]*`([^`]+)`.*?S_FRAMEPROC.*?size = ([0-9]+),')
$owned = @($frames | Where-Object {$_.Groups[1].Value -like 'knightbus_windows_driver::*' -or $_.Groups[1].Value -like 'knightbus_windows_protocol::*' -or $_.Groups[1].Value -like 'knightbus_usb::*'})
if (!$owned.Count) { throw 'No driver/protocol stack records found' }
foreach ($frame in $owned) {
    if ([int]$frame.Groups[2].Value -gt 4096) { throw "Kernel stack frame exceeds 4 KiB: $($frame.Groups[1].Value)" }
}
Write-Output "Checked $($owned.Count) driver/protocol/USB stack frames (4 KiB maximum each)."

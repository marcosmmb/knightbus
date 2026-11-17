# Install the pinned upstream package without changing Windows security settings.
$ErrorActionPreference='Stop'
$driver=Join-Path $PSScriptRoot 'driver'
$hardwareId='ROOT\USBIP_WIN2\UDE'
$rebootRequiredExit=3010
$noDevicesUpdatedExit=259 # ERROR_NO_MORE_ITEMS: package already staged, no better match.
$expected=@{
 'usbip2_filter.cat'='f259d60bdb5ad9d7a976418bfc9156b788dfcec9b67c8d05d036eb03fb18ffe1'
 'usbip2_filter.inf'='b7fe01b04793d2f83cf3407601f2c835f1a10c2618e32a52c8fdef1da6b79bff'
 'usbip2_filter.sys'='b93a5fc3c23d56f9b49634742374e32f269e53f764caa2d5fcb43e3259f7eced'
 'usbip2_ude.cat'='84dd6cc5985857adf1514a9ee202210ff3ee1384c9be40fd49d6810e23e25f3a'
 'usbip2_ude.inf'='1ae9e8dc497929de1a6be1291ae1dca6a19e7fd6c0351e8c266f65dd91f15073'
 'usbip2_ude.sys'='abd4fc43dce40e027e4acc884cbfe92433e026206f6565c5ee5284a39441169a'
}
foreach ($name in $expected.Keys) {
 if ((Get-FileHash -LiteralPath (Join-Path $driver $name) -Algorithm SHA256).Hash -ine $expected[$name]) {
  throw "The bundled USB/IP driver is missing or modified: $name"
 }
}
foreach ($package in @('usbip2_ude','usbip2_filter')) {
 $signature=Get-AuthenticodeSignature (Join-Path $driver "$package.cat")
 if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'CN=Microsoft Windows Hardware Compatibility Publisher,') {
  throw "Windows could not verify the Microsoft signature on $package."
 }
}

foreach ($package in @('usbip2_ude','usbip2_filter')) {
 $packageInf=Join-Path $driver "$package.inf"
 & "$env:WINDIR\System32\pnputil.exe" /add-driver $packageInf /install
 if ($LASTEXITCODE -eq $rebootRequiredExit) {Write-Warning 'Restart Windows before using remote USB devices.'}
 elseif ($LASTEXITCODE -notin @(0,$noDevicesUpdatedExit)) {throw "Windows refused the signed USB/IP driver (exit $LASTEXITCODE)."}
}
$inf=Join-Path $driver 'usbip2_ude.inf'

# PnPUtil stages a root driver but does not create its controller device.
$controllers=@(Get-PnpDevice | Where-Object {
 $ids=(Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName 'DEVPKEY_Device_HardwareIds' -ErrorAction SilentlyContinue).Data
 $ids -contains $hardwareId
})
if ($controllers.Count -gt 1) { throw 'Multiple USB/IP UDE controllers exist. Remove duplicate controllers in Device Manager, then retry.' }
if ($controllers.Count -eq 0) {
 Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
public static class KnightBusUsbIpSetup {
    const uint GenerateId=1, HardwareIdProperty=1, RegisterDevice=25, RemoveDevice=5;
    static readonly Guid UsbClass=new Guid("36FC9E60-C465-11CF-8056-444553540000");
    static readonly IntPtr InvalidHandle=new IntPtr(-1);
    [StructLayout(LayoutKind.Sequential)]
    struct DeviceInfo { public uint Size; public Guid Class; public uint Instance; public UIntPtr Reserved; }
    [DllImport("setupapi.dll",SetLastError=true)]
    static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid cls,IntPtr parent);
    [DllImport("setupapi.dll",CharSet=CharSet.Unicode,SetLastError=true)]
    static extern bool SetupDiCreateDeviceInfoW(IntPtr set,string name,ref Guid cls,string description,IntPtr parent,uint flags,ref DeviceInfo info);
    [DllImport("setupapi.dll",CharSet=CharSet.Unicode,SetLastError=true)]
    static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr set,ref DeviceInfo info,uint property,byte[] value,uint size);
    [DllImport("setupapi.dll",SetLastError=true)]
    static extern bool SetupDiCallClassInstaller(uint function,IntPtr set,ref DeviceInfo info);
    [DllImport("setupapi.dll")]
    static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);
    [DllImport("newdev.dll",CharSet=CharSet.Unicode,SetLastError=true)]
    static extern bool UpdateDriverForPlugAndPlayDevicesW(IntPtr parent,string hardware,string inf,uint flags,out bool reboot);
    static void Check(bool success) { if (!success) throw new Win32Exception(Marshal.GetLastWin32Error()); }
    public static bool Install(string inf,string hardware) {
        Guid cls=UsbClass;
        IntPtr set=SetupDiCreateDeviceInfoList(ref cls,IntPtr.Zero);
        if (set==InvalidHandle) throw new Win32Exception(Marshal.GetLastWin32Error());
        var info=new DeviceInfo { Size=(uint)Marshal.SizeOf(typeof(DeviceInfo)) };
        bool registered=false, installed=false;
        try {
            Check(SetupDiCreateDeviceInfoW(set,"USBIP_WIN2",ref cls,"USBip 3.X Emulated Host Controller",IntPtr.Zero,GenerateId,ref info));
            byte[] ids=Encoding.Unicode.GetBytes(hardware+"\0\0");
            Check(SetupDiSetDeviceRegistryPropertyW(set,ref info,HardwareIdProperty,ids,(uint)ids.Length));
            Check(SetupDiCallClassInstaller(RegisterDevice,set,ref info));
            registered=true;
            bool reboot;
            Check(UpdateDriverForPlugAndPlayDevicesW(IntPtr.Zero,hardware,inf,0,out reboot));
            installed=true;
            return reboot;
        } finally {
            if (registered && !installed) SetupDiCallClassInstaller(RemoveDevice,set,ref info);
            SetupDiDestroyDeviceInfoList(set);
        }
    }
}
'@
 if ([KnightBusUsbIpSetup]::Install($inf,$hardwareId)) { Write-Warning 'Restart Windows before using remote USB devices.' }
} elseif ($controllers[0].Status -ne 'OK') {
 throw 'The USB/IP controller is installed but Windows could not start it. Restart Windows and inspect its status in Device Manager.'
}

# Remove only the idle legacy controller from a verified KnightBus installation.
$legacySys=Join-Path $env:ProgramFiles 'KnightBus\driver\usbip_vhci_ude.sys'
$legacyHash='c04ae09e411870ba4f515b8099aaa05c7ae46eb889ccae0bacb90b089c266b76'
if ((Test-Path -LiteralPath $legacySys) -and (Get-FileHash -LiteralPath $legacySys -Algorithm SHA256).Hash -ieq $legacyHash) {
 foreach ($device in @(Get-PnpDevice -PresentOnly)) {
  $ids=(Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_HardwareIds' -ErrorAction SilentlyContinue).Data
  if ($ids -notcontains 'ROOT\VHCI_ude') {continue}
  $hubs=@((Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName 'DEVPKEY_Device_Children' -ErrorAction Stop).Data)
  foreach ($hub in $hubs) {
   if (!$hub.StartsWith('USB\ROOT_HUB30\',[StringComparison]::OrdinalIgnoreCase)) {throw 'The old USB driver still has an attached device. Disconnect it before upgrading.'}
   $children=(Get-PnpDeviceProperty -InstanceId $hub -KeyName 'DEVPKEY_Device_Children' -ErrorAction Stop).Data
   if (@($children | Where-Object {$_}).Count) {throw 'The old USB driver still has attached devices. Disconnect them before upgrading.'}
  }
  & "$env:WINDIR\System32\pnputil.exe" /remove-device $device.InstanceId
  if ($LASTEXITCODE -eq $rebootRequiredExit) {throw 'Restart Windows to finish removing the old USB controller, then rerun setup.'}
  if ($LASTEXITCODE -ne 0) {throw "Could not remove the idle legacy USB controller (exit $LASTEXITCODE)."}
 }
}

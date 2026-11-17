# Install the pinned upstream package without changing Windows security settings.
$ErrorActionPreference='Stop'
$driver=Join-Path $PSScriptRoot 'driver'
$hardwareId='ROOT\VHCI_ude'
$rebootRequiredExit=3010
$noDevicesUpdatedExit=259 # ERROR_NO_MORE_ITEMS: package already staged, no better match.
$expected=@{
 'usbip_vhci_ude.inf'='869c237cd2a3d6fb5e98adb59f0f415d696614e99502896a856dd0362093b8b2'
 'usbip_vhci_ude.sys'='c04ae09e411870ba4f515b8099aaa05c7ae46eb889ccae0bacb90b089c266b76'
 'usbip_vhci_ude.cat'='a0d905c5faa1849052d79e421e058108ffa074f5acbf4a467ce7716dcbc0accc'
}
foreach ($name in $expected.Keys) {
 if ((Get-FileHash -LiteralPath (Join-Path $driver $name) -Algorithm SHA256).Hash -ine $expected[$name]) {
  throw "The bundled USB/IP driver is missing or modified: $name"
 }
}
$signature=Get-AuthenticodeSignature (Join-Path $driver 'usbip_vhci_ude.cat')
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'CN=Microsoft Windows Hardware Compatibility Publisher,') {
 throw 'Windows could not verify the Microsoft signature on the USB/IP driver.'
}
$inf=Join-Path $driver 'usbip_vhci_ude.inf'
& "$env:WINDIR\System32\pnputil.exe" /add-driver $inf /install
if ($LASTEXITCODE -eq $rebootRequiredExit) { Write-Warning 'Restart Windows before using remote USB devices.' }
elseif ($LASTEXITCODE -notin @(0,$noDevicesUpdatedExit)) { throw "Windows refused the signed USB/IP driver (exit $LASTEXITCODE)." }

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
            Check(SetupDiCreateDeviceInfoW(set,"VHCI_ude",ref cls,"USB/IP virtual USB controller",IntPtr.Zero,GenerateId,ref info));
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

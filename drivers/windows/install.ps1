# Scoped development install/rollback. Requires administrator rights and WDK
# DevGen only for explicit legacy-controller diagnostics. Never binds physical
# USB devices or changes boot/security settings. Stage is the normal owned path.
param([ValidateSet('Stage','Install','Remove')][string]$Action='Stage',
      [string]$Record="$PSScriptRoot\target\installation.json")
$ErrorActionPreference='Stop'
function Step([string]$Tool,[string[]]$Arguments) {
    $output = & $Tool @Arguments 2>&1
    $code = $LASTEXITCODE
    $output | Out-Host
    if ($code -ne 0) { throw "$Tool exited $code; preserve the installation record and inspect before retrying" }
    return ($output -join "`n")
}
# Per-device LowerFilters only: never register a USB class-wide filter. Keep
# other filters intact, and require the hub's parent to match the owned record.
function LoadFilterApi {
    if ('KnightBusDeviceFilters' -as [type]) { return }
    Add-Type @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
public static class KnightBusDeviceFilters {
 [StructLayout(LayoutKind.Sequential)] struct Info {public uint Size;public Guid Class;public uint Node;public UIntPtr Reserved;}
 [DllImport("setupapi.dll",SetLastError=true)] static extern IntPtr SetupDiCreateDeviceInfoList(IntPtr guid,IntPtr hwnd);
 [DllImport("setupapi.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool SetupDiOpenDeviceInfoW(IntPtr set,string id,IntPtr hwnd,uint flags,ref Info info);
 [DllImport("setupapi.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr set,ref Info info,uint property,byte[] data,uint size);
 [DllImport("setupapi.dll")] static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);
 public static void Set(string id,string[] filters) {
  var set=SetupDiCreateDeviceInfoList(IntPtr.Zero,IntPtr.Zero);
  if(set==new IntPtr(-1))throw new Win32Exception();
  try {var info=new Info{Size=(uint)Marshal.SizeOf(typeof(Info))};
   if(!SetupDiOpenDeviceInfoW(set,id,IntPtr.Zero,0,ref info))throw new Win32Exception();
   byte[] value=filters.Length==0?null:Encoding.Unicode.GetBytes(string.Join("\0",filters)+"\0\0");
   if(!SetupDiSetDeviceRegistryPropertyW(set,ref info,18,value,value==null?0:(uint)value.Length))throw new Win32Exception();
  }finally{SetupDiDestroyDeviceInfoList(set);}
 }
}
// Reserve objects directly in the OS-owned Device directory. Its parent ACL
// prevents unprivileged precreation, unlike Win32 Global\ named objects.
public static class KnightBusSlots {
 [StructLayout(LayoutKind.Sequential)] struct Name {public ushort Length,Maximum;public IntPtr Buffer;}
 [StructLayout(LayoutKind.Sequential)] struct Attributes {public uint Length;public IntPtr Root,Name;public uint Flags;public IntPtr Security,Qos;}
 [DllImport("ntdll.dll")] static extern int NtCreateEvent(out IntPtr handle,uint access,ref Attributes attributes,int type,[MarshalAs(UnmanagedType.U1)] bool initial);
 [DllImport("ntdll.dll")] static extern uint RtlNtStatusToDosError(int status);
 public static Microsoft.Win32.SafeHandles.SafeWaitHandle Reserve(int slot) {
  if(slot<0 || slot>=64)throw new ArgumentOutOfRangeException("slot");
  string path=@"\Device\KnightBus.Controller."+slot;
  var descriptor=new System.Security.AccessControl.RawSecurityDescriptor("D:P(A;;GA;;;SY)(A;;GA;;;BA)");
  byte[] bytes=new byte[descriptor.BinaryLength];descriptor.GetBinaryForm(bytes,0);
  IntPtr security=Marshal.AllocHGlobal(bytes.Length),text=Marshal.StringToHGlobalUni(path),name=Marshal.AllocHGlobal(Marshal.SizeOf(typeof(Name)));
  try {
   Marshal.Copy(bytes,0,security,bytes.Length);
   var value=new Name{Length=(ushort)(path.Length*2),Maximum=(ushort)((path.Length+1)*2),Buffer=text};
   Marshal.StructureToPtr(value,name,false);
   // Exclusive create: never adopt another owner's event (no OBJ_OPENIF).
   var attributes=new Attributes{Length=(uint)Marshal.SizeOf(typeof(Attributes)),Name=name,Flags=64,Security=security};
   IntPtr handle;int status=NtCreateEvent(out handle,0x100002,ref attributes,0,false);
   if(status==unchecked((int)0xc0000035))throw new InvalidOperationException("A controller slot is active or quarantined; stop its owner before uninstalling");
   if(status<0)throw new Win32Exception((int)RtlNtStatusToDosError(status));
   return new Microsoft.Win32.SafeHandles.SafeWaitHandle(handle,true);
  }finally{Marshal.FreeHGlobal(name);Marshal.FreeHGlobal(text);Marshal.FreeHGlobal(security);}
 }
}

'@
}
function HubFilters([string]$Hub,[string]$Parent) {
    $actual=(Get-PnpDeviceProperty -InstanceId $Hub -KeyName DEVPKEY_Device_Parent).Data
    if ($actual -ne $Parent -or $Hub -notlike 'USB\ROOT_HUB30\*') {throw 'Root-hub ownership changed'}
    return @((Get-PnpDeviceProperty -InstanceId $Hub -KeyName DEVPKEY_Device_LowerFilters).Data | Where-Object {$_})
}
function BindHub($State,[string]$Path) {
    $deadline=[DateTime]::UtcNow.AddSeconds(5)
    do {
        $hubs=@(Get-PnpDevice -PresentOnly | Where-Object {$_.InstanceId -like 'USB\ROOT_HUB30\*'} |
            Where-Object {(Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName DEVPKEY_Device_Parent).Data -eq $State.instance})
        if ($hubs.Count -eq 1) {break}
        if ($hubs.Count -gt 1 -or [DateTime]::UtcNow -ge $deadline) {throw 'Cannot identify the owned virtual root hub'}
        Start-Sleep -Milliseconds 100
    } while ($true)
    $hub=$hubs[0].InstanceId
    $before=@(HubFilters $hub $State.instance)
    if ($before -contains 'KnightBusUde') {throw 'Existing hub filter: preserve its installation record'}
    $State.hub=$hub
    $State.filterBefore=$before
    # Journal the exact target before changing its property or restarting it.
    $State | ConvertTo-Json | Set-Content $Path
    LoadFilterApi
    [KnightBusDeviceFilters]::Set($hub,[string[]](@($before)+@('KnightBusUde')))
    $null=Step 'pnputil.exe' @('/restart-device',$State.instance)
    if ((Get-PnpDevice -InstanceId $hub).Status -ne 'OK') {throw 'Virtual root-hub filter failed to start'}
}
$hardware='ROOT\KNIGHTBUS_UDE'
if ($Action -eq 'Remove') {
    $state=Get-Content $Record -Raw | ConvertFrom-Json
    if ($state.hardware -ne $hardware -or (!$state.stagedOnly -and $state.instance -notmatch '^ROOT\\') -or $state.package -notmatch '^oem[0-9]+\.inf$') {throw 'Invalid installation record'}
    $package=Get-WindowsDriver -Online -Driver $state.package
    if ([IO.Path]::GetFileName($package.OriginalFileName) -ine 'KnightBusUde.inf' -or (Get-FileHash $package.OriginalFileName).Hash -ne $state.infHash) {throw 'Package ownership changed'}
    if ($state.stagedOnly) {
        # Hold every slot name while uninstalling, so a new embedded/service host
        # cannot create a controller between our inventory check and deletion.
        LoadFilterApi
        $locks=[Collections.Generic.List[Microsoft.Win32.SafeHandles.SafeWaitHandle]]::new()
        try {
            for($slot=0;$slot -lt 64;$slot++) {
                $locks.Add([KnightBusSlots]::Reserve($slot))
            }
            $nodes=@(Get-CimInstance Win32_PnPEntity | Where-Object {$_.HardwareID -contains $hardware})
            if($nodes.Count){throw 'Present KnightBus controller: preserve its owner/installation record'}
            # Only the hash-verified package is uninstalled. Persisted software
            # slots are bounded and can be reused when the package is restaged.
            $null=Step 'pnputil.exe' @('/delete-driver',$state.package,'/uninstall')
        } finally {foreach($event in $locks){$event.Dispose()}}
        return
    }
    if ($state.hub -and (Get-PnpDevice -InstanceId $state.hub -ErrorAction SilentlyContinue)) {
        $filters=@(HubFilters $state.hub $state.instance)
        if ($filters -contains 'KnightBusUde') {
            LoadFilterApi
            [KnightBusDeviceFilters]::Set($state.hub,[string[]]@($filters | Where-Object {$_ -ne 'KnightBusUde'}))
        }
    }
    $node=Get-PnpDevice -InstanceId $state.instance -ErrorAction SilentlyContinue
    if ($node) {
        $ids=(Get-PnpDeviceProperty -InstanceId $state.instance -KeyName DEVPKEY_Device_HardwareIds).Data
        if ($ids -notcontains $hardware) {throw 'Device ownership changed'}
        $null=Step 'pnputil.exe' @('/remove-device',$state.instance,'/subtree')
    }
    $null=Step 'pnputil.exe' @('/delete-driver',$state.package)
    return
}
if (Test-Path $Record) {throw 'Installation record exists; remove that exact installation first'}
if (Confirm-SecureBootUEFI) {throw 'Development signing cannot be used with Secure Boot; this script never changes it'}
$boot=Step 'bcdedit.exe' @('/enum','{current}')
if ($boot -notmatch '(?m)^\s*testsigning\s+Yes\s*$') {throw 'Existing developer test-signing configuration required'}
$inf="$PSScriptRoot\target\package\KnightBusUde.inf"
$signature=Get-AuthenticodeSignature "$PSScriptRoot\target\package\KnightBusUde.sys"
if ($signature.Status -ne 'Valid') {throw 'Valid development signature required'}
if($Action -eq 'Install') {
    $existing=@(Get-CimInstance Win32_PnPEntity | Where-Object {$_.HardwareID -contains $hardware})
    if ($existing.Count) {throw 'Existing KnightBus controller: preserve its installation record'}
}
$output=Step 'pnputil.exe' @('/add-driver',$inf)
$packages=@([regex]::Matches($output,'\boem[0-9]+\.inf\b') | ForEach-Object Value | Select-Object -Unique)
if ($packages.Count -ne 1) {throw 'Cannot identify staged package; inspect pnputil output'}
if($Action -eq 'Stage') {
    @{hardware=$hardware;stagedOnly=$true;package=$packages[0];infHash=(Get-FileHash $inf).Hash} | ConvertTo-Json | Set-Content $Record
    return
}
$name='KnightBus-'+[Guid]::NewGuid().ToString('N')
$state=@{hardware=$hardware;instance="ROOT\$name\0000";package=$packages[0];infHash=(Get-FileHash $inf).Hash}
# Persist package identity before creating a node, including interrupted installs.
$state | ConvertTo-Json | Set-Content $Record
$devgen="${env:ProgramFiles(x86)}\Windows Kits\10\Tools\10.0.28000.0\x64\devgen.exe"
$null=Step $devgen @('/add','/bus','ROOT','/instanceid',$name,'/hardwareid',$hardware)
$nodes=@(Get-CimInstance Win32_PnPEntity | Where-Object {$_.HardwareID -contains $hardware})
if ($nodes.Count -ne 1) {throw 'Unexpected controller count; retain record'}
$state.instance=$nodes[0].PNPDeviceID
$state | ConvertTo-Json | Set-Content $Record
$null=Step 'pnputil.exe' @('/add-driver',$inf,'/install')
BindHub $state $Record
Get-PnpDevice -InstanceId $state.instance

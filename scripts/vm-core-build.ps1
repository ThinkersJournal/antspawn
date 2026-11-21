# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
  Build the Windows Server 2025 Datacenter CORE evaluation base VHDX from the evaluation ISO by letting Setup run inside a
  throwaway Hyper-V VM (docs/RUNNER-VM-PATH.md, "Building the Core base from the evaluation ISO", Route 1). Runs as the
  interactive user in Hyper-V Administrators; never elevates (DISM would: Setup inside the guest does the apply instead).

.DESCRIPTION
  DRY-RUN BY DEFAULT (no -Apply: prints, changes nothing). The VM has no connected network adapter. The guest Administrator
  password comes from ANTSPAWN_PILOT_PW (run under `with-secret`), is written ONLY into the seed ISO's Autounattend.xml, and
  the seed ISO is deleted by -Step Cleanup. Nothing is printed that contains it.

  NOTE: the answer file sets AcceptEula=true (unattended Setup requires it). The licence text beyond the on-image stub
  (it points at aka.ms/useterms) has not been fully read by the lane: that acceptance is CireSnave's evaluation approval.

  Steps: Seed -> Install -> Verify -> Cleanup
    Seed     write Autounattend.xml (image INDEX from -Index, chosen by reading the WIM's XML, never guessed) into seed.iso (IMAPI2)
    Install  create the VM (gen 2, ISO + seed as DVDs, blank 64 GB dynamic VHDX), start it, tap a key to pass "Press any key to
             boot from CD", wait for PowerShell Direct to answer, then stop the VM (the VHDX is the base)
    Verify   read InstallationType over PowerShell Direct (needs a running VM; boots the base through a differencing child)
    Cleanup  remove the install VM and the seed ISO; keep the base VHDX
#>
[CmdletBinding()]
param(
    [ValidateSet('Seed', 'Install', 'Verify', 'Cleanup')][string]$Step = 'Seed',
    [switch]$Apply,
    [string]$Iso = 'C:\vmimages\26100.32230.260111-0550.lt_release_svc_refresh_SERVER_EVAL_x64FRE_en-us.iso',
    [int]$Index = 3,
    [string]$Work = 'C:\vmimages\pilot'
)
$ErrorActionPreference = 'Stop'
$VmName = 'antspawn-core-build'
$Base = Join-Path $Work 'core-base.vhdx'
$SeedIso = Join-Path $Work 'seed.iso'
$mode = if ($Apply) { 'APPLY' } else { 'DRY RUN (nothing is changed)' }
"vm-core-build: step=$Step index=$Index mode=$mode"

function Cred {
    if (-not $env:ANTSPAWN_PILOT_PW) { throw 'run under with-secret ANTSPAWN_PILOT_PW (never printed)' }
    New-Object System.Management.Automation.PSCredential('Administrator', (ConvertTo-SecureString $env:ANTSPAWN_PILOT_PW -AsPlainText -Force))
}

function New-Answer {
    $pw = [Security.SecurityElement]::Escape($env:ANTSPAWN_PILOT_PW)
@"
<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend">
  <settings pass="windowsPE">
    <component name="Microsoft-Windows-International-Core-WinPE" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
      <SetupUILanguage><UILanguage>en-US</UILanguage></SetupUILanguage>
      <InputLocale>en-US</InputLocale><SystemLocale>en-US</SystemLocale><UILanguage>en-US</UILanguage><UserLocale>en-US</UserLocale>
    </component>
    <component name="Microsoft-Windows-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
      <DiskConfiguration>
        <Disk wcm:action="add">
          <DiskID>0</DiskID><WillWipeDisk>true</WillWipeDisk>
          <CreatePartitions>
            <CreatePartition wcm:action="add"><Order>1</Order><Type>EFI</Type><Size>100</Size></CreatePartition>
            <CreatePartition wcm:action="add"><Order>2</Order><Type>MSR</Type><Size>16</Size></CreatePartition>
            <CreatePartition wcm:action="add"><Order>3</Order><Type>Primary</Type><Extend>true</Extend></CreatePartition>
          </CreatePartitions>
          <ModifyPartitions>
            <ModifyPartition wcm:action="add"><Order>1</Order><PartitionID>1</PartitionID><Format>FAT32</Format><Label>System</Label></ModifyPartition>
            <ModifyPartition wcm:action="add"><Order>2</Order><PartitionID>2</PartitionID></ModifyPartition>
            <ModifyPartition wcm:action="add"><Order>3</Order><PartitionID>3</PartitionID><Format>NTFS</Format><Label>Windows</Label><Letter>C</Letter></ModifyPartition>
          </ModifyPartitions>
        </Disk>
      </DiskConfiguration>
      <ImageInstall><OSImage>
        <InstallFrom><MetaData wcm:action="add"><Key>/IMAGE/INDEX</Key><Value>$Index</Value></MetaData></InstallFrom>
        <InstallTo><DiskID>0</DiskID><PartitionID>3</PartitionID></InstallTo>
      </OSImage></ImageInstall>
      <UserData><AcceptEula>true</AcceptEula></UserData>
    </component>
  </settings>
  <settings pass="oobeSystem">
    <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
      <OOBE><HideEULAPage>true</HideEULAPage><SkipMachineOOBE>true</SkipMachineOOBE><ProtectYourPC>3</ProtectYourPC></OOBE>
      <UserAccounts><AdministratorPassword><Value>$pw</Value><PlainText>true</PlainText></AdministratorPassword></UserAccounts>
    </component>
  </settings>
</unattend>
"@
}

function New-SeedIso([string]$path, [string]$answer) {
    Add-Type -TypeDefinition @'
using System; using System.IO; using System.Runtime.InteropServices; using System.Runtime.InteropServices.ComTypes;
public static class IsoWriter {
    public static void Write(object stream, string path) {
        var s = (IStream)stream; System.Runtime.InteropServices.ComTypes.STATSTG st; s.Stat(out st, 1);
        var buf = new byte[1 << 16]; long left = st.cbSize; IntPtr p = Marshal.AllocHGlobal(4);
        using (var f = File.Create(path)) {
            while (left > 0) { int n = (int)Math.Min(buf.Length, left); s.Read(buf, n, p); int got = Marshal.ReadInt32(p); if (got == 0) break; f.Write(buf, 0, got); left -= got; }
        }
        Marshal.FreeHGlobal(p);
    }
}
'@
    $tmp = Join-Path $env:TEMP ('seed-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        [IO.File]::WriteAllText((Join-Path $tmp 'Autounattend.xml'), $answer, (New-Object Text.UTF8Encoding($false)))
        $fsi = New-Object -ComObject IMAPI2FS.MsftFileSystemImage
        $fsi.FileSystemsToCreate = 4   # UDF
        $fsi.VolumeName = 'SEED'
        $fsi.Root.AddTree($tmp, $false)
        [IsoWriter]::Write($fsi.CreateResultImage().ImageStream, $path)
    } finally { Remove-Item -LiteralPath $tmp -Recurse -Force }
}

switch ($Step) {
    'Seed' {
        if (-not $Apply) { "would: write Autounattend.xml (image index $Index, password from ANTSPAWN_PILOT_PW, AcceptEula) into $SeedIso with IMAPI2"; break }
        New-SeedIso $SeedIso (New-Answer)
        "wrote $SeedIso ($((Get-Item $SeedIso).Length) bytes)"
    }
    'Install' {
        if (-not $Apply) { "would: New-VHD $Base 64GB dynamic; New-VM $VmName gen2 4GB 2 CPU, no connected NIC, DVD=$Iso + DVD=$SeedIso, boot from DVD; Start-VM; tap Space (Msvm_Keyboard) until Setup runs; wait for PS Direct; Stop-VM"; break }
        if (-not (Test-Path -LiteralPath $SeedIso)) { throw 'run -Step Seed -Apply first' }
        if (Test-Path -LiteralPath $Base) { throw "$Base exists: refuse to overwrite" }
        New-VHD -Path $Base -SizeBytes 64GB -Dynamic | Out-Null
        New-VM -Name $VmName -Generation 2 -MemoryStartupBytes 4GB -VHDPath $Base | Out-Null
        Set-VM -Name $VmName -ProcessorCount 2 -CheckpointType Disabled -AutomaticCheckpointsEnabled $false
        Add-VMDvdDrive -VMName $VmName -Path $Iso
        Add-VMDvdDrive -VMName $VmName -Path $SeedIso
        $dvd = Get-VMDvdDrive -VMName $VmName | Where-Object Path -eq $Iso
        Set-VMFirmware -VMName $VmName -FirstBootDevice $dvd
        $t = [Diagnostics.Stopwatch]::StartNew()
        Start-VM -Name $VmName
        $cs = Get-CimInstance -Namespace root\virtualization\v2 -ClassName Msvm_ComputerSystem -Filter "ElementName='$VmName'"
        $kb = Get-CimAssociatedInstance -InputObject $cs -ResultClassName Msvm_Keyboard
        1..20 | ForEach-Object { Invoke-CimMethod -InputObject $kb -MethodName TypeKey -Arguments @{ KeyCode = 32 } | Out-Null; Start-Sleep -Milliseconds 500 }
        "key taps sent after $([math]::Round($t.Elapsed.TotalSeconds,1)) s; waiting for the installed guest (up to 60 min)"
        $ok = $false
        while ($t.Elapsed.TotalMinutes -lt 60) {
            Start-Sleep -Seconds 20
            try { $s = New-PSSession -VMName $VmName -Credential (Cred) -ErrorAction Stop; Remove-PSSession $s; $ok = $true; break } catch { }
        }
        if (-not $ok) { throw 'PowerShell Direct never answered in 60 min (look at the console: vmconnect localhost antspawn-core-build)' }
        "installed guest answered PowerShell Direct after $([math]::Round($t.Elapsed.TotalMinutes,1)) min (Setup + first boot)"
        $s = New-PSSession -VMName $VmName -Credential (Cred)
        try {
            Invoke-Command -Session $s -ScriptBlock {
                "InstallationType: $((Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').InstallationType)"
                "EditionID: $((Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').EditionID)"
                "Build: $((Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').CurrentBuildNumber).$((Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').UBR)"
                # setup copies the answer file (with the password) here: remove it
                Remove-Item C:\Windows\Panther\unattend.xml, C:\Windows\Panther\Autounattend.xml, C:\Windows\Panther\UnattendGC\* -Force -ErrorAction SilentlyContinue
                $v = Get-Volume -DriveLetter C
                'C: used {0:N2} GB of {1:N1} GB' -f (($v.Size - $v.SizeRemaining) / 1GB), ($v.Size / 1GB)
            }
        } finally { Remove-PSSession $s }
        Stop-VM -Name $VmName -Force
        "base VHDX now $([math]::Round((Get-Item $Base).Length/1GB,2)) GB on disk"
    }
    'Verify' {
        if (-not $Apply) { 'would: boot a differencing child of the base and read InstallationType (see Install, which already prints it)'; break }
        'Install already prints InstallationType, EditionID and build; boot a child via vm-pilot.ps1 -Image <base> for further steps.'
    }
    'Cleanup' {
        if (-not $Apply) { "would: Remove-VM $VmName (the base $Base is kept); delete $SeedIso"; break }
        if (Get-VM -Name $VmName -ErrorAction SilentlyContinue) { Stop-VM -Name $VmName -TurnOff -Force -ErrorAction SilentlyContinue; Remove-VM -Name $VmName -Force }
        if (Test-Path -LiteralPath $SeedIso) { Remove-Item -LiteralPath $SeedIso -Force }
        "removed the VM; seed ISO exists: $(Test-Path -LiteralPath $SeedIso); base kept: $(Test-Path -LiteralPath $Base)"
    }
}

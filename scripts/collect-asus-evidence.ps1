# Read-only inventory, no hardware configuration changes.
# Read-only inventory, no hardware configuration changes.
param([string]$OutputPath = "PhoenixOS-ASUS-Windows-evidence.txt")

$report = New-Object 'System.Collections.Generic.List[string]'
[void]$report.Add("PhoenixOS: read-only Windows hardware inventory")
[void]$report.Add("Timestamp: $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')")
[void]$report.Add("NOTICE: Windows inventory does not verify PhoenixOS IOMMU translation")

try {
    $pc = Get-CimInstance Win32_ComputerSystem
    [void]$report.Add("Computer: $($pc.Manufacturer) $($pc.Model)")
    [void]$report.Add("Memory_MiB: $([math]::Floor($pc.TotalPhysicalMemory / 1MB))")
} catch { [void]$report.Add("Computer: unavailable") }

try {
    $bios = Get-CimInstance Win32_BIOS
    [void]$report.Add("UEFI: $($bios.SMBIOSBIOSVersion)")
} catch { [void]$report.Add("UEFI: unavailable") }

try {
    foreach ($cpu in @(Get-CimInstance Win32_Processor)) {
        [void]$report.Add("CPU: $($cpu.Name), cores=$($cpu.NumberOfCores), threads=$($cpu.NumberOfLogicalProcessors)")
    }
} catch { [void]$report.Add("CPU: unavailable") }

try {
    foreach ($device in @(Get-CimInstance Win32_PnPEntity)) {
        $id = [string]$device.PNPDeviceID
        if ($id -match 'VEN_([0-9A-F]{4})&DEV_([0-9A-F]{4})') {
            $vendor = $Matches[1]
            $product = $Matches[2]
            [void]$report.Add("PCI VEN=$vendor DEV=$product status=$($device.Status) name=$($device.Name)")
        }
    }
} catch { [void]$report.Add("PCI: unavailable") }

try {
    foreach ($disk in @(Get-CimInstance Win32_DiskDrive)) {
        [void]$report.Add("Storage: $($disk.Model), interface=$($disk.InterfaceType)")
    }
} catch { [void]$report.Add("Storage: unavailable") }

try {
    foreach ($volume in @(Get-BitLockerVolume -ErrorAction Stop)) {
        [void]$report.Add("BitLocker: $($volume.MountPoint), protection=$($volume.ProtectionStatus)")
    }
} catch { [void]$report.Add("BitLocker: unavailable") }

# Read-only inventory, no hardware configuration changes.
# Read-only inventory, no hardware configuration changes.
$report | Out-File -LiteralPath $OutputPath -Encoding UTF8
Write-Output "Saved: $OutputPath"

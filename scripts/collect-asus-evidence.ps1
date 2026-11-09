# Пассивный сбор сведений Windows 10. Драйверы и прошивка не изменяются.
# Запуск: powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\collect-asus-evidence.ps1
param([string]$OutputPath = "PhoenixOS-ASUS-Windows-evidence.txt")

$report = New-Object 'System.Collections.Generic.List[string]'
[void]$report.Add("PhoenixOS: сведения Windows, только чтение")
[void]$report.Add("Дата: $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')")
[void]$report.Add("Внимание: эти данные не подтверждают работу IOMMU в PhoenixOS")

try {
    $pc = Get-CimInstance Win32_ComputerSystem
    [void]$report.Add("Ноутбук: $($pc.Manufacturer) $($pc.Model)")
    [void]$report.Add("Память, МиБ: $([math]::Floor($pc.TotalPhysicalMemory / 1MB))")
} catch { [void]$report.Add("Сведения о компьютере недоступны") }

try {
    $bios = Get-CimInstance Win32_BIOS
    [void]$report.Add("Версия UEFI: $($bios.SMBIOSBIOSVersion)")
} catch { [void]$report.Add("Версия UEFI неизвестна") }

try {
    foreach ($cpu in @(Get-CimInstance Win32_Processor)) {
        [void]$report.Add("Процессор: $($cpu.Name), ядер=$($cpu.NumberOfCores), потоков=$($cpu.NumberOfLogicalProcessors)")
    }
} catch { [void]$report.Add("Сведения о процессоре недоступны") }

try {
    foreach ($device in @(Get-CimInstance Win32_PnPEntity)) {
        $id = [string]$device.PNPDeviceID
        if ($id -match 'VEN_([0-9A-F]{4})&DEV_([0-9A-F]{4})') {
            $vendor = $Matches[1]
            $product = $Matches[2]
            [void]$report.Add("PCI VEN=$vendor DEV=$product status=$($device.Status) name=$($device.Name)")
        }
    }
} catch { [void]$report.Add("Перечень PCI недоступен") }

try {
    foreach ($disk in @(Get-CimInstance Win32_DiskDrive)) {
        [void]$report.Add("Накопитель: $($disk.Model), интерфейс=$($disk.InterfaceType)")
    }
} catch { [void]$report.Add("Перечень накопителей недоступен") }

try {
    foreach ($volume in @(Get-BitLockerVolume -ErrorAction Stop)) {
        [void]$report.Add("BitLocker: $($volume.MountPoint), защита=$($volume.ProtectionStatus)")
    }
} catch { [void]$report.Add("BitLocker: состояние не удалось прочитать") }

# Серийные номера, ключи восстановления и индивидуальные ID не сохраняются.
# Записывается только отчёт в указанный пользователем файл, без прямого ввода-вывода.
$report | Out-File -LiteralPath $OutputPath -Encoding UTF8
Write-Output "Готово: $OutputPath"

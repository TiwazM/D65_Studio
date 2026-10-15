param([string]$Executable, [string]$SourceRoot = (Join-Path $PSScriptRoot '../..'), [Parameter(Mandatory=$true)][string]$FixturePath)
$ErrorActionPreference = 'Stop'
$sourceRoot = (Resolve-Path -LiteralPath $SourceRoot).Path
$exe = Join-Path $sourceRoot 'target/release/d65-studio.exe'
if ($Executable) { $exe = $Executable }
$scratch = Join-Path ([IO.Path]::GetTempPath()) ('d65-release-smoke-' + [guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $scratch | Out-Null
function Run-Cli([string[]]$Arguments, [int]$ExpectedExit = 0) {
    $info = [System.Diagnostics.ProcessStartInfo]::new($exe)
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($arg in $Arguments) { $info.ArgumentList.Add($arg) }
    $process = [System.Diagnostics.Process]::Start($info)
    $out = $process.StandardOutput.ReadToEnd()
    $err = $process.StandardError.ReadToEnd()
    $process.WaitForExit()
    if ($process.ExitCode -ne $ExpectedExit) { throw "Unexpected exit $($process.ExitCode): $out $err" }
    Write-Output "PASS CLI $($Arguments[0]): $($out.Trim()) $($err.Trim())"
}
$image = Join-Path $scratch 'Unicode é disk.d65'
Run-Cli @('create', 'd65', $image, 'SMOKE TEST')
Run-Cli @('add', $image, (Join-Path $sourceRoot 'Images/HELLO.prg'), 'HELLO')
Run-Cli @('list', $image)
Run-Cli @('add', $image, (Join-Path $sourceRoot 'Images/HELLO.prg'), 'HELLO') 1
$d81 = Join-Path $scratch 'control.d81'
$roundtrip = Join-Path $scratch 'roundtrip.d65'
Run-Cli @('convert', $image, $d81, 'd81')
Run-Cli @('convert', $d81, $roundtrip, 'd65')
if ((Get-FileHash $image).Hash -ne (Get-FileHash $roundtrip).Hash) { throw 'D65/D81 roundtrip mismatch' }
Run-Cli @('export-sd', $image, (Join-Path $scratch 'REFUSED.D65')) 1
if (Test-Path (Join-Path $scratch 'REFUSED.D65')) { throw 'Invalid SD path was written' }
Run-Cli @('launch-compat', (Join-Path $scratch 'hook.d65'), $d81)
Write-Output 'PASS release binary: Unicode/spaced paths, create, import, list, duplicate refusal, lossless conversion, unsafe SD-path refusal, LOAD-compatible compilation.'

# Independent reader: does not call Rust's FAT32 engine. Only opens a regular
# simulated partition image, never a Windows volume or physical device.
$fatPath = (Resolve-Path -LiteralPath $FixturePath).Path
if (!$fatPath.EndsWith('.img', [StringComparison]::OrdinalIgnoreCase) -or $fatPath.StartsWith('\\.\') -or $fatPath.StartsWith('\\?\Volume{')) { throw 'Supply a regular simulated .img file, never a device path' }
$f = [System.IO.File]::OpenRead($fatPath)
function Read-At([long]$Offset, [int]$Count) {
    [void]$f.Seek($Offset, [System.IO.SeekOrigin]::Begin)
    $buffer = [byte[]]::new($Count)
    $done = 0
    while ($done -lt $Count) { $n = $f.Read($buffer, $done, $Count - $done); if (!$n) { throw 'Unexpected EOF' }; $done += $n }
    return ,$buffer
}
try {
    $boot = Read-At 0 512
    $reserved = [BitConverter]::ToUInt16($boot, 14)
    $fatSectors = [BitConverter]::ToUInt32($boot, 36)
    $clusterBytes = 512 * $boot[13]
    $dataOffset = ($reserved + 2 * $fatSectors) * 512L
    $fat1 = Read-At ($reserved * 512L) ($fatSectors * 512)
    $fat2 = Read-At (($reserved + $fatSectors) * 512L) ($fatSectors * 512)
    if ([Convert]::ToHexString($fat1) -ne [Convert]::ToHexString($fat2)) { throw 'FAT copies differ' }
    $root = Read-At $dataOffset $clusterBytes
    for ($slot = 0; $slot -lt 2; $slot++) {
        $entry = $slot * 32
        $name = [Text.Encoding]::ASCII.GetString($root, $entry, 11)
        $cluster = ([long][BitConverter]::ToUInt16($root, $entry + 20) -shl 16) -bor [BitConverter]::ToUInt16($root, $entry + 26)
        $size = [BitConverter]::ToUInt32($root, $entry + 28)
        $expectedClusters = [int][Math]::Ceiling($size / $clusterBytes)
        $first = $cluster
        for ($index = 0; $index -lt $expectedClusters; $index++) {
            if ($cluster -ne $first + $index) { throw 'Fragmented chain' }
            $next = [BitConverter]::ToUInt32($fat1, [int]($cluster * 4)) -band 0x0fffffff
            if ($index -eq $expectedClusters - 1) { if ($next -lt 0x0ffffff8) { throw 'Missing terminal FAT entry' } }
            else { $cluster = $next }
        }
        $actual = Read-At ($dataOffset + ($first - 2) * $clusterBytes) $size
        $format = if ($slot -eq 0) { 'd65' } else { 'd81' }
        $label = if ($slot -eq 0) { 'DIRECT TEST' } else { 'CONTROL' }
        $expected = Join-Path $scratch "expected.$format"
        Run-Cli @('create', $format, $expected, $label)
        if ([Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($actual)) -ne (Get-FileHash $expected).Hash) { throw 'Exported bytes differ' }
        Write-Output "PASS independent FAT reader: $name, $size bytes, $expectedClusters consecutive clusters starting at $first, matching FATs and exact payload."
    }
} finally { $f.Dispose() }

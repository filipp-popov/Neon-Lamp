param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$ElfPath
)

$ErrorActionPreference = "Stop"

$serial = "003B002B3331511134333834"
$port = 61234
$server = "C:\ST\STM32CubeIDE_1.14.0\STM32CubeIDE\plugins\com.st.stm32cube.ide.mcu.externaltools.stlink-gdb-server.win32_2.1.100.202310302101\tools\bin\ST-LINK_gdbserver.exe"
$gdb = "C:\ST\STM32CubeIDE_1.14.0\STM32CubeIDE\plugins\com.st.stm32cube.ide.mcu.externaltools.gnu-tools-for-stm32.11.3.rel1.win32_1.1.100.202309141235\tools\bin\arm-none-eabi-gdb.exe"
$cubeProgrammerBin = "C:\Program Files\STMicroelectronics\STM32Cube\STM32CubeProgrammer\bin"

$resolvedElf = Resolve-Path -LiteralPath $ElfPath -ErrorAction Stop
$ElfPath = $resolvedElf.Path

if (-not (Test-Path -LiteralPath $ElfPath)) {
    throw "ELF file not found: $ElfPath"
}

$tempDir = Join-Path $env:TEMP "neon-lamp-stlink"
New-Item -ItemType Directory -Path $tempDir -Force | Out-Null

$serverLog = Join-Path $tempDir "stlink-gdbserver.log"
$gdbScript = Join-Path $tempDir "flash.gdb"

@"
set pagination off
set confirm off
target extended-remote localhost:$port
monitor reset
load
monitor reset
detach
quit
"@ | Set-Content -LiteralPath $gdbScript -Encoding ASCII

$serverArgs = "-e -p $port -d -i $serial --frequency 100 -cp `"$cubeProgrammerBin`" -f `"$serverLog`" -l 31"

$serverProcess = Start-Process -FilePath $server -ArgumentList $serverArgs -WindowStyle Hidden -PassThru

try {
    $deadline = (Get-Date).AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 250
        $client = New-Object Net.Sockets.TcpClient
        try {
            $connect = $client.BeginConnect("127.0.0.1", $port, $null, $null)
            if ($connect.AsyncWaitHandle.WaitOne(200)) {
                $client.EndConnect($connect)
                break
            }
        } catch {
        } finally {
            $client.Close()
        }

        if ($serverProcess.HasExited) {
            throw "ST-LINK_gdbserver exited early. See $serverLog"
        }
    } while ((Get-Date) -lt $deadline)

    if ((Get-Date) -ge $deadline) {
        throw "Timed out waiting for ST-LINK_gdbserver on port $port. See $serverLog"
    }

    & $gdb -q -batch -x $gdbScript $ElfPath
    if ($LASTEXITCODE -ne 0) {
        throw "arm-none-eabi-gdb failed with exit code $LASTEXITCODE"
    }
} finally {
    if ($serverProcess -and -not $serverProcess.HasExited) {
        Stop-Process -Id $serverProcess.Id -Force
    }
}

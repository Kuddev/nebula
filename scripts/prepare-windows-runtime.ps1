[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string] $Destination,
    [string] $ArchivePath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$OutputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

# Reuse the Microsoft 1.22 runtime pair already shipped with Pebrel's predecessor.
# Only these two redistributables are extracted, never the older application.
$archiveHash = '9B2413144C0434E29749CCBD5C2B0F93E930DAFD22634DD70D34B763037E0DA4'
$sourceUrl = 'https://github.com/Kuddev/pebrel/releases/download/v1.5.0/NebulaTerminal-v1.5.0-windows-x64.zip'
$expected = [ordered]@{
    'conpty.dll' = '375BFB0479B6C53836AB307E3F9FD17BEDBD733F2E9690943D0F12E72FB80777'
    'OpenConsole.exe' = '55B18996761C88C351820E82508E05AB0EC2194AEEAD20724FDFAEEDEC076EF4'
}

if ([string]::IsNullOrWhiteSpace($ArchivePath)) {
    $ArchivePath = Join-Path ([System.IO.Path]::GetTempPath()) 'pebrel-conpty-source-v1.5.0.zip'
    if (-not (Test-Path -LiteralPath $ArchivePath -PathType Leaf)) {
        Invoke-WebRequest -Uri $sourceUrl -OutFile $ArchivePath -UseBasicParsing -TimeoutSec 120
    }
}
if ((Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash -ne $archiveHash) {
    throw 'The pinned runtime source archive failed SHA256 verification.'
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::OpenRead((Resolve-Path $ArchivePath).Path)
try {
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    foreach ($name in $expected.Keys) {
        $target = Join-Path $Destination $name
        if ((Test-Path -LiteralPath $target -PathType Leaf) -and
            (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -eq $expected[$name]) {
            continue
        }
        $entry = $archive.GetEntry("runtime\$name")
        if ($null -eq $entry) { throw "Runtime archive is missing runtime/$name" }
        $temporary = "$target.$([guid]::NewGuid().ToString('N')).tmp"
        try {
            $inputStream = $entry.Open()
            try {
                $outputStream = [System.IO.File]::Create($temporary)
                try { $inputStream.CopyTo($outputStream) } finally { $outputStream.Dispose() }
            } finally { $inputStream.Dispose() }
            if ((Get-FileHash -LiteralPath $temporary -Algorithm SHA256).Hash -ne $expected[$name]) {
                throw "The pinned $name failed SHA256 verification."
            }
            Move-Item -LiteralPath $temporary -Destination $target -Force
        } finally {
            if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary }
        }
    }
} finally { $archive.Dispose() }

foreach ($name in $expected.Keys) {
    Write-Output "$name SHA256 $($expected[$name])"
}

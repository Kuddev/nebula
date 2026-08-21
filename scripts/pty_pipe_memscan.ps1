[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][int]$ProcessId,
    [Parameter(Mandatory = $true)][string]$NeedleFile,
    [int]$MaxNeedles = 3,
    [int]$MaxHits = 12,
    [int]$ContextBytes = 260
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$source = @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public class NebMemScan {
    const uint PROCESS_QUERY_INFORMATION = 0x0400;
    const uint PROCESS_VM_READ = 0x0010;
    const uint MEM_COMMIT = 0x1000;
    const uint MEM_PRIVATE = 0x20000;
    const uint PAGE_GUARD = 0x100;
    const uint PAGE_NOACCESS = 0x01;

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool ReadProcessMemory(IntPtr h, IntPtr addr, byte[] buf, IntPtr size, out IntPtr read);
    [DllImport("kernel32.dll")]
    static extern IntPtr VirtualQueryEx(IntPtr h, IntPtr addr, out MEMORY_BASIC_INFORMATION mbi, IntPtr len);

    [StructLayout(LayoutKind.Sequential)]
    public struct MEMORY_BASIC_INFORMATION {
        public IntPtr BaseAddress;
        public IntPtr AllocationBase;
        public uint AllocationProtect;
        public uint __alignment1;
        public IntPtr RegionSize;
        public uint State;
        public uint Protect;
        public uint Type;
        public uint __alignment2;
    }

    public class Hit {
        public string Needle;
        public ulong Address;
        public ulong RegionBase;
        public ulong RegionSize;
        public uint Protect;
        public string Context;
    }

    public static long ScannedBytes;
    public static long ScannedRegions;

    static int IndexOf(byte[] hay, int hayLen, byte[] pat, int from) {
        int last = hayLen - pat.Length;
        for (int i = from; i <= last; i++) {
            int j = 0;
            while (j < pat.Length && hay[i + j] == pat[j]) j++;
            if (j == pat.Length) return i;
        }
        return -1;
    }

    static string Render(byte[] buf, int center, int len, int contextBytes) {
        int start = Math.Max(0, center - contextBytes / 2);
        int end = Math.Min(len, center + contextBytes / 2);
        var sb = new System.Text.StringBuilder();
        for (int i = start; i < end; i++) {
            byte b = buf[i];
            if (b == 0x1b) sb.Append("<ESC>");
            else if (b == 0x0a) sb.Append("<LF>");
            else if (b == 0x0d) sb.Append("<CR>");
            else if (b == 0x07) sb.Append("<BEL>");
            else if (b < 0x20) sb.Append('.');
            else if (b < 0x7f) sb.Append((char)b);
            else sb.Append('·');
        }
        return sb.ToString();
    }

    public static List<Hit> Scan(int pid, string[] needles, int maxHits, int contextBytes) {
        var results = new List<Hit>();
        var pats = new List<byte[]>();
        foreach (var n in needles) pats.Add(System.Text.Encoding.UTF8.GetBytes(n));

        IntPtr h = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid);
        if (h == IntPtr.Zero) throw new Exception("OpenProcess failed: " + Marshal.GetLastWin32Error());

        try {
            ulong addr = 0;
            ulong limit = 0x7FFFFFFFFFFF;
            int mbiSize = Marshal.SizeOf(typeof(MEMORY_BASIC_INFORMATION));
            byte[] buf = null;

            while (addr < limit && results.Count < maxHits) {
                MEMORY_BASIC_INFORMATION mbi;
                if (VirtualQueryEx(h, (IntPtr)(long)addr, out mbi, (IntPtr)mbiSize) == IntPtr.Zero) break;
                ulong regionSize = (ulong)mbi.RegionSize.ToInt64();
                if (regionSize == 0) break;
                ulong regionBase = (ulong)mbi.BaseAddress.ToInt64();

                bool readable = mbi.State == MEM_COMMIT
                    && mbi.Type == MEM_PRIVATE
                    && (mbi.Protect & PAGE_GUARD) == 0
                    && (mbi.Protect & PAGE_NOACCESS) == 0
                    && mbi.Protect != 0;

                if (readable) {
                    // 分块读，块间保留最长 needle - 1 字节的重叠，避免跨块漏匹配。
                    int overlap = 0;
                    foreach (var p in pats) overlap = Math.Max(overlap, p.Length);
                    overlap = Math.Max(0, overlap - 1);

                    int chunk = 4 * 1024 * 1024;
                    if (buf == null || buf.Length < chunk + overlap) buf = new byte[chunk + overlap];

                    ulong off = 0;
                    while (off < regionSize && results.Count < maxHits) {
                        int want = (int)Math.Min((ulong)chunk, regionSize - off);
                        IntPtr read;
                        if (!ReadProcessMemory(h, (IntPtr)(long)(regionBase + off), buf, (IntPtr)want, out read)) break;
                        int got = read.ToInt32();
                        if (got <= 0) break;
                        ScannedBytes += got;

                        for (int pi = 0; pi < pats.Count; pi++) {
                            int at = 0;
                            while (results.Count < maxHits) {
                                at = IndexOf(buf, got, pats[pi], at);
                                if (at < 0) break;
                                var hit = new Hit();
                                hit.Needle = needles[pi];
                                hit.Address = regionBase + off + (ulong)at;
                                hit.RegionBase = regionBase;
                                hit.RegionSize = regionSize;
                                hit.Protect = mbi.Protect;
                                hit.Context = Render(buf, at, got, contextBytes);
                                results.Add(hit);
                                at += pats[pi].Length;
                            }
                        }

                        if (off + (ulong)got >= regionSize) break;
                        if (got <= overlap) break;
                        // 回退 overlap 字节，保证跨 4MiB 块边界的匹配不被切断。
                        off += (ulong)(got - overlap);
                    }
                    ScannedRegions++;
                }

                addr = regionBase + regionSize;
            }
        }
        finally {
            CloseHandle(h);
        }
        return results;
    }
}
'@

Add-Type -TypeDefinition $source -Language CSharp | Out-Null

$all = Get-Content -LiteralPath $NeedleFile -Encoding UTF8 | Where-Object { $_.Trim().Length -gt 0 }
# 取最长的几个：UTF-8 编码后越长，误报概率越低。
$needles = @($all | Sort-Object -Property Length -Descending | Select-Object -First $MaxNeedles)

Write-Output ("scanning pid={0}  needles={1}  (lengths: {2})" -f $ProcessId, $needles.Count,
    (($needles | ForEach-Object { $_.Length }) -join ','))

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$hits = [NebMemScan]::Scan($ProcessId, $needles, $MaxHits, $ContextBytes)
$sw.Stop()

Write-Output ("scanned {0:N0} MiB of MEM_PRIVATE across {1:N0} regions in {2:N1}s" -f `
    ([NebMemScan]::ScannedBytes / 1MB), [NebMemScan]::ScannedRegions, $sw.Elapsed.TotalSeconds)
Write-Output ("HITS: {0}" -f $hits.Count)

$i = 0
foreach ($hit in $hits) {
    $i++
    Write-Output ''
    Write-Output ("--- hit #{0}  addr=0x{1:X}  region=0x{2:X}+{3:N0}B  protect=0x{4:X}" -f `
        $i, $hit.Address, $hit.RegionBase, $hit.RegionSize, $hit.Protect)
    Write-Output ("    ctx: {0}" -f $hit.Context)
}

#Requires -Version 5.1
<#
    .SYNOPSIS
        Names the open files held by chosen processes, so a "file is already in use" can be pinned
        on whoever is actually holding it.

    .DESCRIPTION
        Run this at the moment an application refuses to start because a file is in use, BEFORE
        closing anything -- once the holder exits, the evidence goes with it.

    .EXAMPLE
        .\who-holds-claude.ps1
        .\who-holds-claude.ps1 -Match Cursor -Process Cursor
        .\who-holds-claude.ps1 -Match '' -Process agent-status-tray   # everything the tray holds
#>
[CmdletBinding()]
param(
    # Path fragment to match open files against, case-insensitive. Empty matches everything.
    [string] $Match = 'Claude',

    # Which processes to inspect. Scanning every process means naming hundreds of thousands of
    # handles; a handful is bounded work and is the question actually being asked.
    [string[]] $Process = @('agent-status-tray', 'claude', 'Cursor')
)

$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Threading;

public class HandleScan {
  [DllImport("ntdll.dll")] static extern int NtQuerySystemInformation(int c, IntPtr b, int l, ref int r);
  [DllImport("ntdll.dll")] static extern int NtQueryObject(IntPtr h, int c, IntPtr b, int l, ref int r);
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(int a, bool i, int p);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool DuplicateHandle(IntPtr sp, IntPtr sh, IntPtr tp, out IntPtr th, int a, bool i, int o);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
  [DllImport("kernel32.dll")] static extern int GetCurrentProcessId();

  [StructLayout(LayoutKind.Sequential)]
  struct Entry {
    public IntPtr Object; public IntPtr ProcessId; public IntPtr Handle;
    public int GrantedAccess; public short Backtrace; public short TypeIndex;
    public int Attributes; public int Reserved;
  }

  public class Hit { public int Pid; public string Path; }

  // Naming a handle can never return -- a synchronous pipe is of type File just as a file is, and
  // NtQueryObject on one blocks forever. Every name is therefore fetched on a thread that is given
  // a deadline and abandoned if it misses it. This is why the scan is scoped to a few processes:
  // one throwaway thread per handle is affordable over hundreds, not over hundreds of thousands.
  static string NameWithTimeout(IntPtr dup, int ms) {
    string result = null;
    var t = new Thread(() => {
      IntPtr nb = Marshal.AllocHGlobal(4096);
      try {
        int r = 0;
        if (NtQueryObject(dup, 1, nb, 4096, ref r) == 0)
          result = Marshal.PtrToStringUni((IntPtr)(nb.ToInt64() + IntPtr.Size * 2));
      } catch {} finally { Marshal.FreeHGlobal(nb); }
    });
    t.IsBackground = true;
    t.Start();
    if (!t.Join(ms)) { try { t.Abort(); } catch {} return null; }
    return result;
  }

  public static List<Hit> Run(string match, int[] pids, out int examined) {
    var hits = new List<Hit>();
    examined = 0;

    // Opened before the snapshot, or its handle will not appear in it.
    string probePath = System.IO.Path.Combine(System.IO.Path.GetTempPath(), "handle-probe.tmp");
    System.IO.File.WriteAllText(probePath, "probe");
    var probeStream = System.IO.File.OpenRead(probePath);
    IntPtr probe = probeStream.SafeFileHandle.DangerousGetHandle();

    int len = 1 << 20, ret = 0;
    IntPtr buf = IntPtr.Zero;
    for (int i = 0; i < 12; i++) {
      buf = Marshal.AllocHGlobal(len);
      if (NtQuerySystemInformation(64, buf, len, ref ret) == 0) break;
      Marshal.FreeHGlobal(buf); buf = IntPtr.Zero; len *= 2;
    }
    if (buf == IntPtr.Zero) { probeStream.Close(); return hits; }

    long count = Marshal.ReadIntPtr(buf).ToInt64();
    int size = Marshal.SizeOf(typeof(Entry));
    IntPtr baseP = (IntPtr)(buf.ToInt64() + IntPtr.Size * 2);
    IntPtr me = GetCurrentProcess();
    int mine = GetCurrentProcessId();

    // The File type index is not fixed across Windows builds, so it is discovered rather than
    // assumed: without it every handle of every type would be named.
    short fileType = -1;
    for (long i = 0; i < count; i++) {
      var e = (Entry)Marshal.PtrToStructure((IntPtr)(baseP.ToInt64() + i * size), typeof(Entry));
      if ((int)e.ProcessId == mine && e.Handle == probe) { fileType = e.TypeIndex; break; }
    }
    probeStream.Close();
    try { System.IO.File.Delete(probePath); } catch {}
    if (fileType < 0) throw new Exception("could not identify the File handle type");

    var procs = new Dictionary<int, IntPtr>();
    for (long i = 0; i < count; i++) {
      var e = (Entry)Marshal.PtrToStructure((IntPtr)(baseP.ToInt64() + i * size), typeof(Entry));
      if (e.TypeIndex != fileType) continue;
      int pid = (int)e.ProcessId;
      if (pid == mine || Array.IndexOf(pids, pid) < 0) continue;

      IntPtr hp;
      if (!procs.TryGetValue(pid, out hp)) { hp = OpenProcess(0x40, false, pid); procs[pid] = hp; }
      if (hp == IntPtr.Zero) continue;

      IntPtr dup;
      if (!DuplicateHandle(hp, e.Handle, me, out dup, 0, false, 2)) continue;
      examined++;
      string s = NameWithTimeout(dup, 200);
      CloseHandle(dup);
      if (string.IsNullOrEmpty(s)) continue;
      if (match.Length == 0 || s.IndexOf(match, StringComparison.OrdinalIgnoreCase) >= 0)
        hits.Add(new Hit { Pid = pid, Path = s });
    }
    foreach (var h in procs.Values) if (h != IntPtr.Zero) CloseHandle(h);
    Marshal.FreeHGlobal(buf);
    return hits;
  }
}
'@

$targets = Get-Process -Name $Process -ErrorAction SilentlyContinue
if (-not $targets) {
    Write-Host "None of these are running: $($Process -join ', ')" -ForegroundColor Yellow
    return
}
Write-Host ("Inspecting {0} process(es) for open files matching '{1}'..." -f $targets.Count, $Match) -ForegroundColor Cyan
$targets | Group-Object ProcessName | ForEach-Object { "  {0,-24} {1}" -f $_.Name, $_.Count }

$examined = 0
$hits = [HandleScan]::Run($Match, [int[]]($targets.Id), [ref]$examined)
Write-Host ("{0} file handle(s) named, {1} match." -f $examined, $hits.Count)

if ($hits.Count -eq 0) {
    Write-Host "None of those processes holds an open file matching '$Match'." -ForegroundColor Green
    return
}

$rows = $hits | ForEach-Object {
    $p = Get-Process -Id $_.Pid -ErrorAction SilentlyContinue
    [pscustomobject]@{ Pid = $_.Pid; Process = $(if ($p) { $p.ProcessName } else { '(gone)' }); Path = $_.Path }
}
$rows | Sort-Object Process, Path | Format-Table -AutoSize -Wrap

Write-Host "Holders by process:" -ForegroundColor Yellow
$rows | Group-Object Process | Sort-Object Count -Descending |
    ForEach-Object { "  {0,-24} {1} handle(s)" -f $_.Name, $_.Count }

# The tray lives in a folder called claude-tray, so matching the word "claude" is not enough to
# accuse it -- the verdict asks whether it holds anything inside the two folders that are actually
# Claude's, and its own working directory is neither.
$claudeData = @(
    [regex]::Escape((Join-Path $env:APPDATA 'Claude')),
    [regex]::Escape((Join-Path $env:USERPROFILE '.claude'))
) -join '|'
$culprits = $rows | Where-Object { $_.Process -eq 'agent-status-tray' -and $_.Path -match $claudeData }

Write-Host ""
if ($culprits) {
    Write-Host "agent-status-tray IS holding a file of Claude's. That is a bug -- please report:" -ForegroundColor Red
    $culprits | Format-Table -AutoSize -Wrap
}
else {
    Write-Host "agent-status-tray holds nothing inside Claude's own folders." -ForegroundColor Green
    $others = $rows | Where-Object { $_.Process -ne 'agent-status-tray' -and $_.Path -match $claudeData }
    if ($others) {
        Write-Host ("Those folders are held by: {0}" -f (($others.Process | Sort-Object -Unique) -join ', '))
    }
}

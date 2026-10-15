using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.Cryptography;

namespace Vyrtel.Baseline;

/// <summary>
/// Detects the hardware, software and build facts a baseline must state. Every probe is best
/// effort: anything it cannot read is reported as "unknown" rather than failing the run.
/// </summary>
public static class HostProbe
{
    public const string Unknown = "unknown";

    public static HostInfo Describe(string dataDir, string notes) => new(
        Os: RuntimeInformation.OSDescription,
        Architecture: RuntimeInformation.OSArchitecture.ToString(),
        CpuModel: CpuModel(),
        LogicalCores: Environment.ProcessorCount,
        TotalMemoryBytes: GC.GetGCMemoryInfo().TotalAvailableMemoryBytes,
        Disk: Disk(dataDir),
        Runtime: RuntimeInformation.FrameworkDescription,
        Notes: notes);

    public static BuildFacts Build(string profile, string? executable, string? repoRoot)
    {
        var size = executable is not null && File.Exists(executable) ? new FileInfo(executable).Length : 0;
        string? sha = null;
        if (size > 0)
        {
            using var stream = File.OpenRead(executable!);
            sha = Convert.ToHexStringLower(SHA256.HashData(stream));
        }
        var commit = repoRoot is null ? null : Run("git", ["-C", repoRoot, "rev-parse", "HEAD"]);
        // Untracked files under target/ are ignored, so this only flags real source changes.
        var status = repoRoot is null ? null : Run("git", ["-C", repoRoot, "status", "--porcelain", "--untracked-files=no"]);
        return new BuildFacts(
            profile,
            size,
            sha,
            commit,
            commit is null ? null : !string.IsNullOrWhiteSpace(status),
            Run("rustc", ["--version"]));
    }

    /// <summary>
    /// SHA-256 of a text file with CRLF read as LF, so a Windows checkout (core.autocrlf) and a
    /// Linux one of the same pinned configuration get the same hash.
    /// </summary>
    public static string Sha256OfText(string path)
    {
        var text = File.ReadAllText(path).Replace("\r\n", "\n");
        return Convert.ToHexStringLower(SHA256.HashData(System.Text.Encoding.UTF8.GetBytes(text)));
    }

    /// <summary>The CPU model string from /proc/cpuinfo, the Windows registry or sysctl.</summary>
    public static string CpuModel()
    {
        try
        {
            if (OperatingSystem.IsLinux() && File.Exists("/proc/cpuinfo"))
            {
                return ParseCpuInfo(File.ReadAllText("/proc/cpuinfo")) ?? Unknown;
            }
            if (OperatingSystem.IsWindows())
            {
                using var key = Microsoft.Win32.Registry.LocalMachine.OpenSubKey(@"HARDWARE\DESCRIPTION\System\CentralProcessor\0");
                return (key?.GetValue("ProcessorNameString") as string)?.Trim() ?? Unknown;
            }
            if (OperatingSystem.IsMacOS())
            {
                return Run("sysctl", ["-n", "machdep.cpu.brand_string"]) ?? Unknown;
            }
        }
        catch
        {
            // fall through to unknown
        }
        return Unknown;
    }

    /// <summary>
    /// The first <c>model name</c> in /proc/cpuinfo. Many arm64 kernels omit it, so fall back to
    /// the implementer and part numbers, which still identify the core type.
    /// </summary>
    public static string? ParseCpuInfo(string cpuinfo)
    {
        string? implementer = null, part = null;
        foreach (var line in cpuinfo.Split('\n'))
        {
            var colon = line.IndexOf(':');
            if (colon < 0)
            {
                continue;
            }
            var key = line[..colon].Trim();
            var value = line[(colon + 1)..].Trim();
            switch (key)
            {
                case "model name" or "Model" when value.Length > 0:
                    return value;
                case "CPU implementer":
                    implementer ??= value;
                    break;
                case "CPU part":
                    part ??= value;
                    break;
            }
        }
        return implementer is null ? null : $"CPU implementer {implementer}, part {part ?? "?"}";
    }

    /// <summary>The mounted volume holding <paramref name="path"/>: the drive whose root is its longest prefix.</summary>
    public static DiskInfo? Disk(string path)
    {
        try
        {
            var full = Path.GetFullPath(path);
            var comparison = OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal;
            var drive = DriveInfo.GetDrives()
                .Where(d => d.IsReady && full.StartsWith(d.RootDirectory.FullName, comparison))
                .MaxBy(d => d.RootDirectory.FullName.Length);
            return drive is null
                ? null
                : new DiskInfo(drive.RootDirectory.FullName, drive.DriveFormat, drive.DriveType.ToString(), drive.TotalSize, drive.AvailableFreeSpace);
        }
        catch
        {
            return null;
        }
    }

    /// <summary>Runs a short command and returns its trimmed standard output, or null if it fails.</summary>
    private static string? Run(string file, string[] args)
    {
        try
        {
            var info = new ProcessStartInfo(file)
            {
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                UseShellExecute = false,
                CreateNoWindow = true,
            };
            foreach (var a in args)
            {
                info.ArgumentList.Add(a);
            }
            using var p = Process.Start(info);
            if (p is null)
            {
                return null;
            }
            var output = p.StandardOutput.ReadToEndAsync();
            if (!p.WaitForExit(10_000))
            {
                p.Kill(entireProcessTree: true);
                return null;
            }
            return p.ExitCode == 0 ? output.Result.Trim() : null;
        }
        catch
        {
            return null;
        }
    }
}

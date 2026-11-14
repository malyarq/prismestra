# Package-root entry point. Compatible with Windows PowerShell 5.1.
[CmdletBinding()]
param(
    [switch]$Connected,
    [switch]$ClassicEditor,
    [switch]$Check
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)

function Assert-StateAccess([string]$Path) {
    $directory = $Path
    while (-not [System.IO.Directory]::Exists($directory)) {
        if ([System.IO.File]::Exists($directory)) {
            throw "State path is a file: $directory. Choose a writable extraction folder."
        }
        $directory = [System.IO.Path]::GetDirectoryName($directory)
        if (-not $directory) { throw "Cannot find a parent directory for state: $Path" }
    }
    # Writability is probed only on launch; -Check must not create anything.
}

function ConvertTo-NativeArgument([string]$Value) {
    # Windows CommandLineToArgvW/CRT quoting, including trailing backslashes.
    '"' + [regex]::Replace([regex]::Replace($Value, '(\\*)"', '$1$1\"'), '(\\+)$', '$1$1') + '"'
}

$root = [System.IO.Path]::GetFullPath($PSScriptRoot)
$exe = Join-Path $root 'Prismestra.exe'
$profile = Join-Path $root 'data\profile.json'
$state = Join-Path $root 'data\state'
$bindings = Join-Path $state 'lighting-local.json'
$plan = [ordered]@{
    ok = $false
    mode = $(if ($Connected) { 'connected' } else { 'preview' })
    executable = $exe
    working_directory = $root
    state_dir = $state
    profile = $null
    arguments = @()
    state_write_check = 'not tested by nonmutating Check; actual write probe on launch'
    error = $null
}

try {
    if (-not [System.IO.File]::Exists($exe)) {
        throw "Missing Prismestra.exe: $exe. Extract the complete release ZIP first."
    }
    if ([System.IO.Directory]::Exists($profile)) { throw "Profile path is a directory: $profile" }
    $hasProfile = [System.IO.File]::Exists($profile)
    if ($Connected -and -not $hasProfile) {
        throw "Missing saved profile: $profile. Open Prismestra in preview and save your profile there first."
    }
    if ($Connected -and -not [System.IO.File]::Exists($bindings)) {
        throw "Missing saved local connections: $bindings. Configure and save connections in Prismestra first."
    }
    Assert-StateAccess $state
    $nativeArguments = @('--state-dir', $state)
    if ($hasProfile) {
        $plan.profile = $profile
        $nativeArguments += @('--project', $profile)
    }
    if ($Connected) { $nativeArguments += '--start-connected' }
    if ($ClassicEditor) { $nativeArguments += '--classic-editor' }
    $plan.arguments = $nativeArguments
    $plan.ok = $true
    if ($Check) {
        $plan | ConvertTo-Json -Depth 4
        exit 0
    }
    $probe = Join-Path $state ('.launcher-write-' + [guid]::NewGuid().ToString('N'))
    try {
        [System.IO.Directory]::CreateDirectory($state) | Out-Null
        $stream = [System.IO.File]::Open($probe, 'CreateNew', 'Write', 'None')
        $stream.Dispose()
    } catch {
        throw "Cannot write portable state at $state. Extract the ZIP into a writable folder. $($_.Exception.Message)"
    } finally {
        if ([System.IO.File]::Exists($probe)) { [System.IO.File]::Delete($probe) }
    }
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = $exe
    $start.WorkingDirectory = $root
    $start.UseShellExecute = $false
    $start.Arguments = ($nativeArguments | ForEach-Object { ConvertTo-NativeArgument $_ }) -join ' '
    $process = [System.Diagnostics.Process]::Start($start)
    if (-not $process.WaitForExit(1500)) {
        $process.Dispose()
        exit 0
    }
    $exitCode = $process.ExitCode
    $process.Dispose()
    if ($exitCode -ne 0) { [Console]::Error.WriteLine("Prismestra exited with code $exitCode. Review the application error above.") }
    exit $exitCode
} catch {
    if ($Check) {
        $plan.ok = $false
        $plan.error = $_.Exception.Message
        $plan | ConvertTo-Json -Depth 4
    } else {
        [Console]::Error.WriteLine("Prismestra launcher: " + $_.Exception.Message)
    }
    exit 1
}

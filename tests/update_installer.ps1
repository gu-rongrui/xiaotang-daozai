# 使用独立目录和最小测试程序验证更新，不启动或替换日常使用的岛仔。
param([switch]$EncryptedCache)
$ErrorActionPreference = 'Stop'
$project = Split-Path $PSScriptRoot -Parent
$ast=[Management.Automation.Language.Parser]::ParseFile((Join-Path $project 'installer\update.ps1'),[ref]$null,[ref]$null)
$readError=$ast.Find({param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-UpdateReadError'},$true)
if(-not $readError){throw 'Missing update read-error handler'}
. ([scriptblock]::Create($readError.Extent.Text))
foreach($code in @(225,226)){
    $failure=[IO.IOException]::new('simulated security denial',(-2147024896+$code))
    $message=Get-UpdateReadError ([Reflection.TargetInvocationException]::new($failure))
    if($message -notmatch '安全防护拦截' -or $message -match '版本不匹配'){throw "Security denial was reported as a version mismatch: $code"}
    Write-Output "通过：security-read-error-$code"
}
$testRoot = Join-Path $project ('target\update-test-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testRoot | Out-Null
$oldLocal = $env:LOCALAPPDATA
try {
    $env:LOCALAPPDATA = if ($EncryptedCache) { Join-Path $oldLocal ('DaozaiUpdateTest-' + [Guid]::NewGuid().ToString('N')) } else { Join-Path $testRoot 'local' }
    New-Item -ItemType Directory -Path $env:LOCALAPPDATA -Force | Out-Null
    if ($EncryptedCache -and -not ((Get-Item -LiteralPath $env:LOCALAPPDATA).Attributes -band [IO.FileAttributes]::Encrypted)) { throw '测试缓存目录必须启用 EFS 加密' }
    $compiler = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
    $source = Join-Path $testRoot 'fixture.cs'
    '[assembly:System.Reflection.AssemblyFileVersion("9.8.7")] class Fixture { static void Main() { System.IO.File.WriteAllText(System.IO.Path.Combine(System.AppDomain.CurrentDomain.BaseDirectory,"started.txt"),"ok"); } }' | Set-Content -LiteralPath $source
    $fixture = Join-Path $testRoot 'fixture.exe'
    & $compiler /nologo /target:winexe "/out:$fixture" $source
    if ($LASTEXITCODE -ne 0) { throw '测试程序编译失败' }
    $installer = Join-Path $testRoot 'apply.ps1'
    [IO.File]::WriteAllText($installer, [IO.File]::ReadAllText((Join-Path $project 'installer\update.ps1')), [Text.UTF8Encoding]::new($true))
    $targetDir = Join-Path $testRoot 'installed'
    New-Item -ItemType Directory -Path $targetDir | Out-Null
    $target = Join-Path $targetDir 'daozai.exe'
    $data = Join-Path $targetDir 'settings.json'
    Set-Content -LiteralPath $data -Value 'keep-user-data'
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    foreach ($case in @('success','inherited-shell','bad-hash','wrong-version','missing-version','traversal','upstream-package')) {
        Remove-Item -LiteralPath (Join-Path $targetDir 'started.txt') -ErrorAction SilentlyContinue
        for ($attempt=0; ; $attempt++) {
            try { Set-Content -LiteralPath $target -Value 'previous-program'; break }
            catch [IO.IOException] { if ($attempt -ge 49) { throw }; Start-Sleep -Milliseconds 100 }
        }
        $before = (Get-FileHash -LiteralPath $target).Hash
        $work = Join-Path $env:LOCALAPPDATA ('DaozaiPersonal\Updates\' + $case)
        New-Item -ItemType Directory -Path $work -Force | Out-Null
        $archive = Join-Path $work 'update.zip'
        $zip = [IO.Compression.ZipFile]::Open($archive, [IO.Compression.ZipArchiveMode]::Create)
        $version = if ($case -eq 'wrong-version') {'9.8.8'} else {'9.8.7'}
        $entry = if ($case -eq 'traversal') {'../daozai.exe'} elseif ($case -eq 'upstream-package') {"Daozai-v$version-windows-x64/daozai.exe"} else {"DaozaiPersonal-v$version-windows-x64/daozai.exe"}
        if($case -eq 'missing-version'){
            $writer=[IO.StreamWriter]::new($zip.CreateEntry($entry).Open())
            try{$writer.Write('MZ fixture without version information')}finally{$writer.Dispose()}
        }else{[IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $fixture, $entry) | Out-Null}
        $zip.Dispose()
        Set-Content -LiteralPath (Join-Path $work 'version.txt') -Value $version
        $hash = if ($case -eq 'bad-hash') {'a' * 64} else {(Get-FileHash -LiteralPath $archive).Hash}
        Set-Content -LiteralPath (Join-Path $work 'hash.txt') -Value $hash
        $shell = Join-Path $env:WINDIR 'System32\WindowsPowerShell\v1.0\powershell.exe'
        if ($case -eq 'inherited-shell') {
            # 模拟应用继承 PowerShell 7 的模块路径，绕过交互式 shell 对子进程环境的自动修正。
            $start = [Diagnostics.ProcessStartInfo]::new($shell)
            $start.UseShellExecute = $false
            $start.CreateNoWindow = $true
            $start.EnvironmentVariables['PSModulePath'] = $env:PSModulePath
            $start.Arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$installer`" -WorkPath `"$work`" -TargetExe `"$target`" -ParentProcessId 0"
            $process = [Diagnostics.Process]::Start($start)
            if (-not $process.WaitForExit(30000)) { throw '继承终端环境的更新测试超时' }
            $code = $process.ExitCode
            $process.Dispose()
        } else {
            & $shell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $installer -WorkPath $work -TargetExe $target -ParentProcessId 0
            $code = $LASTEXITCODE
        }
        if ($case -in @('success','inherited-shell')) {
            if ($code -ne 0) { throw "更新安装失败：$case" }
            if ((Get-FileHash -LiteralPath $target).Hash -ne (Get-FileHash -LiteralPath $fixture).Hash) { throw '未替换为新版程序' }
            for ($attempt=0; $attempt -lt 50 -and -not (Test-Path -LiteralPath (Join-Path $targetDir 'started.txt')); $attempt++) { Start-Sleep -Milliseconds 100 }
            if (-not (Test-Path -LiteralPath (Join-Path $targetDir 'started.txt'))) { throw '更新后没有重启' }
            # The marker is written just before exit; wait for the fixture to release its EXE.
            Get-Process daozai -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $target } | ForEach-Object {
                if (-not $_.WaitForExit(5000)) { throw '更新测试程序没有退出' }
            }
        } else {
            if ($code -eq 0 -or -not (Test-Path -LiteralPath (Join-Path $work 'error.txt'))) { throw "未拒绝无效更新：$case" }
            if($case -eq 'missing-version'){
                $message=Get-Content -LiteralPath (Join-Path $work 'error.txt') -Raw
                if($message -notmatch '无法读取更新程序的版本信息' -or $message -match '版本不匹配'){throw 'Missing metadata was reported as a version mismatch'}
            }
            if ((Get-FileHash -LiteralPath $target).Hash -ne $before) { throw '无效更新修改了原程序' }
        }
        if ((Get-Content -LiteralPath $data -Raw).Trim() -ne 'keep-user-data') { throw '用户数据被修改' }
        Write-Output "通过：$case"
    }
} finally {
    $env:LOCALAPPDATA = $oldLocal
}

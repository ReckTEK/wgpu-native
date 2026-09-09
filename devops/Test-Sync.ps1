# Only local bare repositories and fake release responses are used by these tests.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$scriptPath = Join-Path $PSScriptRoot 'Sync-Upstream.ps1'
$root = Join-Path ([IO.Path]::GetTempPath()) ('recktek sync tests ' + [Guid]::NewGuid().ToString('N'))
$null = [IO.Directory]::CreateDirectory($root)
$saved = @{}
foreach ($name in @('GIT_CONFIG_GLOBAL', 'GIT_CONFIG_SYSTEM', 'GIT_ALLOW_PROTOCOL', 'GITHUB_REPOSITORY')) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name)
}
$empty = Join-Path $root 'empty.gitconfig'
[IO.File]::WriteAllText($empty, '')
$env:GIT_CONFIG_GLOBAL = $empty
$env:GIT_CONFIG_SYSTEM = $empty
$env:GIT_ALLOW_PROTOCOL = 'file'
[Environment]::SetEnvironmentVariable('GITHUB_REPOSITORY', $null)
$metrics = @{ assertions = 0 }
$script:counter = 0

function Test-Assert([bool] $Condition, [string] $Message) {
    if (!$Condition) { throw "FAIL: $Message" }
    $metrics.assertions++
}

function Test-Git([string] $Directory, [string[]] $Arguments) {
    $start = [Diagnostics.ProcessStartInfo]::new('git')
    $start.WorkingDirectory = $Directory
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $text, $errorText = $stdout.GetAwaiter().GetResult(), $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode) { throw "Fixture git failed: $($Arguments -join ' ') $errorText" }
        return $text.Trim()
    } finally { $process.Dispose() }
}

function Test-Commit([string] $Directory, [string] $Name, [string] $Value) {
    $null = [IO.Directory]::CreateDirectory((Split-Path (Join-Path $Directory $Name) -Parent))
    [IO.File]::WriteAllText((Join-Path $Directory $Name), $Value)
    $null = Test-Git $Directory @('add', '--', $Name)
    $null = Test-Git $Directory @('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false', 'commit', '-qm', $Value)
    return Test-Git $Directory @('rev-parse', 'HEAD')
}

function New-Fixture([string] $Name) {
    $directory = Join-Path $root $Name
    $null = [IO.Directory]::CreateDirectory($directory)
    $work, $upstream, $fork, $forkWork = @('work', 'upstream.git', 'fork.git', 'fork-work') | ForEach-Object { Join-Path $directory $_ }
    $null = Test-Git $directory @('init', '-q', '-b', 'trunk', $work)
    $null = Test-Commit $work 'base.txt' 'base'
    $base = Test-Commit $work '.github/workflows/ci.yml' 'fixture workflow; never executed'
    $null = Test-Git $directory @('clone', '-q', '--bare', $work, $upstream)
    $null = Test-Git $directory @('clone', '-q', '--bare', $work, $fork)
    $null = Test-Git $directory @('clone', '-q', $fork, $forkWork)
    $commit = Test-Commit $work 'upstream.txt' 'upstream'
    $null = Test-Git $work @('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'tag', '-a', 'v1.2.3', '-m', 'Release source')
    $null = Test-Git $work @('push', '-q', $upstream, 'trunk', 'refs/tags/v1.2.3')
    return @{ Directory = $directory; Work = $work; Upstream = $upstream; Fork = $fork; ForkWork = $forkWork; Base = $base; Commit = $commit; Release = $null; Posts = 0; FailPost = $false; Race = $null; Workflows = @(@{ path = '.github/workflows/ci.yml'; state = 'disabled_manually' }) }
}

function Run-Sync($Fixture, [switch] $Apply, [switch] $Failure, [switch] $NoRelease, [int] $Schema = 1) {
    $script:counter++
    $config = Join-Path $Fixture.Directory "config-$script:counter.json"
    $output = Join-Path $Fixture.Directory "result-$script:counter.json"
    [IO.File]::WriteAllText($config, (@{
        schemaVersion = $Schema; repository = 'fixture/fork'; upstream = 'fixture/upstream'
        branch = 'main'; seedBranch = 'trunk'; upstreamBranch = 'trunk'; mirrorSourceRelease = !$NoRelease; holdUpstreamBuilds = $true
    } | ConvertTo-Json))
    $api = {
        param($Method, $Path, $Body)
        if ($Method -ceq 'GET' -and $Path -ceq '/repos/fixture/fork/actions/workflows?per_page=100&page=1') {
            return @{ workflows = $Fixture.Workflows }
        }
        if ($Method -ceq 'GET' -and $Path -ceq '/repos/fixture/upstream/releases?per_page=100&page=1') {
            if ($Fixture.Race) {
                $null = Test-Git $Fixture.Fork @('update-ref', 'refs/heads/main', $Fixture.Race, $Fixture.Base)
                $Fixture.Race = $null
            }
            return ,@(@{ id = 123; tag_name = 'v1.2.3'; draft = $false; prerelease = $false; published_at = [DateTime]::UtcNow })
        }
        if ($Method -ceq 'GET' -and $Path -ceq '/repos/fixture/fork/releases/tags/v1.2.3') {
            if ($Fixture.Release) { return $Fixture.Release }
            throw [Microsoft.PowerShell.Commands.HttpResponseException]::new('Missing', [Net.Http.HttpResponseMessage]::new([Net.HttpStatusCode]::NotFound))
        }
        if ($Method -ceq 'POST' -and $Path -ceq '/repos/fixture/fork/releases') {
            $Fixture.Posts++
            if ($Fixture.FailPost) { throw [Microsoft.PowerShell.Commands.HttpResponseException]::new('Denied', [Net.Http.HttpResponseMessage]::new([Net.HttpStatusCode]::Forbidden)) }
            Test-Assert (!$Body.Contains('target_commitish')) 'Release creation must not select a moving branch'
            Test-Assert (!$Body.Contains('assets')) 'Source notification does not claim built artifacts'
            $tag = Test-Git $Fixture.Fork @('rev-parse', 'refs/tags/v1.2.3^{commit}')
            Test-Assert ($tag -ceq $Fixture.Commit) 'The exact source tag exists before release creation'
            $Fixture.Release = @{ id = 456; tag_name = $Body.tag_name; draft = $false; prerelease = $false; body = $Body.body }
            return $Fixture.Release
        }
        throw "Unexpected API request: $Method $Path"
    }.GetNewClosure()
    $failed = $false
    try {
        & $scriptPath -Config $config -Output $output -Workspace $Fixture.Directory -LocalRepository $Fixture.Fork -LocalUpstream $Fixture.Upstream -Apply:$Apply -Api $api
    } catch {
        $failed = $true
        [IO.File]::WriteAllText("$output.error.txt", $_.ToString())
        if (!$Failure) { throw }
    }
    Test-Assert ($failed -eq $Failure.IsPresent) 'Expected sync success/failure'
    return Get-Content -LiteralPath $output -Raw | ConvertFrom-Json -AsHashtable
}

try {
    $fixture = New-Fixture 'preview-and-release'
    $preview = Run-Sync $fixture
    Test-Assert ($preview.status -ceq 'prepared' -and !$preview.pushed) 'Preview prepares but does not push'
    Test-Assert (!(Test-Git $fixture.Fork @('show-ref', '--heads') | Select-String 'refs/heads/main')) 'Preview does not create main'
    Test-Assert ((Test-Git $fixture.Fork @('tag', '-l')).Length -eq 0 -and $fixture.Posts -eq 0) 'Preview creates neither remote tags nor releases'
    Test-Assert ($preview.workflowHold[0].state -ceq 'not-checked-preview') 'Preview reports incoming workflows without requiring administrative API state'
    $applied = Run-Sync $fixture -Apply
    Test-Assert ($applied.status -ceq 'complete' -and $applied.pushed -and $applied.release.published) 'Apply updates source and release'
    Test-Assert ((Test-Git $fixture.Fork @('rev-parse', 'main')) -ceq $fixture.Commit) 'Missing main seeds from trunk and advances normally'
    Test-Assert ((Test-Git $fixture.Fork @('symbolic-ref', 'HEAD')) -ceq 'refs/heads/trunk') 'Sync never changes the default branch'
    Test-Assert ((Test-Git $fixture.Fork @('rev-parse', 'refs/tags/v1.2.3')) -ceq (Test-Git $fixture.Upstream @('rev-parse', 'refs/tags/v1.2.3'))) 'Annotated tag object is mirrored exactly'
    $again = Run-Sync $fixture -Apply
    Test-Assert (!$again.pushed -and $fixture.Posts -eq 1) 'Repeated sync reuses source refs and release metadata'

    $diverged = New-Fixture 'fork-commits'
    $null = Test-Git $diverged.ForkWork @('switch', '-qc', 'main')
    $forkCommit = Test-Commit $diverged.ForkWork 'fork.txt' 'fork customization'
    $null = Test-Git $diverged.ForkWork @('push', '-q', 'origin', 'main')
    $merged = Run-Sync $diverged -Apply -NoRelease
    $null = Test-Git $diverged.Fork @('merge-base', '--is-ancestor', $forkCommit, 'main')
    $null = Test-Git $diverged.Fork @('merge-base', '--is-ancestor', $diverged.Commit, 'main')
    Test-Assert ((Test-Git $diverged.Fork @('show', 'main:fork.txt')) -ceq 'fork customization') 'Normal merge retains fork changes'
    Test-Assert ($merged.release -eq $null -and $diverged.Posts -eq 0) 'The wgpu fork mirrors tags without publishing native release metadata'

    $conflict = New-Fixture 'merge-conflict'
    $null = Test-Git $conflict.ForkWork @('switch', '-qc', 'main')
    $forkCommit = Test-Commit $conflict.ForkWork 'upstream.txt' 'conflicting fork content'
    $null = Test-Git $conflict.ForkWork @('push', '-q', 'origin', 'main')
    $failed = Run-Sync $conflict -Apply -Failure
    Test-Assert (!$failed.pushed -and (Test-Git $conflict.Fork @('rev-parse', 'main')) -ceq $forkCommit) 'Merge conflicts preserve fork main'
    Test-Assert ((Test-Git $conflict.Fork @('tag', '-l')).Length -eq 0) 'Merge conflicts do not partially mirror tags'

    $tagConflict = New-Fixture 'tag-conflict'
    $null = Test-Git $tagConflict.Fork @('tag', 'v1.2.3', $tagConflict.Base)
    $failed = Run-Sync $tagConflict -Apply -Failure
    Test-Assert ($failed.error -match 'Immutable tag' -and !$failed.pushed) 'Conflicting version tags stop before any push'
    Test-Assert ((Test-Git $tagConflict.Fork @('rev-parse', 'refs/tags/v1.2.3')) -ceq $tagConflict.Base) 'Conflicting tags are never replaced'

    $retry = New-Fixture 'release-retry'
    $retry.FailPost = $true
    $failed = Run-Sync $retry -Apply -Failure
    Test-Assert ($failed.pushed -and !$failed.release.published) 'Release API failure preserves honest source-push evidence'
    $retry.FailPost = $false
    $retried = Run-Sync $retry -Apply
    Test-Assert (!$retried.pushed -and $retried.release.published) 'Release retry does not rewrite existing source refs'
    $retry.Release.body = 'User-owned release'
    $failed = Run-Sync $retry -Apply -Failure
    Test-Assert ($failed.error -match 'existing release' -and $retry.Release.body -ceq 'User-owned release') 'Existing unrelated release metadata is preserved'

    $race = New-Fixture 'branch-race'
    $null = Test-Git $race.Fork @('update-ref', 'refs/heads/main', $race.Base)
    $raceCommit = Test-Commit $race.ForkWork 'concurrent.txt' 'concurrent fork change'
    $null = Test-Git $race.ForkWork @('push', '-q', 'origin', 'HEAD:refs/heads/race-object')
    $race.Race = $raceCommit
    $failed = Run-Sync $race -Apply -Failure
    Test-Assert (!$failed.pushed -and (Test-Git $race.Fork @('rev-parse', 'main')) -ceq $raceCommit) 'Concurrent fork changes reject a non-forced push'
    Test-Assert ((Test-Git $race.Fork @('tag', '-l')).Length -eq 0) 'An atomic push rejects tags together with a conflicting branch'

    $bad = Run-Sync (New-Fixture 'invalid-config') -Apply -Failure -Schema 2
    Test-Assert ($bad.status -ceq 'failed' -and !$bad.pushed) 'Unsupported configuration fails before source mutation'
    foreach ($state in @('active', 'unregistered')) {
        $held = New-Fixture "workflow-$state"
        if ($state -ceq 'active') { $held.Workflows[0].state = 'active' } else { $held.Workflows = @() }
        $failed = Run-Sync $held -Apply -Failure
        Test-Assert (!$failed.pushed -and $failed.error -match 'administrator must register and disable') 'Build hold blocks active and unknown inherited workflows'
        Test-Assert ((Test-Git $held.Fork @('tag', '-l')).Length -eq 0) 'Build hold prevents tag-triggered jobs too'
    }
    $historical = New-Fixture 'historical-workflow'
    $null = Test-Git $historical.Work @('switch', '-qc', 'historical', $historical.Base)
    $null = Test-Commit $historical.Work '.github/workflows/old.yml' 'historical workflow'
    $null = Test-Git $historical.Work @('tag', 'v1.0.0')
    $null = Test-Git $historical.Work @('push', '-q', $historical.Upstream, 'refs/tags/v1.0.0')
    $failed = Run-Sync $historical -Apply -Failure
    Test-Assert (!$failed.pushed -and $failed.error -match 'old.yml') 'The hold checks historical tag workflows absent from incoming main'
    $null = Test-Git $historical.Work @('push', '-q', $historical.Fork, 'refs/tags/v1.0.0')
    $seeded = Run-Sync $historical -Apply -NoRelease
    Test-Assert ($seeded.status -ceq 'complete' -and $seeded.workflowHold.Count -eq 1) 'Already-seeded historical tags do not retrigger workflows and are excluded from the hold check'
    Write-Host "PASS: $($metrics.assertions) fork-local assertions; no network or builds."
} finally {
    foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name]) }
    Remove-Item -LiteralPath $root -Recurse -Force
}

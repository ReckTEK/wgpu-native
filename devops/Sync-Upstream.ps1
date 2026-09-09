# This fork owns its sync. Prepare all refs before one atomic, non-forced push.
[CmdletBinding()]
param(
    [string] $Config = (Join-Path $PSScriptRoot 'sync.json'),
    [Parameter(Mandatory)][string] $Output,
    [switch] $Apply,
    [string] $Workspace = ([IO.Path]::GetTempPath()),
    [string] $LocalRepository,
    [string] $LocalUpstream,
    [scriptblock] $Api
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$utf8 = [Text.UTF8Encoding]::new($false)
$run = Join-Path ([IO.Path]::GetFullPath($Workspace)) ('upstream-sync-' + [Guid]::NewGuid().ToString('N'))
$null = [IO.Directory]::CreateDirectory($run)
$hooks = Join-Path $run 'empty-hooks'
$null = [IO.Directory]::CreateDirectory($hooks)
$log = Join-Path $run 'git.log'
$Output = [IO.Path]::GetFullPath($Output)
$record = [ordered]@{ schemaVersion = 1; status = 'preparing'; repository = $null; branch = $null; baseCommit = $null; upstreamCommit = $null; commit = $null; tags = @(); workflowHold = @(); release = $null; pushed = $false; error = $null }

function Save-Record {
    $null = [IO.Directory]::CreateDirectory((Split-Path $Output -Parent))
    [IO.File]::WriteAllText($Output, ($record | ConvertTo-Json -Depth 12).Replace("`r`n", "`n") + "`n", $utf8)
}

function Git([string] $Directory, [string[]] $Arguments, [int[]] $Allowed = @(0)) {
    $start = [Diagnostics.ProcessStartInfo]::new('git')
    $start.WorkingDirectory = $Directory
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $start.Environment['GIT_TERMINAL_PROMPT'] = '0'
    $start.Environment['GCM_INTERACTIVE'] = 'Never'
    $start.Environment['GIT_MERGE_AUTOEDIT'] = 'no'
    $start.Environment['LC_ALL'] = 'C'
    $process = [Diagnostics.Process]::Start($start)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $result = @{ Code = $process.ExitCode; Text = $stdout.GetAwaiter().GetResult(); Error = $stderr.GetAwaiter().GetResult() }
    } finally { $process.Dispose() }
    [IO.File]::AppendAllText($log, "git $($Arguments -join ' ')`n$($result.Text)$($result.Error)`n", $utf8)
    if ($result.Code -notin $Allowed) { throw "Git failed ($($result.Code)): git $($Arguments -join ' ')`n$($result.Error)" }
    return $result
}

function Request([string] $Method, [string] $Path, $Body = $null) {
    if ($Api) { $value = & $Api $Method $Path $Body; return ,$value }
    $headers = @{ Accept = 'application/vnd.github+json'; 'X-GitHub-Api-Version' = '2022-11-28' }
    if ($env:GH_TOKEN) { $headers.Authorization = "Bearer $env:GH_TOKEN" }
    if ($Method -cne 'GET' -and !$env:GH_TOKEN) { throw 'GH_TOKEN is required to publish source release metadata.' }
    $parameters = @{ Uri = "https://api.github.com$Path"; Method = $Method; Headers = $headers; UserAgent = 'recktek-fork-sync' }
    if ($null -ne $Body) { $parameters.Body = $Body | ConvertTo-Json -Depth 10 -Compress; $parameters.ContentType = 'application/json' }
    $value = Invoke-RestMethod @parameters
    return ,$value
}

function Publication-Time($Value) {
    if ($Value -is [DateTimeOffset]) { return $Value }
    if ($Value -is [DateTime] -and $Value.Kind -ne [DateTimeKind]::Unspecified) { return [DateTimeOffset]::new($Value) }
    if ($Value -is [string] -and $Value -cmatch '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$') {
        return [DateTimeOffset]::Parse($Value, [Globalization.CultureInfo]::InvariantCulture)
    }
    throw 'Invalid source release publication time.'
}

function Stable-Release([string] $Repository) {
    $releases = [Collections.Generic.List[object]]::new()
    for ($page = 1; $page -le 100; $page++) {
        $items = Request GET "/repos/$Repository/releases?per_page=100&page=$page"
        if ($items -isnot [array]) { throw 'Expected a release array.' }
        foreach ($item in $items) {
            if ($item.draft -isnot [bool] -or $item.prerelease -isnot [bool]) { throw 'Invalid source release flags.' }
            if ($item.draft -or $item.prerelease) { continue }
            if (($item.id -isnot [int] -and $item.id -isnot [long]) -or $item.id -le 0) { throw 'Invalid source release ID.' }
            $null = Publication-Time $item.published_at
            $releases.Add($item)
        }
        if ($items.Count -lt 100) { break }
        if ($page -eq 100) { throw 'Release pagination exceeded its limit.' }
    }
    if (!$releases.Count) { throw 'No stable upstream source release exists.' }
    return $releases | Sort-Object @{ Expression = { Publication-Time $_.published_at }; Descending = $true },
        @{ Expression = { $_.id }; Descending = $true } | Select-Object -First 1
}

function Read-Tags([string] $Text) {
    $tags = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::Ordinal)
    foreach ($line in ($Text -split '\r?\n')) {
        if (!$line) { continue }
        if ($line -cnotmatch '^([0-9a-f]{40})\s+refs/tags/(v[^\s]+)$') { throw "Malformed upstream tag: $line" }
        $sha, $name = $Matches[1], $Matches[2]
        if ($name.EndsWith('^{}', [StringComparison]::Ordinal)) { continue }
        if ($name -cnotmatch '^v\d+\.\d+\.\d+(?:\.\d+)?(?:-[0-9A-Za-z.-]+)?$') { continue }
        if (!$tags.TryAdd($name, $sha)) { throw "Duplicate tag: $name" }
    }
    return $tags
}

function Check-WorkflowHold([string] $Directory, $Settings) {
    if (!$Settings.holdUpstreamBuilds) { return }
    $paths = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $commits = @($record.commit) + @($record.tags | Where-Object { $_.missing } | ForEach-Object { $_.commit })
    foreach ($commit in ($commits | Select-Object -Unique)) {
        $files = (Git $Directory @('ls-tree', '-r', '--name-only', $commit, '--', '.github/workflows')).Text
        foreach ($path in ($files -split '\r?\n')) {
            if ($path -cmatch '^\.github/workflows/[^/]+\.ya?ml$' -and $path -cne '.github/workflows/recktek-sync.yml') { $null = $paths.Add($path) }
        }
    }
    $states = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::Ordinal)
    if ($Apply -and $paths.Count) {
        for ($page = 1; $page -le 100; $page++) {
            $response = Request GET "/repos/$($Settings.repository)/actions/workflows?per_page=100&page=$page"
            if ($response.workflows -isnot [array]) { throw 'Expected an Actions workflow array.' }
            foreach ($workflow in $response.workflows) {
                if ($workflow.path -isnot [string] -or $workflow.state -isnot [string]) { throw 'Invalid Actions workflow state.' }
                $states[$workflow.path] = $workflow.state
            }
            if ($response.workflows.Count -lt 100) { break }
            if ($page -eq 100) { throw 'Workflow pagination exceeded its limit.' }
        }
    }
    $names = [string[]]@($paths)
    [Array]::Sort($names, [StringComparer]::Ordinal)
    $record.workflowHold = @($names | ForEach-Object {
        $state = if (!$Apply) { 'not-checked-preview' } elseif ($states.ContainsKey($_)) { $states[$_] } else { 'unregistered' }
        [ordered]@{ path = $_; state = $state }
    })
    if ($Apply) {
        $blocked = @($record.workflowHold | Where-Object { $_.state -cnotin @('disabled_manually', 'disabled_fork') })
        if ($blocked.Count) {
            $details = ($blocked | ForEach-Object { "$($_.path) ($($_.state))" }) -join ', '
            throw "Upstream builds are on hold. An administrator must register and disable these workflows before sync: $details"
        }
    }
}

Save-Record
try {
    $settings = Get-Content -LiteralPath $Config -Raw | ConvertFrom-Json -AsHashtable
    if ($settings.schemaVersion -ne 1 -or $settings.mirrorSourceRelease -isnot [bool] -or $settings.holdUpstreamBuilds -isnot [bool]) { throw 'Unsupported fork-sync configuration.' }
    foreach ($name in @('repository', 'upstream')) {
        if ($settings[$name] -isnot [string] -or $settings[$name] -cnotmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { throw "Invalid $name repository." }
    }
    foreach ($name in @('branch', 'upstreamBranch', 'seedBranch')) {
        if ($settings[$name] -isnot [string] -or !$settings[$name]) { throw "Missing $name." }
        $null = Git $run @('check-ref-format', "refs/heads/$($settings[$name])")
    }
    if ($env:GITHUB_REPOSITORY -and $env:GITHUB_REPOSITORY -cne $settings.repository) { throw 'This workflow may update only its configured repository.' }
    $record.repository = $settings.repository
    $record.branch = $settings.branch
    $origin = "git@github.com:$($settings.repository).git"
    # Public upstream reads need no identity. The only SSH key belongs to this fork.
    $upstream = "https://github.com/$($settings.upstream).git"
    if ($LocalRepository -or $LocalUpstream) {
        if (![IO.Directory]::Exists($LocalRepository) -or ![IO.Directory]::Exists($LocalUpstream)) { throw 'Both local fixture repositories must exist.' }
        $origin = [IO.Path]::GetFullPath($LocalRepository)
        $upstream = [IO.Path]::GetFullPath($LocalUpstream)
    }

    $directory = Join-Path $run 'checkout'
    $null = Git $run @('clone', '--no-checkout', '--no-tags', '--', $origin, $directory)
    $null = Git $directory @('config', 'core.hooksPath', $hooks)
    $remote = Git $directory @('ls-remote', '--exit-code', '--heads', 'origin', "refs/heads/$($settings.branch)") @(0, 2)
    $created = $remote.Code -eq 2
    $seed = if ($created) { $settings.seedBranch } else { $settings.branch }
    $null = Git $directory @('fetch', '--no-tags', 'origin', "refs/heads/${seed}:refs/sync/base")
    $null = Git $directory @('remote', 'add', 'upstream', $upstream)
    $null = Git $directory @('fetch', '--no-tags', 'upstream', "refs/heads/$($settings.upstreamBranch):refs/sync/upstream", 'refs/tags/v*:refs/sync-tags/v*')
    $record.baseCommit = (Git $directory @('rev-parse', 'refs/sync/base')).Text.Trim()
    $record.upstreamCommit = (Git $directory @('rev-parse', 'refs/sync/upstream')).Text.Trim()
    $null = Git $directory @('checkout', '--quiet', '--detach', $record.baseCommit)
    $null = Git $directory @('-c', 'user.name=ReckTEK Upstream Sync', '-c', 'user.email=upstream-sync@users.noreply.github.com',
        '-c', 'commit.gpgsign=false', 'merge', '--ff', '--no-edit', $record.upstreamCommit)
    $record.commit = (Git $directory @('rev-parse', 'HEAD')).Text.Trim()

    $upstreamTags = Read-Tags (Git $directory @('ls-remote', '--tags', 'upstream', 'refs/tags/v*')).Text
    $forkTags = Read-Tags (Git $directory @('ls-remote', '--tags', 'origin', 'refs/tags/v*')).Text
    $push = [Collections.Generic.List[string]]::new()
    if ($created -or $record.commit -cne $record.baseCommit) { $push.Add("$($record.commit):refs/heads/$($settings.branch)") }
    $names = [string[]]@($upstreamTags.Keys)
    [Array]::Sort($names, [StringComparer]::Ordinal)
    $tagRecords = [Collections.Generic.List[object]]::new()
    foreach ($name in $names) {
        $tag = "refs/sync-tags/$name"
        $object = (Git $directory @('rev-parse', $tag)).Text.Trim()
        if ($object -cne $upstreamTags[$name]) { throw "Upstream tag '$name' moved during observation; retry before writing." }
        $commit = (Git $directory @('rev-parse', "$tag^{commit}")).Text.Trim()
        if ($forkTags.ContainsKey($name) -and $forkTags[$name] -cne $object) { throw "Immutable tag '$name' differs in this fork; nothing was pushed." }
        $missing = !$forkTags.ContainsKey($name)
        $tagRecords.Add([ordered]@{ name = $name; object = $object; commit = $commit; missing = $missing })
        if ($missing) { $push.Add("${tag}:refs/tags/$name") }
    }
    $record.tags = @($tagRecords)
    Check-WorkflowHold $directory $settings

    if ($settings.mirrorSourceRelease) {
        $source = Stable-Release $settings.upstream
        $tag = @($tagRecords | Where-Object { $_.name -ceq $source.tag_name })
        if ($tag.Count -ne 1) { throw 'The latest source release does not have a verified version tag.' }
        $record.release = [ordered]@{
            upstreamRepository = $settings.upstream; upstreamId = $source.id; tag = $source.tag_name
            commit = $tag[0].commit; object = $tag[0].object; id = $null; published = $false
        }
    }
    $record.status = 'prepared'
    Save-Record

    if ($Apply) {
        if ($push.Count) {
            # GitHub supports atomic pushes. A conflicting ref or concurrent branch advance rejects the complete batch.
            $null = Git $directory (@('push', '--atomic', '--porcelain', 'origin') + @($push))
            $record.pushed = $true
            Save-Record
        }
        if ($settings.mirrorSourceRelease) {
            $path = "/repos/$($settings.repository)/releases"
            $existing = $null
            try { $existing = Request GET "$path/tags/$([Uri]::EscapeDataString($record.release.tag))" }
            catch {
                if (!($_.Exception.PSObject.Properties['Response'] -and $_.Exception.Response.StatusCode -eq 404)) { throw }
            }
            $marker = "<!-- upstream-release: $($settings.upstream)#$($record.release.upstreamId) $($record.release.commit) -->"
            if ($existing) {
                if ($existing.draft -or $existing.prerelease -or $existing.body -notlike "*$marker*") { throw 'An existing release has different metadata; it was preserved.' }
            } else {
                # The exact tag already exists. Do not let the release API create a tag from a moving branch.
                $existing = Request POST $path ([ordered]@{
                    tag_name = $record.release.tag
                    name = "Upstream source $($record.release.tag)"
                    body = "Source release mirrored from https://github.com/$($settings.upstream)/releases/tag/$($record.release.tag).`n`nUpstream commit: $($record.release.commit).`n`nThis records source identity only. No native binaries or binding packages have been built or published by this release.`n`n$marker"
                    draft = $false; prerelease = $false; make_latest = 'true'
                })
            }
            if (($existing.id -isnot [int] -and $existing.id -isnot [long]) -or $existing.id -le 0 -or
                $existing.tag_name -cne $record.release.tag -or $existing.draft -ne $false -or
                $existing.prerelease -ne $false -or $existing.body -notlike "*$marker*") {
                throw 'Invalid mirrored release response; publication was not confirmed.'
            }
            $record.release.id = $existing.id
            $record.release.published = $true
        }
        $record.status = 'complete'
    }
    Save-Record
    Write-Host "$($settings.repository): $($record.status); report $Output"
} catch {
    $record.status = 'failed'
    $record.error = $_.Exception.Message
    Save-Record
    throw "Upstream sync stopped; changes already pushed are recorded in $Output. Diagnostics: $log. $($_.Exception.Message)"
}

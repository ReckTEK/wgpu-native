# Fork-owned upstream maintenance

This fork merges `gfx-rs/wgpu-native:trunk` into its own `main`. It preserves fork commits
and mirrors upstream version tags so consumers can pin compatible release commits.
The bindings repository, `ReckTEK/wgpu-native-x`, owns binding builds and publication;
it does not synchronize these forks.

`recktek-sync.yml` runs the local fixture tests, then prepares the update in a
disposable checkout. Manual runs default to preview. Scheduled application remains
disabled until `UPSTREAM_SYNC_ENABLED=true`. Set this fork's default branch to
`main` during administrative setup so its scheduled workflow is available.

## Credentials and build hold

Register a fresh repository-scoped **write deploy key** on this fork. Put the private
key in this fork's `UPSTREAM_SYNC_KEY` Actions secret; use this fork's SSH URL as the
key comment when generating it. The workflow does not use or upload a personal key.
The deploy key permits pushing workflow-file changes that the built-in
`GITHUB_TOKEN` cannot authorize. The token reads workflow state and, on the native
fork, manages source-release metadata.

The fork Git URL remains SSH. Public upstream Git reads use HTTPS without a key.
No credential can write to upstream or to the other fork. The workflow pins GitHub's
published SSH host key from `github_known_hosts` instead of trusting a live scan.

`holdUpstreamBuilds=true` prevents sync from starting inherited upstream builds.
Before applying, an administrator must register and disable every inherited workflow
in the incoming main commit **and newly mirrored tag commits**, leaving only
`recktek-sync.yml` enabled. The script checks their Actions API states and stops
before pushing if any workflow is active or unknown. Newly introduced upstream
workflow files therefore require administrative setup. Preview reports the file
inventory without requiring that setup. Lift this hold only when upstream builds
are deliberately enabled.

## Sync and release contract

Run `pwsh -NoProfile -File devops/Test-Sync.ps1` for fixture tests. They use temporary
local bare repositories and fake API responses; they never build Rust or contact
GitHub. A manual workflow preview performs real read-only source observation. Set
its `apply` input to true to authorize the prepared update.

The script starts from existing fork `main`, or seeds a missing main from fork
`trunk`. A normal merge retains both histories. Conflicts stop for manual resolution.
Semantic `v*` tags preserve their exact upstream tag objects and peeled commits;
an existing tag with a different object is an error. One atomic, non-forced push
updates the branch and missing tags together, so a concurrent branch change cannot
leave a partial tag batch. The script never resets, force-pushes, changes a default
branch, or edits consumer submodule pins.

Only the native fork sets `mirrorSourceRelease=true`. After its exact upstream tag
exists in the fork, it mirrors the latest stable upstream release as a clearly
labelled **source release**, recording the upstream release ID and commit. It omits
`target_commitish`, never attaches build assets, and refuses to overwrite unrelated
existing release metadata. Release API failures remain explicit; the source refs
and partial-success report are preserved, and retry completes metadata without
rewriting the tags. Historical workflow permission restrictions can require
administrator intervention; a source release is claimed only after the API succeeds.

The bindings repository observes native fork `main` for nightly inputs and these
native fork releases for stable inputs. Its scheduled polling requires no cross-repo
write token or shared SSH key. Exact compatible wgpu commits, matching headers,
consumer patches, and successful target builds remain part of binding promotion.

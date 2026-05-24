# rust-template

GitHub template repository for new Rust projects. Provides CI, dependabot,
release tagging and three publish workflows out of the box.

## Use it

1. Click "Use this template" -> "Create a new repository" on GitHub.
2. Rename the package in `Cargo.toml` and update the binary path in `Dockerfile`
   (the `target/release/rust-template` line).
3. Add the secrets listed in [Secrets](#secrets).
4. Protect `master` by importing a ruleset - see [Branch protection](#branch-protection).

## Dependency flow

Dependabot opens PRs straight into `master`. Each PR runs the full `ci.yml`
(fmt, clippy, the test matrix and a release build), so a bump is merged only
when it is green. Branch protection keeps `master` green even when several PRs
land close together - see [Branch protection](#branch-protection).

There is no separate dependencies branch. In Rust the compiler, `clippy` and
the test matrix catch breakage from a bump at PR time, so a holding branch adds
machinery without adding safety.

## Branch protection

`master` should require a passing CI before a merge. `ci.yml` ends with a
`CI success` job that depends on every other job - require that single check
rather than the individual matrix jobs, whose names change with the matrix.
The job name `CI success` and the ruleset's required-check context are one
contract: rename one without the other and the required check stays pending
forever.

Two ready-to-import rulesets live in `.github/rulesets/`. Import one under
Settings -> Rules -> Rulesets -> "Import a ruleset":

| Ruleset                   | Use it for                              | Catches interaction breakage by                                       |
|---------------------------|-----------------------------------------|------------------------------------------------------------------------|
| `master.json`             | personal repos, or orgs without a queue | requiring branches to be up to date before merge (re-runs CI on the rebased PR) |
| `master-merge-queue.json` | organization repos                      | a merge queue that tests each PR combined with the ones ahead of it    |

Both rulesets let the repository admin (built-in `RepositoryRole` id 5) bypass
the rule, so `bump-and-release` can push the version commit with
`RELEASE_TOKEN`. The token must belong to an account with the admin role -
otherwise the bump push is blocked by the required check, which never runs on a
direct push. On an org with custom roles, verify the bypass actor id.

Merge queue is available only on organization-owned repositories, not on
personal accounts. `ci.yml` already triggers on `merge_group`, so enabling the
queue on an org repo is a single setting. Required status checks can also be
applied org-wide through an organization-level ruleset; the merge queue rule
itself is repository-level only.

## Workflows

| File                       | Trigger                                | Purpose                                                 |
|----------------------------|----------------------------------------|---------------------------------------------------------|
| `ci.yml`                   | push/PR on master, merge queue, manual | fmt, clippy, test matrix (rust x distro), release build |
| `bump-and-release.yml`     | manual                                 | bump version, tag, optional GitHub Release              |
| `publish-crates.yml`       | manual on a tag                        | publish to crates.io                                    |
| `publish-docker-hub.yml`   | manual on a tag                        | build and push image to Docker Hub, multi-arch          |
| `publish-ghcr.yml`         | manual on a tag                        | build and push image to ghcr.io, multi-arch             |

### Test matrix

`fmt` and `clippy` run in parallel, then the `test` matrix, then `build`.
`ci.yml` runs `cargo test --all-features` across:

- rust: `1.94`, `1.93`, `1.92` - the dev version plus the two below it
- env: `ubuntu-latest`, `debian:12`, `archlinux:latest`, `rust:1-alpine`

12 matrix jobs, `fail-fast: false`. Distros run via `container:`. The dev
version lives in `rust-toolchain.toml` (used by local builds, the Dockerfile,
`fmt`/`clippy`/`build` and the publish gate); the matrix lists it plus the two
below. Raise both by hand when the floor moves. Fixed versions test exactly the
toolchains you develop and ship on; `stable`/`beta` drift over time.

Keep `rust-toolchain.toml`: `fmt`/`clippy`/`build`, the publish gate and the
Dockerfile have no explicit toolchain step and fall back to the runner's
bundled Rust if it is removed.

### Bump and release

Manual trigger from any branch (selected via "Use workflow from"). Inputs:

| Input           | Type    | Default | Effect                                                  |
|-----------------|---------|---------|---------------------------------------------------------|
| `version`       | choice  | patch   | bump level: patch / minor / major                       |
| `beta`          | boolean | false   | append `-beta.N` suffix; N is auto-incremented per base |
| `force_release` | boolean | false   | create GitHub Release even for a beta tag               |
| `commit_note`   | string  | ""      | optional text appended to the bump commit message       |

A `checks` job (fmt, clippy, tests via the reusable `checks.yml`) runs first;
nothing is bumped or tagged if it fails. Then the job updates `Cargo.toml`,
commits, tags `vX.Y.Z[-beta.N]` and pushes the commit and tag to the selected
branch. Pushing the commit into a protected `master` needs `RELEASE_TOKEN`;
`GITHUB_TOKEN` cannot bypass branch rules. A second job creates a GitHub
Release unless the tag is beta and `force_release` is false. Beta releases are
marked `--prerelease`.

### Publish

Each publish workflow is triggered manually with "Use workflow from:
tags/vX.Y.Z". A guard rejects runs from a branch. Each one first runs the
reusable `checks.yml` (fmt, clippy, tests on the dev toolchain) against the
tagged commit and publishes only if it passes - so a tag that never went
through PR CI cannot ship a broken artifact. The full matrix stays on PRs.
Docker tags are derived from the tag name; `latest` is
set only for non-beta tags.

Publish is manual on purpose, so a tag without a published artifact is allowed.
If you switch to auto-publishing on tag push, you will need a PAT: a tag pushed
with `GITHUB_TOKEN` does not trigger other workflows.

## Dependabot

Two ecosystems, both opening PRs into `master`:

- `cargo` - weekly, max 5 open PRs, minor+patch grouped, major opens its own PR
- `github-actions` - weekly, max 5 open PRs, same grouping

## Secrets

Add these in repository settings before using the relevant workflow:

| Secret                 | Used by            | Notes                                                                  |
|------------------------|--------------------|------------------------------------------------------------------------|
| `RELEASE_TOKEN`        | bump-and-release   | PAT with `contents: write`. Lets the bump commit reach protected `master`; `GITHUB_TOKEN` cannot. |
| `CARGO_REGISTRY_TOKEN` | publish-crates     | crates.io API token                                                    |
| `DOCKERHUB_USERNAME`   | publish-docker-hub | Docker Hub login                                                       |
| `DOCKERHUB_TOKEN`      | publish-docker-hub | Docker Hub access token                                                |

The GitHub Release step and GHCR publish use the built-in `GITHUB_TOKEN`
(`contents: write` and `packages: write`). No extra secret.

## Dockerfile

Minimal alpine multi-stage. The binary path in the runtime stage is
`target/release/rust-template`. Rename it together with the package name in
`Cargo.toml`, otherwise the `COPY --from=builder` step fails.

A `.dockerignore` keeps `.git`, `target` and local env files out of the build
context.

Crates with C dependencies under musl need extra apk packages in the builder
stage (for example `openssl-dev pkgconfig`).

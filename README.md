# Kamal GUI

A native macOS app, written entirely in Rust ([egui](https://github.com/emilk/egui)), for deploying, watching and debugging all
your [Kamal](https://kamal-deploy.org) projects from one place, without tying
deploys to the checkout you're working in.

[![CI](https://github.com/alexvikman/kamal-gui/actions/workflows/ci.yml/badge.svg)](https://github.com/alexvikman/kamal-gui/actions/workflows/ci.yml)

## Opinionated

Kamal GUI is built around one way of working with Kamal and doesn't try to
support every setup. If yours differs, the CLI is still there.

- **Deploy what's on the remote, never your working copy.** Only pushed commits
  can be deployed: the app clones from origin and deploys from its own worktree.
  Uncommitted or unpushed changes are invisible to it by design.
- **Secrets live in 1Password, references live in git.** `.kamal/secrets` is
  committed and only contains `$(kgui secret op://…)` references. A deploy stops
  early if the secrets file isn't in the commit.
- **Config by convention.** `config/deploy.yml` is the app, `config/deploy.workers.yml`
  its workers; `deploy.<name>.yml` is a Kamal destination when it only overrides
  a few keys and a separate config when it's complete.
- **Status comes from the servers.** What's running is read from Docker over ssh
  (Kamal's container labels), not from any local record.
- **One run per project at a time,** and you can't start another until it's done.
- **macOS and GitHub first.** Built for macOS, uses `gh` to find repositories and
  releases, and the system `ssh`, `git`, `op` and `kamal` as they are configured
  on your machine.
- **Kamal 2.**

## Install

Every push to `main` that passes CI publishes a
[nightly release](https://github.com/alexvikman/kamal-gui/releases/tag/nightly)
(universal: Apple Silicon + Intel):

- `KamalGUI-nightly-macos-universal.zip`: unzip, move `Kamal GUI.app` to
  `/Applications`. It isn't notarized, so clear the quarantine flag once:
  `xattr -dr com.apple.quarantine "/Applications/Kamal GUI.app"`
- `kgui-nightly-macos-universal.tar.gz`: the `kgui` CLI

Each artifact has signed build provenance:
`gh attestation verify <file> -R alexvikman/kamal-gui`.

### Updates

The app checks for a newer nightly at start and every six hours ("Update
available" in the sidebar). **Kamal GUI → About Kamal GUI** (or **Check for
Updates…**) shows the version and commit you're on and installs the update:
it downloads the release, checks the SHA-256 checksum, then verifies the signed
build provenance with `gh attestation verify` (built by this repo's `ci.yml` on
`main`, at the release's commit, on a GitHub-hosted runner). If that can't be
verified, for instance because `gh` isn't signed in, nothing is installed. It then
checks the bundle id and signature, swaps the app in place and relaunches.
It never installs a build older than the one that's running.
Updating is disabled while a deploy is running.

## How it works

- **Projects are added to the app**, either by picking repositories from GitHub
  (through `gh`, with Kamal repos detected via code search) or by pasting a git URL
  (`https://…` or ssh, e.g. `git@github.com:org/app.git`).
  The app keeps its own bare clone of each one in
  `~/Library/Application Support/kgui/repos/`. Your working copies are never read or touched.
- **Deploys run from a dedicated worktree** (`…/kgui/worktrees/<project>`) checked out at
  the commit you pick: the default branch, one of the latest commits, any branch or tag.
  You can keep working (dirty tree and all) while a deploy runs.
- **Deploy units** come from `config/deploy*.yml` on origin's default branch and are grouped into targets:
  - `deploy.yml` + `deploy.workers.yml` → target `default` (web + workers)
  - `deploy.ff.yml` without `servers`/`registry`/`builder` → destination, run as `-c config/deploy.yml -d ff`
  - `deploy.acme.yml` with a full config → standalone, `-c config/deploy.acme.yml`
- **Status** comes straight from Docker on each host (`ssh <user>@<host> docker ps`, using Kamal's
  `service`/`role`/`destination` labels). It shows which commit runs where, how far behind the
  default branch it is, and which older versions you can roll back to.
- **Logs** stream with `docker logs --follow` over ssh into a filterable console.
- **History**: every run is stored with its commit, outcome and full output
  (`…/kgui/history.sqlite`, `…/kgui/logs/<id>.log`). Secret values the app fetched for
  the run are replaced with `[REDACTED]` in the output, should a command print one.

## Secrets

`.kamal/secrets` is committed to the repo and only holds references to 1Password:

```
APP_KEY=$(kgui secret op://deploy/my-app/APP_KEY)
```

Before running kamal, the app reads every reference with **one `op` call per item**
(one Touch ID prompt) and passes the values to the kamal process through environment
variables. Nothing is written to disk. Run from a terminal, `kgui secret` calls `op` itself.
A field missing in 1Password is an error; an empty field resolves to an empty string.

Migrate files that use the old bash adapter (`op-read-item-field`). Without `--write` it only
shows what would change:

```bash
kgui migrate-secrets path/to/repo/.kamal/secrets*
```

```bash
kgui migrate-secrets --write path/to/repo/.kamal/secrets*
```

Both styles in use are handled: direct calls, and `$OP_HELPER $OP_ITEM …` with variables
assigned in the same file. Files using `kamal secrets fetch` are left alone; they work as-is.

## Command line

```bash
cargo install --path crates/kgui-cli
```

```
kgui projects                          # projects and their deploy targets
kgui add GIT_URL                       # add a project (clones it)
kgui remove PROJECT                    # remove a project and its clone
kgui fetch PROJECT                     # fetch from origin
kgui status PROJECT                    # containers per host
kgui secret op://VAULT/ITEM/FIELD      # for .kamal/secrets
kgui migrate-secrets [--write] FILE…
kgui doctor                            # which kamal, op, gh, git and ssh are used
```

The app binary doubles as `kgui`: the app symlinks itself into `…/kgui/bin/` and puts that
first on PATH for kamal processes, so `.kamal/secrets` works without installing the CLI.
Cancelling a run stops kamal's whole process group, including the ssh and docker processes it started.

## Requirements

- `kamal` installed in some Ruby (mise, rbenv, asdf or chruby installs are auto-detected,
  newest Ruby first; override in Settings)
- `op` (1Password CLI) with desktop app integration
- `gh` signed in, for browsing GitHub (adding by URL works without it)
- ssh access to the servers (VPN)

## Development

```bash
cargo run -p kamal-gui
```

```bash
cargo test --workspace
```

Build `Kamal GUI.app` (in `target/release/bundle/`):

```bash
./scripts/bundle-macos.sh
```

Layout:

- `crates/kgui-core`: all logic (project registry, git, GitHub, secrets, kamal, status, run engine, history, CLI)
- `crates/kgui-cli`: the `kgui` binary
- `app`: the egui desktop app (sidebar, project view, console, dialogs)
- `scripts/bundle-macos.sh`: wraps the release binary in a `.app`

The UI is light-only and uses [Geist](https://vercel.com/font) (bundled, SIL Open Font License,
see `app/assets/fonts/Geist-OFL.txt`) with SF Mono for code when available.

## Current limitations

- Interactive commands (`kamal app exec -i`, aliases like `shell`/`tinker`) need a terminal; the
  app copies the command for you. A built-in terminal (pty) is planned.
- One run per project at a time, since runs share the deploy worktree.
- Kamal runs locally: images are built on your Mac and the servers must be reachable.

## License

[MIT](LICENSE). The bundled [Geist](https://vercel.com/font) font is under the
SIL Open Font License (`app/assets/fonts/Geist-OFL.txt`).

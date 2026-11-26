# Build, Test, and Release

The root `Makefile` is the canonical interface for repeatable workspace builds,
checks, tests, and release packaging.

## Supported build target

The v0.1.1 release target is x86_64 GNU/Linux. Building on that target can
automatically download and verify the pinned Embeddable Lemonade runtime. On
other targets, embedded provisioning is unavailable; the build degrades to
graph-only mode or can connect to a separately managed Lemonade Server.

Hardware guidance is separate from compiler and packaging requirements; see
[COMPAT.md](COMPAT.md).

## Prerequisites

Install the stable Rust toolchain from [rustup.rs](https://rustup.rs), GNU Make,
a C compiler, `curl`, `sha256sum`, and `tar`. Debian and Ubuntu also need the
native libraries used by GPUI:

```bash
sudo apt-get update
sudo apt-get install --yes \
  build-essential curl make pkg-config tar \
  libasound2-dev libfontconfig1-dev libudev-dev libwayland-dev \
  libx11-xcb-dev libxcb-xfixes0-dev libxkbcommon-dev libxkbcommon-x11-dev
```

SQLite is bundled and does not require a system SQLite development package.

## Build and run

From the repository root:

```bash
make build
cargo run -p u-forge
```

No environment variables are required. On x86_64 GNU/Linux, the first build
downloads and verifies the pinned upstream Ubuntu x64 Embeddable Lemonade
runtime into `target/`. The application starts that private runtime when needed
and opens Lemonade setup when backends or models are missing.

To build without downloading or staging the embedded runtime:

```bash
UFORGE_SKIP_EMBEDDED_LEMONADE=1 make build
```

Without a staged runtime, the desktop remains graph-only unless you set
`LEMONADE_URL` to select a compatible external server, even if it uses the
standard loopback port. Without that override, the desktop attempts to launch
its private runtime; only standalone discovery helpers and direct integration
tests probe an existing loopback server.

## Verification

For a substantial change, run the canonical local checks:

```bash
make fmt-check
make check
make clippy
make test
```

`make test` builds the workspace and the patched `cosmic-text` crate, provisions
one checksum-pinned embedded Lemonade server for the complete serial suite, and
tears down the owned process tree afterward. It requires x86_64 GNU/Linux and
either network access or a cached copy of the pinned runtime. Keep the default
serial execution for the shared runtime; a focused Cargo test does not replace
this full gate after substantial code changes.

CI uses the graph-only path:

```bash
make test-ci
```

That target disables embedded provisioning and skips live Lemonade tests. Use
`make fmt` to apply Rust formatting and `make help` to list the remaining
targets.

### Live inference fabric baseline

`make inference-fabric-test` checks the measurement logic offline.
`make inference-fabric FABRIC_CONFIG=<experiment.toml>` runs the real embedding
backfill and chat/search overlap matrix against an owned local runtime. See the
[baseline protocol](docs/inference-fabric.md) for the isolated cache, exact
hardware and model pins, optional whole-system energy meter, and result schema.

## Build the AppImage

AppImage packaging additionally requires `desktop-file-utils` and `patchelf`:

```bash
sudo apt-get install --yes desktop-file-utils patchelf
make release
```

`make release` performs a locked optimized build, requires the pinned embedded
runtime, downloads the checksum-pinned `linuxdeploy` tool when it is not cached,
and writes these artifacts to `dist/`:

```text
u-forge-0.1.1-x86_64.AppImage
u-forge-0.1.1-x86_64.AppImage.sha256
```

Release automation may override the artifact version with a tag:

```bash
RELEASE_VERSION=v0.1.1 make release
```

The resulting AppImage contains the application, packaged defaults, and private
Lemonade runtime.

## Optional environment overrides

- `LEMONADE_URL` — explicit external Lemonade Server URL; suppresses the desktop's embedded launch.
- `LEMONADE_API_KEY` — inference API key override.
- `LEMONADE_ADMIN_API_KEY` — management API key override.
- `UFORGE_SKIP_EMBEDDED_LEMONADE=1` — skip private runtime provisioning.
- `UFORGE_LEMOND_PATH=/path/to/lemond` — development binary override.
- `RUST_LOG=info` — application log verbosity.

These are overrides, not setup requirements.

# u-forge.ai (Universe Forge)

> **Build a world you can see, search, and talk to.** A local-first desktop
> workspace for game masters.

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Status](https://img.shields.io/badge/status-Alpha-yellow.svg)
![Distribution](https://img.shields.io/badge/distribution-AppImage-6f5bd3.svg)

![u-forge.ai showing World Canvas, Details, and Assistant](assets/images/u-forge-ai.png)

## Your campaign notes, connected

Worldbuilding rarely fits into a folder tree. Characters belong to factions,
quests cross locations, artifacts change hands, and old events explain current
conflicts. u-forge brings those pieces together as a living knowledge graph so
you can follow the connections instead of hunting through documents.

- **See the whole setting.** World Canvas turns people, places, factions,
  quests, events, and anything else you define into an explorable relationship
  map.
- **Shape u-forge around your world.** Schemas act as templates for your lore,
  defining the kinds of things in a setting, the details they carry, and how
  they can relate. The editor adapts to those choices instead of forcing every
  campaign into the same character-sheet format.
- **Find lore the way you remember it.** Search exact words, search by meaning,
  or combine both into a best match. When local AI is unavailable, word search
  keeps working.
- **Ask questions in context.** The Assistant answers from your own setting,
  shows when it searches the graph, and keeps conversations alongside the world
  they are about.
- **Build with the Assistant.** Compatible models can search, create, and update
  world items and relationships while respecting the structure you defined.

## Local-first by design

Your world lives in local SQLite databases and does not require an account,
subscription, or hosted service. Graph exploration, editing, import/export,
and word search continue to work without any AI server.

Optional AI features run through
[Lemonade Server](https://github.com/lemonade-sdk/lemonade). On x86_64 Linux
with GNU libc, u-forge can provision and run its own private Lemonade runtime.
That runtime is distributed upstream as an Ubuntu artifact; Arch Linux and
CachyOS are the primary development and dogfooding platforms for u-forge. Its
guided setup discovers the available hardware and manages the models needed for
search and chat. You can also connect to a separately managed server; world
data leaves the machine only if you explicitly point u-forge at a non-local
one.

## Made for a GM's desk

World, Search, Details, Assistant, and the permanent World Canvas stay together
in a focused native workspace rather than being split across tools. A cohesive
dark theme and color-coded world types keep even dense settings readable, while
the technical AI controls remain out of the way until you want them.

Imports are checked against the world's templates, so malformed records and
ambiguous relationships are reported rather than quietly reshaping the setting.
The included Foundation sample is available as a starting point, or you can
bring your own schemas and JSONL data.

## Install the AppImage

Linux x86_64 releases are distributed as one AppImage with a companion SHA-256
checksum. Download both files, verify the checksum, and run the image:

```bash
sha256sum --check u-forge-0.1.1-pre-x86_64.AppImage.sha256
chmod +x u-forge-0.1.1-pre-x86_64.AppImage
./u-forge-0.1.1-pre-x86_64.AppImage
```

The release is built on Ubuntu 26.04 and targets contemporary x86_64 GNU/Linux
desktops. The AppImage bundles u-forge, its default schemas and example data,
and its private Lemonade runtime.

On first launch, u-forge creates its per-user files at the standard XDG paths:

- configuration: `${XDG_CONFIG_HOME:-$HOME/.config}/u-forge/u-forge.toml`
- database and editable defaults: `${XDG_DATA_HOME:-$HOME/.local/share}/u-forge`
- downloaded Lemonade models and runtime state: `${XDG_CACHE_HOME:-$HOME/.cache}/u-forge/lemonade`

The shipped defaults are copied only once. Removing or editing a user copy is
therefore durable across later launches.

## Build and run from source

Install the stable Rust toolchain and a C compiler first. Debian and Ubuntu
also need the native GPUI development libraries used by CI:

```bash
sudo apt-get update
sudo apt-get install --yes \
  libasound2-dev libfontconfig1-dev libudev-dev libwayland-dev \
  libx11-xcb-dev libxcb-xfixes0-dev libxkbcommon-dev libxkbcommon-x11-dev
```

Then build and launch from the repository root:

```bash
make build
cargo run -p u-forge
```

On x86_64 Linux with GNU libc, the first build downloads and verifies the
pinned upstream Ubuntu x64 Embeddable Lemonade runtime. The app starts that
private runtime automatically and opens Lemonade AI Setup when required
components are missing. Models and backend executables are downloaded only
when selected in that setup flow.

For a reproducible optimized build of the distributable application, run
`make release`. The x86_64 AppImage and its checksum are written to `dist/`.

To build without the embedded runtime, set
`UFORGE_SKIP_EMBEDDED_LEMONADE=1`. On other platforms, or when using an
independently managed server, run a compatible Lemonade Server and set
`LEMONADE_URL` only when it is not available at the default loopback address.

### Start a world

On a new profile, the app opens a guided world-creation flow. Choose a required
schema directory and, optionally, an initial JSONL data file. Lemonade discovery
and downloads continue in the background; schema-only worlds can be created
without waiting for AI models, while importing initial data waits for the
selected embedding prerequisites.

The existing **File → Import Schema…** and **File → Import Data…** actions remain
available for later imports. Select items in World or the canvas, edit them in
Details, and save changes explicitly.

## For developers

[ARCHITECTURE.md](ARCHITECTURE.md) covers the crate layout, SQLite and vector
storage, inference pipeline, schema boundaries, and UI architecture. Open
product work and completed implementation briefs are indexed by
[the plan ledger](.plans/README.md); archived plans are historical audit
material rather than current instructions.

```bash
make fmt-check
make check
make clippy
make test
```

`make test` provisions one checksum-pinned embedded Lemonade instance for the
complete suite, then unloads models and tears down the owned process tree.

## License

MIT License. Your worlds belong to you.

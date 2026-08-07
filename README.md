# u-forge.ai (Universe Forge)

> **Build a world you can see, search, and talk to.** A local-first desktop
> workspace for game masters.

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Status](https://img.shields.io/badge/status-Alpha-yellow.svg)
![Distribution](https://img.shields.io/badge/distribution-source%20builds-lightgrey.svg)

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
[Lemonade Server](https://github.com/lemonade-sdk/lemonade). On Ubuntu x64,
u-forge can provision and run its own private Lemonade runtime. Its guided setup
discovers the available hardware and manages the models needed for search and
chat. You can also connect to a separately managed server; world data leaves
the machine only if you explicitly point u-forge at a non-local one.

## Made for a GM's desk

World, Search, Details, Assistant, and the permanent World Canvas stay together
in a focused native workspace rather than being split across tools. A cohesive
dark theme and color-coded world types keep even dense settings readable, while
the technical AI controls remain out of the way until you want them.

Imports are checked against the world's templates, so malformed records and
ambiguous relationships are reported rather than quietly reshaping the setting.
The included Foundation sample is available as a starting point, or you can
bring your own schemas and JSONL data.

## Build and run from source

u-forge.ai is currently available as an Alpha source build. Binary releases
have not been published yet.

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
cargo run -p u-forge-ui-gpui
```

On Ubuntu x64, the first build downloads and verifies the pinned Embeddable
Lemonade runtime. The app starts that private runtime automatically and opens
Lemonade AI Setup when required components are missing. Models and backend
executables are downloaded only when selected in that setup flow.

To build without the embedded runtime, set
`UFORGE_SKIP_EMBEDDED_LEMONADE=1`. On other platforms, or when using an
independently managed server, run a compatible Lemonade Server and set
`LEMONADE_URL` only when it is not available at the default loopback address.

### Start a world

The app opens with an empty local database:

1. Use **File → Import Schema…** and select `defaults/schemas` or your own
   schema directory.
2. Use **File → Import Data…** and select `defaults/data/memory.jsonl` or a
   matching JSONL file.
3. Select items in World or the canvas, edit them in Details, and save changes
   explicitly.

## For developers

[ARCHITECTURE.md](ARCHITECTURE.md) covers the crate layout, SQLite and vector
storage, inference pipeline, schema boundaries, and UI architecture. Current
implementation briefs and their status live in
[the active plan ledger](.plans/README.md); archived plans are historical audit
material.

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

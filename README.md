# u-forge.ai (Universe Forge)

> **Turn scattered campaign lore into a world you can see, search, and talk
> to.**
>
> u-forge.ai is a local-first desktop workspace for game masters: map every
> connection, find the detail you need, and create with an AI assistant that
> understands the structure of your setting.

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Status](https://img.shields.io/badge/status-Alpha-yellow.svg)
![Distribution](https://img.shields.io/badge/distribution-AppImage-6f5bd3.svg)

![u-forge.ai showing World Canvas, Details, and Assistant](assets/images/u-forge-ai.png)

## One world, not a pile of documents

Campaign lore grows sideways. Characters serve factions, artifacts change
hands, quests cross continents, and events from centuries ago still shape the
next session. Before long, the answer you need is somewhere in your notes—but
the connections between those notes are harder to see.

u-forge turns that sprawl into a living knowledge graph. It gives you one
focused workspace to build your setting, explore how everything fits together,
and bring the right lore back when the table needs it.

- **See how the world fits together.** World Canvas turns people, places, factions,
  quests, events, and anything else you define into an explorable relationship
  map.
- **Shape it around your campaign.** Schemas define the kinds of things in your
  setting, the details they carry, and the relationships they allow. The editor
  adapts to your campaign instead of forcing every world into the same sheet.
- **Find lore the way you remember it.** Exact-word, semantic, and hybrid search
  surface the right pieces even when you cannot remember where you wrote them.
- **Work with an assistant that knows your world.** Compatible local models can
  search your setting, answer in context, and create or update items and
  relationships while respecting your schemas.
- **Know what made it into your world.** Imports are validated against your
  schemas. Malformed records and ambiguous relationships are reported instead
  of silently changing the shape of your data.

## Local-first. AI-optional. Yours.

Your worlds live in local SQLite databases. u-forge requires no account,
subscription, or hosted service, and graph exploration, editing, import/export,
and full-text search keep working without an AI server.

Optional AI runs through
[Lemonade Server](https://github.com/lemonade-sdk/lemonade). On Linux x86_64,
u-forge can provision and manage a private Lemonade runtime; it can also use a
separately managed server. Your world data stays on your machine unless you
explicitly connect u-forge to a non-local endpoint.

World, Search, Details, Assistant, and the permanent World Canvas stay together
in one native workspace. Explore the included Foundation setting to see it in
action, or begin with your own schemas and JSONL data.

## The road to 1.0

u-forge is growing into more than a note vault with an assistant attached. The
vision for 1.0 is a connected campaign workspace where the world graph, its
history and geography, what happened at the table, and the rules behind it all
reinforce one another.

Each release builds toward that vision without giving up the local-first
foundation:

| Release | What it adds to the world |
|---------|---------------------------|
| **v0.1.2** | A focused refinement cycle makes the core worldbuilding experience smoother and more dependable. |
| **v0.2** | Schema-embedded SVG icons give every kind of world item a visual identity across the World panel and graph, with per-item overrides when something should stand apart. |
| **v0.3** | Smarter embedding and chunking make semantic recall more accurate and efficient as worlds grow. |
| **v0.4** | A dedicated timeline turns epochs, eras, and events into an explorable history of narrative arcs and systemic consequences. |
| **v0.5** | A world map uses `located_in` relationships to place connected entities automatically, letting existing world data reveal its geography. |
| **v0.6** | Session diarization and transcription preserve what happened at the table so the Assistant can recall it and help turn play into lasting world data. |
| **v0.7–v0.8** | Rules from PDF bring table-heavy TTRPG books into a dedicated rules graph, with sourced rules linked directly to the world they govern. |
| **v0.9** | A focused refinement release brings the complete experience together. |
| **v1.0** | A complete local-first campaign workspace connects preparation, world knowledge, and play. Your worlds await. |
| **v1.x** | Scheming intensifies… |

Release contents and ordering may evolve as each feature meets real worlds and
real tables.

## Try v0.1.1

u-forge is currently alpha software. The packaged release targets Linux x86_64
and is distributed as an AppImage with a companion SHA-256 checksum.

**[Download the latest release](https://github.com/th3raid0r/u-forge.ai/releases/latest)**,
grab both files, then verify and run the AppImage:

```bash
sha256sum --check u-forge-0.1.1-x86_64.AppImage.sha256
chmod +x u-forge-0.1.1-x86_64.AppImage
./u-forge-0.1.1-x86_64.AppImage
```

The AppImage bundles u-forge, its default schemas and example data, and its
private Lemonade runtime. See [COMPAT.md](COMPAT.md) for the current platform
and hardware guidance. Developers and source-build users should start with
[BUILD.md](BUILD.md).

On first launch, u-forge creates its per-user files at the standard XDG paths:

- configuration: `${XDG_CONFIG_HOME:-$HOME/.config}/u-forge/u-forge.toml`
- worlds and editable defaults: `${XDG_DATA_HOME:-$HOME/.local/share}/u-forge`
- Lemonade models and runtime state: `${XDG_CACHE_HOME:-$HOME/.cache}/u-forge/lemonade`

The shipped defaults are copied only once, so edits and deletions in your user
copy remain yours across later launches.

### Start a world

A new profile opens a guided world-creation flow. Choose a schema directory
and, optionally, an initial JSONL data file. Lemonade discovery and downloads
continue in the background, so a schema-only world can be created without
waiting for AI models. The included defaults are the quickest way to start
exploring.

Use **File → Import Schema…** and **File → Import Data…** for later imports.
Select an item in World or on the canvas, edit it in Details, and save the
change when it is ready.

## Support u-forge

If u-forge helps you build better worlds, you can support its continued
development on Ko-fi.

<a href='https://ko-fi.com/P4U0251C4T' target='_blank'><img height='36' style='border:0px;height:36px;' src='https://storage.ko-fi.com/cdn/kofi6.png?v=6' border='0' alt='Buy Me a Coffee at ko-fi.com' /></a>

## Project documentation

- [Compatibility and hardware](COMPAT.md)
- [Build, test, and release](BUILD.md)
- [Architecture](ARCHITECTURE.md)
- [Product and implementation ledger](.plans/README.md)

## License

MIT License. Your worlds belong to you.

# u-forge.ai (Universe Forge)

> **Your worlds, your data, your way.** A local-first TTRPG worldbuilding tool powered by AI.

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)
![Status](https://img.shields.io/badge/status-Early%20Prototype-red.svg)

## What is u-forge.ai?

u-forge.ai (Universe Forge) is an **early prototype** of a local-first TTRPG worldbuilding tool. Currently in very early development, it demonstrates core concepts for AI-powered knowledge management while keeping your data completely local and under your control.

**⚠️ Current Status: This is a bare-bones proof-of-concept, not a production application.**

### 🎯 Vision for Game Masters:
- Build rich, interconnected worlds with characters, locations, factions, and lore
- Never lose track of campaign details with AI-powered semantic search
- Capture session notes automatically through audio transcription (*planned*)
- Visualize relationships between story elements in an interactive graph (*planned*)
- Keep their data private without relying on cloud services

**Note: Most features are planned for future development. See "Current Prototype Features" below.**

## 🚧 Current Prototype Features

**What Actually Works Today:**
- Basic RocksDB-backed knowledge graph storage
- Local text embeddings using FastEmbed (BGE-small-en-v1.5)
- Simple semantic search with cosine similarity
- FST-based exact name matching
- Basic CRUD operations for worldbuilding objects
- Command-line demo application

**⚠️ What's Missing (Planned for Future):**
- Visual graph interface
- Advanced vector search (HNSW integration)
- Audio transcription
- Content generation
- Cross-platform GUI
- Most features described in the vision above

## 🔮 Planned Features

### 🏠 Local-First & Private
- All your worldbuilding data stays on your device
- No subscriptions, no servers, no data harvesting
- You own your API keys and control your AI usage
- Works completely offline (with reduced AI features)

### 🧠 AI-Powered Worldbuilding
- **Local-First Embeddings**: Built-in semantic search using FastEmbed-rs (no external APIs required)
- **Multiple Model Options**: Choose from 15+ pre-trained models based on your quality/size preferences
- **Hybrid Search**: Combine semantic similarity with exact name matching for comprehensive retrieval
- **Context-aware Content Generation**: Using your world's established lore with optional cloud AI integration
- **Automatic Entity Detection**: AI-powered relationship mapping between story elements
- **Optional Cloud Integration**: Add OpenAI/Anthropic API keys for enhanced generation capabilities
- **Ollama Support**: Seamless integration if you're already using Ollama for local LLMs

### 🕸️ Visual Knowledge Graph
- Interactive graph canvas showing connections between story elements
- TTRPG-specific schemas for characters, locations, factions, items, and events
- Zoom from high-level campaign overview to detailed character relationships
- Real-time updates as you build your world

### 🎙️ Session Capture & Notes (*Future Development*)
- Record in-person game sessions with automatic speech-to-text transcription
- AI automatically detects and promotes important story beats to permanent campaign notes
- Smart speaker identification for tracking who said what around the table
- Link session events directly to characters, locations, and plot threads in your world graph

### 🏪 U-Store (*Future Development*)
- Official licensed content from major publishers with pre-built knowledge graphs
- Integration with physical book purchases
- Drop-in ready campaigns and official campaign settings

### 🚀 Premium Integrations (*Future Development*)
- Virtual session recording with multi-platform correlation
- Unified session capture across platforms
- Cloud sync and collaboration features

### ⚡ Performance Goals
- Sub-second search across large datasets (*in development*)
- Native application performance with Rust + Tauri (*planned*)
- Efficient memory usage (*basic implementation complete*)
- Responsive UI (*planned*)

## 🛠️ Development Setup

> **⚠️ This is a prototype for developers only. No end-user releases are available.**

### System Requirements
- RAM: 4GB minimum for development
- Storage: 2GB for Rust toolchain + dependencies
- OS: Linux (primary), macOS, Windows (all require development setup)
- GCC 13+ (for RocksDB compilation)

### Current Model Support
- BGE-small-en-v1.5 (133MB) - Default local embedding model
- Other FastEmbed models available but not pre-configured

### Development Installation

**Prerequisites:**
```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Linux: Install GCC 13
sudo apt install gcc-13 g++-13  # Ubuntu/Debian
# or your distribution's equivalent

# Set environment variables
export CC=gcc-13
export CXX=g++-13
```

**Build and Run:**
```bash
git clone https://github.com/your-org/u-forge.ai.git
cd u-forge.ai

# Build (first build takes ~10 minutes due to RocksDB compilation)
cargo build

# Run the demo with default schemas
cargo run --bin u-forge-ai

# Or load a specific JSON dataset
cargo run --bin u-forge-ai -- ./examples/data/memory.json
```

### Loading JSON Data

The main application can now load structured JSON datasets instead of the hardcoded demo data:

**JSON Format:**
```json
{"type":"node","name":"Object Name","nodeType":"location","metadata":["Key: Value","Description: Object description"]}
{"type":"edge","from":"Source Object","to":"Target Object","edgeType":"relationship_type"}
```

**Supported Node Types:**
- `location` - Places, planets, systems
- `npc` - Non-player characters  
- `player_character` - Player characters
- `faction` - Organizations, governments
- `quest` - Missions, events, storylines
- `artifact` - Items, equipment, vehicles
- `currency` - Money, resources
- `skills` - Abilities, spells, powers
- `temporal` - Events, timelines
- `setting_reference` - Rules, lore
- `system_reference` - Game mechanics

**Edge Types (Relationships):**
All edge types are now string-based for maximum schema flexibility:
- Use any descriptive relationship name: `"led_by"`, `"governs"`, `"trades_with"`
- No need to map to predefined enums - just use meaningful names
- Schema system can define valid edge types and constraints

**Example Usage:**
```bash
# Load Foundation universe data (included)
cargo run -- ./examples/data/memory.json

# Load your own dataset
cargo run -- /path/to/your/data.json
```

### What the Demo Shows

The current demo loads structured worldbuilding data and demonstrates:
1. JSON parsing and object creation from metadata
2. **Flexible string-based relationship types** (no enum constraints)
3. Schema-based object validation
4. Text embedding generation for all content
5. Semantic search across the knowledge graph
6. Exact name matching with FST
7. Hybrid search combining both approaches

**Note: This is a single-shot CLI demo, not an interactive application.**

## 🎯 Intended Use Cases (*When Complete*)

### For In-Person Campaign Management
- Track complex political relationships between factions with visual connections
- Remember which NPCs know which secrets and when they revealed them
- Quickly find story elements using semantic search
- Generate consistent lore that builds on established facts
- Work completely offline with local models

### For Worldbuilding Authors
- Maintain consistency across large fictional universes
- Find related concepts instantly through semantic search
- Visualize how different story elements connect
- Export world data for use in other tools

### For Content Creators
- Build comprehensive campaign settings for publication
- Create interconnected adventure modules
- Maintain series continuity across multiple works

**Note: These are planned capabilities. Current prototype only demonstrates basic storage and search.**

## 🛠️ Technical Architecture

**Current Implementation:**
- Core Engine: Rust (performance and memory safety)
- Database: RocksDB (storage and crash recovery)
- Embeddings: FastEmbed-rs (local text embeddings)
- Vector Search: Simple cosine similarity (*HNSW integration pending*)
- Name Search: FST (finite state transducers)

**Planned Additions:**
- UI Framework: Tauri + Svelte
- Advanced vector search with HNSW
- Multi-provider AI integration
- Audio transcription pipeline

### Known Technical Challenges

**HNSW Integration Issues:**
The current implementation uses a simplified vector search due to significant API compatibility issues with `hnsw_rs` v0.3.x. The HNSW crate underwent major breaking changes that affected:
- Method signatures (`nb_elements` → `get_nb_point`, etc.)
- Struct field names in search results
- Serialization/deserialization support
- Lifetime parameter requirements

This forced a fallback to basic cosine similarity search. Proper HNSW integration requires either:
1. Extensive API migration work, or
2. Evaluating alternative vector search libraries

See `CLAUDE.MD` for detailed technical documentation of these issues.

### Contributing

This is an early prototype. Contributions welcome, but expect significant API changes.

## 🗺️ Development Roadmap

### Current State - Foundation Prototype
- [x] Core storage engine with RocksDB
- [x] Basic text embeddings (FastEmbed)
- [x] Simple semantic search
- [x] FST-based exact matching
- [x] Command-line demo
- [x] HNSW vector search integration
- [ ] Basic GUI interface
- [ ] Cross-platform packaging

### Next Phase - Minimal Viable Product
- [ ] Tauri-based desktop application
- [ ] Visual knowledge graph interface
- [ ] Improved vector search performance
- [ ] Basic content generation features
- [ ] Import/export capabilities

### Future Development
- [ ] Audio transcription pipeline
- [ ] Advanced graph layouts and filtering
- [ ] Multi-platform session recording
- [ ] Cloud integration options
- [ ] Publisher content marketplace

**Timeline: No specific dates. This is exploratory development.**

## 🤝 Community

- Discord: [Join our community](https://discord.gg/u-forge-ai) for discussions and support
- Issues: Report bugs and request features on [GitHub Issues](https://github.com/your-org/u-forge.ai/issues)
- Documentation: Full guides and API docs at [docs.u-forge.ai](https://docs.u-forge.ai)

## 📄 License

The core u-forge.ai application is released under the [MIT License](LICENSE). Your worlds belong to you.

Premium integrations and cloud services are available under separate commercial licenses.

## 💼 Future Business Model

**Current Status: Open source prototype (MIT License)**

Planned approach if development continues:
- Core Application: Free and open source (MIT License)
- Premium Features: Enhanced integrations and cloud services
- Content Marketplace: Official licensed knowledge graphs
- Your Data: Always yours, regardless of tier

**Note: No commercial features exist yet. This is purely conceptual.**

## 🙏 Acknowledgments

- Built for the TTRPG community, by the TTRPG community
- Inspired by tools like Obsidian, World Anvil, and Kanka
- Powered by open-source AI and database technologies

---

**Interested in local-first TTRPG tools?** Star this repo to follow early development progress! 🌟

**⚠️ Reminder: This is an early prototype, not a usable application.**

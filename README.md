# u-forge.ai (Universe Forge)

> **Your worlds, your data, your way.** A local-first TTRPG worldbuilding tool powered by AI.

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)
![Status](https://img.shields.io/badge/status-In%20Development-orange.svg)

## What is u-forge.ai?

u-forge.ai (Universe Forge) is a revolutionary worldbuilding application designed specifically for tabletop RPG creators. It combines the power of AI-assisted content generation with a visual knowledge graph, all while keeping your precious campaign data completely local and under your control.

### 🎯 Perfect for Game Masters who want to:
- Build rich, interconnected worlds with characters, locations, factions, and lore
- Never lose track of campaign details with AI-powered semantic search
- Capture session notes automatically through audio transcription
- Visualize relationships between story elements in an interactive graph
- Keep their data private without relying on cloud services

## ✨ Key Features

### 🏠 Local-First & Private
- All your worldbuilding data stays on your device
- No subscriptions, no servers, no data harvesting
- You own your API keys and control your AI usage
- Works completely offline (with reduced AI features)

### 🧠 AI-Powered Worldbuilding
- Semantic search across massive campaign notes (1B+ tokens)
- Context-aware content generation using your world's established lore
- Automatic entity detection and relationship mapping
- Support for OpenAI, Anthropic, and local AI models

### 🕸️ Visual Knowledge Graph
- Interactive graph canvas showing connections between story elements
- TTRPG-specific schemas for characters, locations, factions, items, and events
- Zoom from high-level campaign overview to detailed character relationships
- Real-time updates as you build your world

### 🎙️ Session Capture & Notes (Local Table Focus)
- Record in-person game sessions with automatic speech-to-text transcription
- AI automatically detects and promotes important story beats to permanent campaign notes
- Smart speaker identification for tracking who said what around the table
- Link session events directly to characters, locations, and plot threads in your world graph
- Perfect for traditional tabletop groups who want digital organization without losing the in-person magic

### 🏪 U-Store (Available to All Users)
- Official licensed content from major publishers with pre-built knowledge graphs
- Perfect companion: Buy the physical book, download the knowledge graph for the best of both worlds
- Drop-in ready campaigns like Curse of Strahd, Pathfinder Adventure Paths, and official campaign settings
- Publisher partnership marketplace supporting your favorite game companies
- Note: U-Store content is sold separately - pricing varies by publisher and content

### 🚀 Premium Integrations (Separate Products)
- Virtual session recording with multi-platform correlation (Discord voice + FoundryVTT text + Roll20 dice)
- Unified session capture across different platforms in one coherent timeline
- Cloud sync and collaboration for distributed gaming groups
- Enhanced U-Store integration with automatic session correlation

### ⚡ Performance First
- Sub-second search across millions of notes and documents
- Native application performance (built with Rust + Tauri)
- Efficient memory usage even with massive datasets
- Instant startup and responsive UI

## 🚀 Getting Started

> **Note:** u-forge.ai is currently in development. Follow this repo for updates!

### System Requirements
- RAM: 8GB recommended (4GB minimum)
- Storage: 10GB free space for large datasets
- OS: Linux (primary), macOS, Windows

### Installation

#### Linux (Flatpak - Recommended)
```bash
# Coming soon to Flathub
flatpak install ai.u-forge.universe-forge
```

#### macOS
```bash
# Download from releases page
# Install DMG package
```

#### Windows
```bash
# Download from releases page  
# Run MSI installer
```

### Quick Start

1. Launch u-forge.ai and create your first world
2. Set up AI integration by adding your API keys (stored securely in your OS keystore)
3. Create your first nodes - characters, locations, or factions
4. Start connecting related elements to build your knowledge graph
5. Search and explore your world using natural language queries

## 🎮 Use Cases

### For In-Person Campaign Management
- Record your table sessions and never miss important plot developments
- Track complex political relationships between factions with visual connections
- Remember which NPCs know which secrets and when they revealed them
- Quickly find that tavern from three sessions ago using natural language search
- Generate consistent lore that builds on established facts from your recorded sessions
- U-Store integration: Buy Curse of Strahd at your local game store, then purchase the official knowledge graph to have Barovia's entire cast of characters, locations, and plot threads ready to explore

### For Worldbuilding Authors
- Maintain consistency across large fictional universes
- Explore "what if" scenarios with AI assistance
- Visualize how different story elements connect
- Export your world data for use in other tools
- Learn from the masters: Study how professional game designers structure their worlds by exploring official knowledge graphs from the U-Store

### For Content Creators
- Build comprehensive campaign settings for publication
- Create interconnected adventure modules
- Maintain series continuity across multiple works
- Export your world data for use in other tools
- Professional templates: Use official publisher knowledge graphs as starting points for your own derivative works

### For VTT Users (Premium Features)
- Unified session recording that combines Discord voice chat with FoundryVTT text interactions, Roll20 dice rolls, and D&D Beyond character updates in one timeline
- Cross-platform intelligence that automatically correlates "I cast fireball" in Discord with spell usage in FoundryVTT and damage rolls in Roll20
- Advanced speaker separation that identifies players across different usernames on different platforms
- Cloud sync for multi-device access during online games with real-time collaboration
- Enhanced U-Store integration with premium features like automatic session correlation with official content

## 🛠️ Development

u-forge.ai is built with modern, performant technologies:

- Core Engine: Rust (for performance and memory safety)
- UI Framework: Tauri + Svelte (native performance with web flexibility)
- Database: RocksDB (proven scalability and crash recovery)
- AI Integration: Multi-provider support (OpenAI, Anthropic, local models)
- Vector Search: HNSW (memory-mapped for massive datasets)

### Contributing

We welcome contributions! Please see our [Contributing Guide](CONTRIBUTING.md) for details.

### Development Setup

```bash
# Clone the repository
git clone https://github.com/your-org/u-forge.ai.git
cd u-forge.ai

# Install Rust and Node.js dependencies
cargo build
npm install

# Run in development mode
cargo tauri dev
```

## 🗺️ Roadmap

### v0.1 - Foundation (Current)
- [ ] Core storage engine with RocksDB
- [ ] Basic graph visualization and editing
- [ ] AI integration with major providers
- [ ] Cross-platform packaging

### v0.2 - Enhanced Experience
- [ ] Advanced graph layouts and filtering
- [ ] Git integration for version control
- [ ] Enhanced import/export capabilities
- [ ] Performance optimizations

### v0.3 - Premium Ecosystem
- [ ] Virtual session recording with multi-platform support and temporal correlation (Premium)
- [ ] Cross-platform integration (Discord + FoundryVTT + Roll20 + D&D Beyond) (Premium)
- [ ] Cloud sync and collaboration for distributed teams (Premium)
- [ ] U-Store marketplace launch with publisher partnerships for official licensed content

## 🤝 Community

- Discord: [Join our community](https://discord.gg/u-forge-ai) for discussions and support
- Issues: Report bugs and request features on [GitHub Issues](https://github.com/your-org/u-forge.ai/issues)
- Documentation: Full guides and API docs at [docs.u-forge.ai](https://docs.u-forge.ai)

## 📄 License

The core u-forge.ai application is released under the [MIT License](LICENSE). Your worlds belong to you.

Premium integrations and cloud services are available under separate commercial licenses.

## 💼 Business Model

Core Philosophy: Free for pen-and-paper, premium for digital tools.

- Core Application: Always free and open source (MIT License)
- U-Store Content: Available to all users - purchase official licensed knowledge graphs to complement your physical books (pricing varies by publisher)
- Premium Integrations: Enhanced session recording with multi-platform correlation and cross-platform VTT integrations
- Cloud Services: Optional paid services for sync, collaboration, and unified multi-platform session management
- Your Data: Always yours, whether you use free, U-Store, or premium features

## 🙏 Acknowledgments

- Built for the TTRPG community, by the TTRPG community
- Inspired by tools like Obsidian, World Anvil, and Kanka
- Powered by open-source AI and database technologies

---

**Ready to forge your universe?** Star this repo to stay updated on our progress! 🌟
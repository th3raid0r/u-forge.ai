# u-forge.ai Development Guide

## 🏗️ Project Structure

After reorganization, the project now has a clean separation of concerns:

```
u-forge.ai/
├── Cargo.toml                    # Workspace root
├── DEVELOPMENT.md                 # This file
├── README.md                      # Project overview
├── build.sh                       # Production build script
├── dev.sh                         # Development script
├── env.sh                         # Environment setup (source this!)
├──
├── backend/                       # Core Rust library
│   ├── Cargo.toml
│   ├── dev.sh                     # Backend-specific dev tools
│   ├── src/
│   │   ├── lib.rs                 # Main library entry point
│   │   ├── types.rs               # Core types and structures
│   │   ├── storage.rs             # RocksDB storage layer
│   │   ├── embeddings.rs          # AI embedding providers
│   │   ├── embedding_queue.rs     # Background embedding processing
│   │   ├── vector_search.rs       # Vector similarity search
│   │   ├── schema.rs              # Dynamic schema system
│   │   ├── schema_manager.rs      # Schema management
│   │   ├── schema_ingestion.rs    # Schema data import
│   │   └── data_ingestion.rs      # JSON data import
│   └── examples/
│       └── cli_demo.rs            # CLI demonstration app

```

## 🚨 CRITICAL: Environment Variables

**BEFORE RUNNING ANY COMMANDS**, you MUST set these environment variables for RocksDB compilation to work:

```bash
export CC=gcc-13
export CXX=g++-13
export WEBKIT_DISABLE_DMABUF_RENDERER=1
```

### Easy Setup

Source the environment script (recommended):

```bash
source env.sh
```

Or manually export each time:

```bash
export CC=gcc-13 CXX=g++-13 WEBKIT_DISABLE_DMABUF_RENDERER=1
```

**⚠️ WARNING**: Without these variables, RocksDB compilation will fail with cryptic errors!

**📋 SCHEMA SYSTEM**: The backend provides a sophisticated schema system with flexible property types, validation rules, and relationship definitions. Both the Tauri app and CLI demo use `SchemaIngestion` for proper JSON schema loading.

## 🚀 Quick Start

### 1. Full Development Environment

```bash
# Start both frontend dev server and Tauri app
./dev.sh
```

### 2. Individual Components

```bash
# Frontend only (Svelte dev server)
./dev.sh --frontend-only

# Backend only (for library development)
./dev.sh --backend-only

# Tauri only (requires frontend to be built first)
./dev.sh --tauri-only
```

## 🔧 Development Workflows

### Backend Development

```bash
cd backend

# Use the backend dev script
./dev.sh build      # Build library
./dev.sh test       # Run tests
./dev.sh cli-demo   # Run CLI example
./dev.sh doc        # Generate docs
./dev.sh workflow   # Full dev workflow

# Or use cargo directly (with env vars!)
source ../env.sh
cargo build --lib
cargo test
cargo run --example cli_demo

# CLI with custom data paths
cargo run --example cli_demo /path/to/data.json /path/to/schemas
cargo run --example cli_demo -- --help  # Show usage
```

### Frontend Development

```bash
cd frontend

# Install dependencies
npm install

# Start dev server
npm run dev

# Build for production
npm run build

# Lint and format
npm run lint
npm run format
```

### Tauri Application

```bash
# From frontend directory
npm run tauri:dev    # Development mode
npm run tauri:build  # Production build
npm run tauri:bundle # Create installers

# Or from project root
cd src-tauri
source ../env.sh
cargo tauri dev
cargo tauri build
```

## 🏗️ Build Scripts

### Production Build

```bash
./build.sh
```

This will:
1. Clean previous builds
2. Install frontend dependencies
3. Build backend library
4. Run backend tests
5. Build frontend
6. Build Tauri application
7. Show build artifacts

### Development Mode

```bash
./dev.sh
```

Options:
- `--frontend-only`: Start only Svelte dev server
- `--backend-only`: Backend development mode
- `--tauri-only`: Start only Tauri (requires built frontend)
- `--help`: Show usage information

## 🧪 Testing

### Backend Tests

```bash
cd backend
source ../env.sh
cargo test

# Or use the dev script
./dev.sh test
./dev.sh test-watch  # Watch mode (requires cargo-watch)
```

### Frontend Tests

```bash
cd frontend
npm test            # Run tests
npm run test:watch  # Watch mode
```

### Integration Tests

```bash
# Full test suite
./build.sh  # This includes backend tests

# Manual integration testing
cd backend
cargo run --example cli_demo
```

## 📦 Dependencies

### System Requirements

- **Rust** (latest stable)
- **Node.js** (18+ recommended)
- **GCC 13** (critical for RocksDB)
- **G++ 13** (critical for RocksDB)

### Backend Dependencies

Key dependencies in `backend/Cargo.toml`:
- **rocksdb**: Database storage
- **fastembed**: AI embeddings
- **hnsw_rs**: Vector search
- **tauri**: Desktop app framework
- **serde**: Serialization
- **tokio**: Async runtime

### Frontend Dependencies

Key dependencies in `frontend/package.json`:
- **Svelte**: UI framework
- **Vite**: Build tool
- **TypeScript**: Type safety
- **Tauri API**: Desktop integration

## 🎯 Common Tasks

### Adding New Backend Features

1. Create new module in `backend/src/`
2. Add to `backend/src/lib.rs`
3. Write tests
4. Update documentation

### Adding New Frontend Components

1. Create component in `frontend/src/lib/`
2. Import in parent component
3. Add to TypeScript types if needed

### Adding New Tauri Commands

1. Add command function in `src-tauri/src/main.rs`
2. Register in `tauri::Builder`
3. Call from frontend using `@tauri-apps/api`

### Schema Development

```bash
cd backend
source ../env.sh

# Test schema system with custom schemas
cargo run --example cli_demo ../src-tauri/examples/data/memory.json ../src-tauri/examples/schemas

# Test with default paths
cargo run --example cli_demo

# Or explore the schema modules
cargo doc --open --no-deps
```

### Data Path Configuration

```bash
# Set custom paths for development
export UFORGE_SCHEMA_DIR="/path/to/schemas"
export UFORGE_DATA_FILE="/path/to/data.json"
source env.sh

# Test with different data
cargo run --example cli_demo custom.json ./schemas

# See PATH_CONFIGURATION.md for complete guide
```

## 🐛 Troubleshooting

### RocksDB Compilation Errors

**Problem**: Cryptic compilation errors mentioning RocksDB or C++ linking.

**Solution**: 
```bash
# Ensure environment variables are set
source env.sh
# Then retry your command
```

### Frontend Not Loading

**Problem**: Blank page or connection refused.

**Solution**:
```bash
cd frontend
npm run dev  # Start dev server first
# Then in another terminal:
cd ../src-tauri
source ../env.sh
cargo tauri dev
```

### Tauri Build Failures

**Problem**: Tauri fails to build or bundle.

**Solutions**:
1. Ensure frontend is built: `cd frontend && npm run build`
2. Set environment variables: `source env.sh`
3. Clean and rebuild: `cargo clean && cargo build`

### Missing Dependencies

**Problem**: Import errors or missing modules.

**Solutions**:
```bash
# Backend dependencies
cd backend && cargo build

# Frontend dependencies  
cd frontend && npm install

# Check versions
cd backend && cargo tree
cd frontend && npm list
```

### Tauri Dev Server Constantly Rebuilding

**Problem**: Tauri dev server rebuilds the app every time database files change, showing messages like:
```
Info File src-tauri/default-project/db/000116.dbtmp changed. Rebuilding application...
Info File src-tauri/default-project/db/OPTIONS-000118 changed. Rebuilding application...
```

**Solution**: The `.taurignore` file excludes database files from file watching:
```bash
# The .taurignore file in src-tauri/ already excludes:
# - default-project/db/
# - *.db, *.dbtmp, *.log files
# - MANIFEST-*, CURRENT, OPTIONS-* files

# If you still see rebuilds, verify the file exists:
ls -la src-tauri/.taurignore

# Restart the dev server after creating .taurignore:
./dev.sh
```

## 📝 Development Notes

### Schema System Integration

- **Schema Loading**: Both Tauri app and CLI demo use `SchemaIngestion::load_schemas_from_directory` directly
- **Data Import**: JSON data ingestion uses `DataIngestion` from `data_ingestion.rs` module
- **Flexible Types**: Support for String, Text, Number, Boolean, Array, Object, Reference, Enum
- **Validation**: Real-time validation with min/max length, patterns, allowed values
- **Relationships**: Custom edge types with descriptions and cardinality
- **Clear Separation**: Schema ingestion (`SchemaIngestion`) vs Data ingestion (`DataIngestion`)

### Workspace Benefits

- **Shared dependencies**: Common crates use workspace versions
- **Unified builds**: Single `cargo build` for all Rust code
- **Better IDE support**: Language servers understand the full project

### Path Dependencies

- `src-tauri` depends on `backend` via `path = "../backend"`
- Frontend builds to `../dist` for Tauri consumption
- All build artifacts respect the new structure

### Environment Variables

These are CRITICAL and REQUIRED for any RocksDB compilation:
- `CC=gcc-13`: C compiler
- `CXX=g++-13`: C++ compiler  
- `WEBKIT_DISABLE_DMABUF_RENDERER=1`: WebKit compatibility

### Performance Tips

- Use `cargo build --release` for production backends
- Enable Vite's build optimizations for frontend
- Consider splitting large components for better load times

## 🚀 Deployment

### Desktop Application

```bash
./build.sh
# Installers will be in: src-tauri/target/release/bundle/
```

### Development Builds

```bash
./dev.sh
# Development builds with debug symbols and hot reload
```

## 🤝 Contributing

1. **Set up environment**: `source env.sh`
2. **Follow structure**: Keep backend, frontend, and Tauri separate
3. **Test thoroughly**: Run both `./build.sh` and component tests
4. **Format code**: Use `cargo fmt` and `npm run format`
5. **Update docs**: Keep this file and code comments current

## 📚 Additional Resources

- **Backend docs**: `cd backend && ./dev.sh doc`
- **Schema system**: See `SCHEMA_SYSTEM.md`
- **UI guide**: See `UI_GUIDE.md`
- **Path configuration**: See `PATH_CONFIGURATION.md`
- **Implementation notes**: See `CLAUDE.MD`
- **Tauri docs**: https://tauri.app/
- **Svelte docs**: https://svelte.dev/

---

**Remember**: Always `source env.sh` or set the environment variables before any Rust/Cargo commands! 🔧
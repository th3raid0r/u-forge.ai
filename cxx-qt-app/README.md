# U-Forge CXX-Qt Demo

## Overview

This directory contains a working proof-of-concept CXX-Qt application that validates the migration approach from Tauri to Qt/QML for U-Forge. 

**Status**: ✅ Functional demo with validated architecture patterns

## Quick Start

```bash
# Ensure Qt 6.0+ is installed with development packages
# Run the demo
./run.sh

# Or manually
cargo build
cargo run
```

## What This Demonstrates

- **Rust-Qt Integration**: Seamless property binding and method invocation
- **Modern QML Interface**: Responsive UI with native performance
- **Memory Safety**: Pin-based borrowing patterns working correctly
- **Build Pipeline**: Cargo-based build with Qt resource compilation

## Key Files

- `src/demo_object.rs` - CXX-Qt bridge patterns and implementation
- `qml/main.qml` - Modern QML interface with interactive controls
- `build.rs` - Qt integration and QML module compilation
- `run.sh` - Convenience script for building and running

## Documentation

For complete migration planning, architecture details, and validated technical approach, see:

**📋 [../CXXQT_MIGRATION.md](../CXXQT_MIGRATION.md)**

This consolidated document contains:
- Proof-of-concept validation results
- Technical implementation details  
- Validated dependencies and build configuration
- Migration timeline and next steps
- Success metrics and risk assessment

## Prerequisites

- **Rust**: 1.70+ (2021 edition)
- **Qt**: 6.0+ development packages
- **qmake**: Must be in PATH or set via QMAKE environment variable

See the main migration document for platform-specific setup instructions.
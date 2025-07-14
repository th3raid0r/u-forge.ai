#!/bin/bash

# Environment Setup Script for u-forge.ai
# Source this file before running any cargo commands that build RocksDB
# Usage: source env.sh

# CRITICAL: Environment variables required for RocksDB compilation
export CC=gcc-13
export CXX=g++-13
export WEBKIT_DISABLE_DMABUF_RENDERER=1

# Optional: Set default paths for data ingestion
# These can be overridden by setting these environment variables
# For development with Tauri:
export UFORGE_SCHEMA_DIR="./examples/schemas"
export UFORGE_DATA_FILE="./examples/data/memory.json"

# For development with backend CLI:
# export UFORGE_SCHEMA_DIR="${UFORGE_SCHEMA_DIR:-./examples/schemas}"
# export UFORGE_DATA_FILE="${UFORGE_DATA_FILE:-./examples/data/memory.json}"

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}✅ Environment variables set for u-forge.ai development${NC}"
echo -e "${YELLOW}📝 Environment:${NC}"
echo "   CC=$CC"
echo "   CXX=$CXX"
echo "   WEBKIT_DISABLE_DMABUF_RENDERER=$WEBKIT_DISABLE_DMABUF_RENDERER"
echo ""
echo -e "${YELLOW}📁 Data Paths:${NC}"
echo "   UFORGE_SCHEMA_DIR=$UFORGE_SCHEMA_DIR"
echo "   UFORGE_DATA_FILE=$UFORGE_DATA_FILE"
echo ""
echo -e "${YELLOW}💡 Usage examples:${NC}"
echo "   cargo build                    # Build with correct environment"
echo "   cargo test                     # Run tests"
echo "   cargo run --example cli_demo   # Run CLI demo"
echo "   cd backend && cargo check      # Check backend code"
echo "   cd src-tauri && cargo build    # Build Tauri app"
echo ""
echo -e "${YELLOW}🗂️  Path Configuration:${NC}"
echo "   # Override schema directory:"
echo "   export UFORGE_SCHEMA_DIR=/path/to/schemas"
echo "   # Override data file:"
echo "   export UFORGE_DATA_FILE=/path/to/data.json"
echo "   # CLI with custom paths:"
echo "   cargo run --example cli_demo /path/to/data.json /path/to/schemas"
echo ""
echo -e "${GREEN}🚀 Ready for development!${NC}"

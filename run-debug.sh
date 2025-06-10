#!/bin/bash

# Simple Debug Runner for u-forge.ai
# Starts frontend, waits for it, then starts Tauri with clear logging

set -e

# CRITICAL: Set environment variables for RocksDB compilation
export CC=gcc-13
export CXX=g++-13
export WEBKIT_DISABLE_DMABUF_RENDERER=1

# Set paths for development
export UFORGE_SCHEMA_DIR="${UFORGE_SCHEMA_DIR:-./src-tauri/examples/schemas}"
export UFORGE_DATA_FILE="${UFORGE_DATA_FILE:-./src-tauri/examples/data/memory.json}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo -e "${BLUE}🚀 u-forge.ai Debug Runner${NC}"
echo -e "${BLUE}========================${NC}"
echo ""

# Cleanup on exit
cleanup() {
    echo -e "\n${YELLOW}🛑 Cleaning up...${NC}"
    pkill -f "npm run dev" 2>/dev/null || true
    pkill -f "cargo tauri dev" 2>/dev/null || true
    exit 0
}
trap cleanup INT TERM

# Check directory
if [ ! -f "Cargo.toml" ] || [ ! -d "frontend" ] || [ ! -d "src-tauri" ]; then
    echo -e "${RED}❌ Run this from u-forge.ai root directory${NC}"
    exit 1
fi

# Build backend first
echo -e "${BLUE}🔧 Building backend...${NC}"
cd backend
cargo build --quiet
cd ..
echo -e "${GREEN}✅ Backend built${NC}"

# Install frontend deps if needed
echo -e "${BLUE}🔧 Checking frontend...${NC}"
cd frontend
if [ ! -d "node_modules" ]; then
    echo -e "${YELLOW}📦 Installing dependencies...${NC}"
    npm install --silent
fi
cd ..
echo -e "${GREEN}✅ Frontend ready${NC}"

echo ""
echo -e "${BLUE}🌐 Starting frontend server...${NC}"
echo -e "${YELLOW}Frontend logs will appear below:${NC}"
echo "============================================"

# Start frontend in background
cd frontend
npm run dev &
FRONTEND_PID=$!
cd ..

# Wait for frontend
echo ""
echo -e "${BLUE}⏳ Waiting for frontend on port 1420...${NC}"
for i in {1..30}; do
    if curl -s http://localhost:1420 >/dev/null 2>&1; then
        echo -e "${GREEN}✅ Frontend ready!${NC}"
        break
    fi
    sleep 1
    echo -n "."
done

echo ""
echo ""
echo -e "${BLUE}🖥️  Starting Tauri app...${NC}"
echo -e "${YELLOW}Tauri logs will appear below:${NC}"
echo "============================================"

# Start Tauri in foreground
cd src-tauri
exec cargo tauri dev
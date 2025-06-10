#!/bin/bash

# u-forge.ai Build Script
# Builds the entire application: frontend, backend, and Tauri app

set -e  # Exit on any error

# CRITICAL: Set environment variables for RocksDB compilation
export CC=gcc-13
export CXX=g++-13
export WEBKIT_DISABLE_DMABUF_RENDERER=1

echo "🚀 Building u-forge.ai..."

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored output
print_step() {
    echo -e "${BLUE}📦 $1${NC}"
}

print_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

print_warning() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

print_error() {
    echo -e "${RED}❌ $1${NC}"
}

# Check if we're in the right directory
if [ ! -f "Cargo.toml" ] || [ ! -d "frontend" ] || [ ! -d "src-tauri" ]; then
    print_error "This script must be run from the u-forge.ai root directory"
    exit 1
fi

# Clean previous builds
print_step "Cleaning previous builds..."
rm -rf dist target frontend/node_modules/.vite frontend/dist
print_success "Cleaned previous builds"

# Step 1: Install frontend dependencies
print_step "Installing frontend dependencies..."
cd frontend
if [ ! -d "node_modules" ]; then
    npm install
else
    npm ci
fi
cd ..
print_success "Frontend dependencies installed"

# Step 2: Build backend library
print_step "Building backend library..."
cd backend
cargo build --release --lib
cd ..
print_success "Backend library built"

# Step 3: Test backend
print_step "Running backend tests..."
cd backend
cargo test --release
cd ..
print_success "Backend tests passed"

# Step 4: Build frontend
print_step "Building frontend..."
cd frontend
npm run build
cd ..
print_success "Frontend built and output to dist/"

# Step 5: Build Tauri application
print_step "Building Tauri application..."
cd src-tauri
cargo tauri build
cd ..
print_success "Tauri application built"

# Step 6: Show build artifacts
print_step "Build completed! Artifacts:"
echo ""
echo "Frontend build:"
if [ -d "dist" ]; then
    echo "  📁 dist/ - Frontend static files"
fi

echo ""
echo "Backend library:"
if [ -f "target/release/libu_forge_ai.rlib" ]; then
    echo "  📚 target/release/libu_forge_ai.rlib - Backend library"
fi

echo ""
echo "Tauri application:"
if [ -d "src-tauri/target/release/bundle" ]; then
    echo "  📱 src-tauri/target/release/bundle/ - Platform-specific bundles"
    ls -la src-tauri/target/release/bundle/
fi

print_success "Build completed successfully! 🎉"

echo ""
echo "Next steps:"
echo "  • Install the application from src-tauri/target/release/bundle/"
echo "  • Or run in development mode with: ./dev.sh"
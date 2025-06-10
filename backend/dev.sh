#!/bin/bash

# Backend Development Script for u-forge.ai
# Provides backend-specific development tasks

set -e  # Exit on any error

# CRITICAL: Set environment variables for RocksDB compilation
export CC=gcc-13
export CXX=g++-13
export WEBKIT_DISABLE_DMABUF_RENDERER=1

echo "🔧 u-forge.ai Backend Development Tools"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored output
print_step() {
    echo -e "${BLUE}🔧 $1${NC}"
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

# Function to show help
show_help() {
    echo "Backend Development Script for u-forge.ai"
    echo ""
    echo "Usage: $0 [command]"
    echo ""
    echo "Commands:"
    echo "  build         - Build the backend library"
    echo "  test          - Run all tests"
    echo "  test-watch    - Run tests in watch mode"
    echo "  check         - Check code without building"
    echo "  clippy        - Run Clippy linter"
    echo "  fmt           - Format code"
    echo "  doc           - Generate and open documentation"
    echo "  clean         - Clean build artifacts"
    echo "  cli-demo      - Run the CLI demo example"
    echo "  bench         - Run benchmarks (if available)"
    echo "  features      - List available features"
    echo "  deps          - Check dependency versions"
    echo "  help          - Show this help message"
    echo ""
    echo "Examples:"
    echo "  $0 build      # Build the backend"
    echo "  $0 test       # Run tests"
    echo "  $0 cli-demo   # Run CLI demo"
}

# Function to build backend
build() {
    print_step "Building backend library..."
    cargo build --lib
    print_success "Backend library built successfully"
    
    print_step "Building examples..."
    cargo build --examples
    print_success "Examples built successfully"
}

# Function to run tests
test() {
    print_step "Running backend tests..."
    cargo test --lib
    print_success "Tests completed"
}

# Function to run tests in watch mode
test_watch() {
    print_step "Starting test watch mode..."
    print_warning "This requires cargo-watch. Install with: cargo install cargo-watch"
    cargo watch -x test
}

# Function to check code
check() {
    print_step "Checking backend code..."
    cargo check --lib
    cargo check --examples
    print_success "Code check completed"
}

# Function to run Clippy
clippy() {
    print_step "Running Clippy linter..."
    cargo clippy --lib -- -D warnings
    cargo clippy --examples -- -D warnings
    print_success "Clippy check completed"
}

# Function to format code
fmt() {
    print_step "Formatting backend code..."
    cargo fmt --all
    print_success "Code formatting completed"
}

# Function to generate documentation
doc() {
    print_step "Generating documentation..."
    cargo doc --lib --open --no-deps
    print_success "Documentation generated and opened"
}

# Function to clean build artifacts
clean() {
    print_step "Cleaning backend build artifacts..."
    cargo clean
    rm -rf .fastembed_cache
    print_success "Backend cleaned"
}

# Function to run CLI demo
cli_demo() {
    print_step "Running CLI demo..."
    cargo run --example cli_demo
}

# Function to run benchmarks
bench() {
    print_step "Running benchmarks..."
    if cargo bench --help >/dev/null 2>&1; then
        cargo bench
        print_success "Benchmarks completed"
    else
        print_warning "No benchmarks configured"
    fi
}

# Function to list features
features() {
    print_step "Available features:"
    echo ""
    echo "🔧 Backend Features:"
    grep -A 20 "\[features\]" Cargo.toml | grep -E "^[a-zA-Z]" | while read line; do
        echo "  • $line"
    done
    echo ""
    echo "To build with features: cargo build --features feature-name"
}

# Function to check dependencies
deps() {
    print_step "Checking dependency versions..."
    
    print_step "Outdated dependencies:"
    if command -v cargo-outdated >/dev/null 2>&1; then
        cargo outdated
    else
        print_warning "cargo-outdated not installed. Install with: cargo install cargo-outdated"
    fi
    
    print_step "Dependency tree:"
    cargo tree --depth 1
}

# Function to run development workflow
dev_workflow() {
    print_step "Running full development workflow..."
    
    print_step "1/5 - Formatting code..."
    cargo fmt --all
    
    print_step "2/5 - Checking code..."
    cargo check --lib --examples
    
    print_step "3/5 - Running Clippy..."
    cargo clippy --lib --examples -- -D warnings
    
    print_step "4/5 - Running tests..."
    cargo test --lib
    
    print_step "5/5 - Building examples..."
    cargo build --examples
    
    print_success "Development workflow completed! 🎉"
}

# Main script logic
case "${1:-help}" in
    build)
        build
        ;;
    test)
        test
        ;;
    test-watch)
        test_watch
        ;;
    check)
        check
        ;;
    clippy)
        clippy
        ;;
    fmt)
        fmt
        ;;
    doc)
        doc
        ;;
    clean)
        clean
        ;;
    cli-demo)
        cli_demo
        ;;
    bench)
        bench
        ;;
    features)
        features
        ;;
    deps)
        deps
        ;;
    workflow)
        dev_workflow
        ;;
    help)
        show_help
        ;;
    *)
        print_error "Unknown command: $1"
        echo ""
        show_help
        exit 1
        ;;
esac
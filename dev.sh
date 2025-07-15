#!/bin/bash

# u-forge.ai Consolidated Development Script
# Enhanced logging, process management, and multi-mode support

set -e  # Exit on any error

# CRITICAL: Set environment variables for RocksDB compilation
export CC=gcc-13
export CXX=g++-13
export WEBKIT_DISABLE_DMABUF_RENDERER=1

# Set default paths for development
export UFORGE_SCHEMA_DIR="./defaults/schemas"
export UFORGE_DATA_FILE="./defaults/data/memory.json"

# Logging configuration
export RUST_LOG="${RUST_LOG:-info}"
export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
MAGENTA='\033[0;95m'
GRAY='\033[0;37m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# Log prefixes with colors
BACKEND_PREFIX="${PURPLE}[BACKEND]${NC}"
SYSTEM_PREFIX="${BLUE}[SYSTEM]${NC}"

# Configuration
SHOW_TIMESTAMPS=true
LOG_LEVEL="info"
MODE="default"

# Function to print colored output with timestamps
print_log() {
    local prefix="$1"
    local message="$2"
    local timestamp=""

    if [ "$SHOW_TIMESTAMPS" = true ]; then
        timestamp="$(date '+%H:%M:%S') "
    fi

    echo -e "${timestamp}${prefix} ${message}"
}

print_system() {
    print_log "$SYSTEM_PREFIX" "$1"
}

print_success() {
    print_log "${GREEN}[SUCCESS]${NC}" "$1"
}

print_warning() {
    print_log "${YELLOW}[WARNING]${NC}" "$1"
}

print_error() {
    print_log "${RED}[ERROR]${NC}" "$1"
}

print_debug() {
    print_log "${GRAY}[DEBUG]${NC}" "$1"
}

print_step() {
    echo ""
    print_log "${BOLD}${BLUE}[STEP]${NC}" "$1"
}


# Function to process logs with prefixes
process_logs() {
    local prefix="$1"
    local pipe="$2"

    while IFS= read -r line; do
        print_log "$prefix" "$line"
    done < "$pipe"
}

# Function to start log processor in background
start_log_processor() {
    local prefix="$1"
    local pipe="$2"

    process_logs "$prefix" "$pipe" &
}


# Function to show system information
show_system_info() {
    print_step "System Information"
    print_debug "Rust: $(rustc --version 2>/dev/null || echo 'not found')"
    print_debug "Cargo: $(cargo --version 2>/dev/null || echo 'not found')"
    print_debug "Working directory: $(pwd)"
    print_debug "Schema dir: $UFORGE_SCHEMA_DIR"
    print_debug "Data file: $UFORGE_DATA_FILE"
    print_debug "Rust log level: $RUST_LOG"
    print_debug "Mode: $MODE"
}

# Function to check prerequisites
check_prerequisites() {
    print_step "Checking Prerequisites"

    # Check directory
    if [ ! -f "Cargo.toml" ] ; then
        print_error "This script must be run from the u-forge.ai root directory"
        exit 1
    fi

    # Check required tools
    local missing_tools=""

    if ! command -v cargo >/dev/null 2>&1; then
        missing_tools="$missing_tools cargo"
    fi

    if [ ! -z "$missing_tools" ]; then
        print_error "Missing required tools:$missing_tools"
        print_error "Please install the missing tools and try again"
        exit 1
    fi

    print_success "All prerequisites met"
}


# Function to show help
show_help() {
    echo "u-forge.ai Development Script"
    echo ""
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Modes:"
    echo "  --backend-only      Backend development mode"
    echo ""
    echo "Options:"
    echo "  --log-level LEVEL   Set Rust log level (trace,debug,info,warn,error) [default: info]"
    echo "  --no-timestamps     Disable timestamps in logs"
    echo "  --help, -h          Show this help message"
    echo ""
    echo "Environment Variables:"
    echo "  RUST_LOG            Rust logging level (overrides --log-level)"
    echo "  UFORGE_SCHEMA_DIR   Schema directory path"
    echo "  UFORGE_DATA_FILE    Data file path"
    echo ""
    echo "Examples:"
    echo "  $0                          # Start full development environment"
    echo "  $0 --frontend-only          # Frontend development only"
    echo "  $0 --log-level debug        # Enable debug logging"
    echo "  $0 --tauri-only             # Tauri only (requires frontend running)"
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --backend-only)
            MODE="backend"
            shift
            ;;
        --log-level)
            if [ -z "$2" ]; then
                print_error "--log-level requires a value"
                exit 1
            fi
            export RUST_LOG="$2"
            LOG_LEVEL="$2"
            shift 2
            ;;
        --no-timestamps)
            SHOW_TIMESTAMPS=false
            shift
            ;;
        --help|-h)
            show_help
            exit 0
            ;;
        *)
            print_error "Unknown option: $1"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
done

# Main execution
echo ""
echo -e "${BOLD}${BLUE}🚀 u-forge.ai Development Environment${NC}"
echo -e "${BOLD}${BLUE}====================================${NC}"
echo ""

show_system_info
check_prerequisites

case $MODE in
    "backend")
        print_step "Backend-Only Mode"
        cd backend

        print_success "Backend ready for development"
        echo ""
        print_system "Available commands:"
        print_system "  cargo run --example cli_demo    # Run CLI demo"
        print_system "  cargo test                      # Run tests"
        print_system "  cargo doc --open               # Open documentation"
        print_system "  cargo run --bin <binary>        # Run specific binary"
        echo ""
        print_system "Press Ctrl+C to exit"
        cargo test
        cargo run --example cli_demo

        # Keep the script running for backend development
        while true; do
            sleep 1
        done
        ;;

    *)
        # Default: Full development environment
        print_step "Full Development Environment Disabled"
        exit 1

esac


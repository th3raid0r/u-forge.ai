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
FRONTEND_PREFIX="${CYAN}[FRONTEND]${NC}"
TAURI_PREFIX="${MAGENTA}[TAURI]${NC}"
BACKEND_PREFIX="${PURPLE}[BACKEND]${NC}"
SYSTEM_PREFIX="${BLUE}[SYSTEM]${NC}"

# Process tracking
FRONTEND_PID=""
TAURI_PID=""
LOG_PIPE_FRONTEND=""
LOG_PIPE_TAURI=""

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

# Enhanced cleanup function
cleanup() {
    echo ""
    print_warning "Shutting down development servers..."

    # Kill named pipes
    if [ ! -z "$LOG_PIPE_FRONTEND" ] && [ -p "$LOG_PIPE_FRONTEND" ]; then
        rm -f "$LOG_PIPE_FRONTEND"
    fi
    if [ ! -z "$LOG_PIPE_TAURI" ] && [ -p "$LOG_PIPE_TAURI" ]; then
        rm -f "$LOG_PIPE_TAURI"
    fi

    # Kill processes by PID
    if [ ! -z "$FRONTEND_PID" ]; then
        kill $FRONTEND_PID 2>/dev/null || true
        print_debug "Killed frontend process $FRONTEND_PID"
    fi
    if [ ! -z "$TAURI_PID" ]; then
        kill $TAURI_PID 2>/dev/null || true
        print_debug "Killed Tauri process $TAURI_PID"
    fi

    # Fallback: kill by process name
    pkill -f "npm run dev" 2>/dev/null || true
    pkill -f "cargo tauri dev" 2>/dev/null || true

    # Kill any remaining log processors
    jobs -p | xargs -r kill 2>/dev/null || true

    print_success "Cleanup complete"
    exit 0
}

# Set up signal handlers
trap cleanup INT TERM

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

# Function to check if port is in use
check_port() {
    local port="$1"

    if command -v lsof >/dev/null 2>&1; then
        lsof -i ":$port" >/dev/null 2>&1
    elif command -v netstat >/dev/null 2>&1; then
        netstat -ln 2>/dev/null | grep -q ":$port "
    elif command -v ss >/dev/null 2>&1; then
        ss -ln 2>/dev/null | grep -q ":$port "
    else
        # Fallback: try to connect
        timeout 1 bash -c "</dev/tcp/localhost/$port" 2>/dev/null
    fi
}

# Function to wait for service to be ready
wait_for_service() {
    local service_name="$1"
    local port="$2"
    local max_wait="$3"
    local url="http://localhost:$port"

    print_system "Waiting for $service_name on port $port..."

    local count=0
    while [ $count -lt $max_wait ]; do
        if check_port "$port"; then
            if command -v curl >/dev/null 2>&1; then
                if curl -s "$url" >/dev/null 2>&1; then
                    print_success "$service_name is ready on port $port"
                    return 0
                fi
            else
                print_success "$service_name detected on port $port"
                return 0
            fi
        fi

        count=$((count + 1))
        printf "."
        sleep 1
    done

    echo ""
    print_warning "$service_name didn't start within ${max_wait} seconds"
    return 1
}

# Function to show system information
show_system_info() {
    print_step "System Information"
    print_debug "Node.js: $(node --version 2>/dev/null || echo 'not found')"
    print_debug "npm: $(npm --version 2>/dev/null || echo 'not found')"
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
    if [ ! -f "Cargo.toml" ] || [ ! -d "frontend" ] || [ ! -d "src-tauri" ]; then
        print_error "This script must be run from the u-forge.ai root directory"
        exit 1
    fi

    # Check required tools
    local missing_tools=""

    if ! command -v npm >/dev/null 2>&1; then
        missing_tools="$missing_tools npm"
    fi

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

# Function to setup development environment
setup_environment() {
    print_step "Setting up Development Environment"

    # Install frontend dependencies if needed
    print_system "Checking frontend dependencies..."
    cd frontend
    if [ ! -d "node_modules" ]; then
        print_system "Installing frontend dependencies..."
        npm install
    fi
    cd ..
    print_success "Frontend dependencies ready"

    # Build backend library in debug mode
    print_system "Building backend library (debug mode)..."
    cd backend
    cargo build --quiet
    cd ..
    print_success "Backend library built"
}

# Function to start frontend with enhanced logging
start_frontend() {
    print_step "Starting Frontend Development Server"

    # Create named pipe for frontend logs
    LOG_PIPE_FRONTEND="/tmp/uforge_frontend_$$"
    mkfifo "$LOG_PIPE_FRONTEND"

    # Start log processor
    start_log_processor "$FRONTEND_PREFIX" "$LOG_PIPE_FRONTEND"

    # Start frontend
    cd frontend
    npm run dev > "$LOG_PIPE_FRONTEND" 2>&1 &
    FRONTEND_PID=$!
    cd ..

    print_system "Frontend started with PID: $FRONTEND_PID"
    print_system "Frontend logs will appear with $FRONTEND_PREFIX prefix"
}

# Function to start Tauri with enhanced logging
start_tauri() {
    print_step "Starting Tauri Development Mode"

    # Create named pipe for Tauri logs
    LOG_PIPE_TAURI="/tmp/uforge_tauri_$$"
    mkfifo "$LOG_PIPE_TAURI"

    # Start log processor
    start_log_processor "$TAURI_PREFIX" "$LOG_PIPE_TAURI"

    # Start Tauri
    cd src-tauri
    cargo tauri dev > "$LOG_PIPE_TAURI" 2>&1 &
    TAURI_PID=$!
    cd ..

    print_system "Tauri started with PID: $TAURI_PID"
    print_system "Tauri logs will appear with $TAURI_PREFIX prefix"
}

# Function to show help
show_help() {
    echo "u-forge.ai Development Script"
    echo ""
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Modes:"
    echo "  --frontend-only     Start only the frontend dev server"
    echo "  --tauri-only        Start only Tauri dev mode (requires frontend on port 1420)"
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
        --frontend-only)
            MODE="frontend"
            shift
            ;;
        --tauri-only)
            MODE="tauri"
            shift
            ;;
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
setup_environment

case $MODE in
    "frontend")
        print_step "Frontend-Only Mode"
        start_frontend

        print_success "Frontend development server started!"
        print_system "Frontend URL: http://localhost:1420"
        print_system "Press Ctrl+C to stop"

        # Keep script running and show logs
        wait $FRONTEND_PID
        ;;

    "tauri")
        print_step "Tauri-Only Mode"
        print_warning "Make sure frontend dev server is running on port 1420"

        if ! wait_for_service "Frontend" 1420 5; then
            print_error "Frontend server not detected on port 1420"
            print_error "Please start frontend first with: $0 --frontend-only"
            exit 1
        fi

        start_tauri

        print_success "Tauri development mode started!"
        print_system "Press Ctrl+C to stop"

        # Keep script running and show logs
        wait $TAURI_PID
        ;;

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

        # Keep the script running for backend development
        while true; do
            sleep 1
        done
        ;;

    *)
        # Default: Full development environment
        print_step "Full Development Environment"

        # Start frontend first
        start_frontend

        # Wait for frontend to be ready
        if wait_for_service "Frontend" 1420 30; then
            # Start Tauri
            start_tauri

            print_success "Development environment ready!"
            echo ""
            print_system "🌐 Frontend: http://localhost:1420"
            print_system "🖥️  Tauri: Starting up..."
            echo ""
            print_system "📋 Log Legend:"
            print_system "  $FRONTEND_PREFIX Frontend development server"
            print_system "  $TAURI_PREFIX Tauri application"
            print_system "  $SYSTEM_PREFIX System messages"
            echo ""
            print_system "Press Ctrl+C to stop all services"

            # Wait for both processes
            while kill -0 $FRONTEND_PID 2>/dev/null && kill -0 $TAURI_PID 2>/dev/null; do
                sleep 1
            done

            print_warning "One or more services stopped unexpectedly"
        else
            print_error "Frontend failed to start properly"
            cleanup
            exit 1
        fi
        ;;
esac

# If we get here, cleanup and exit
cleanup

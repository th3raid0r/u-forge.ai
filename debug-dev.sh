#!/bin/bash

# u-forge.ai Debug Development Script
# Simplified version that runs everything in foreground with combined logs

set -e  # Exit on any error

# CRITICAL: Set environment variables for RocksDB compilation
export CC=gcc-13
export CXX=g++-13
export WEBKIT_DISABLE_DMABUF_RENDERER=1

# Optional: Set default paths for development
export UFORGE_SCHEMA_DIR="${UFORGE_SCHEMA_DIR:-./src-tauri/examples/schemas}"
export UFORGE_DATA_FILE="${UFORGE_DATA_FILE:-./src-tauri/examples/data/memory.json}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
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

print_debug() {
    echo -e "${PURPLE}🐛 $1${NC}"
}

print_info() {
    echo -e "${CYAN}ℹ️  $1${NC}"
}

echo "🐛 Starting u-forge.ai DEBUG development environment..."
echo "   This script will show all logs in one place for easier debugging"
echo ""

# Function to cleanup background processes on exit
cleanup() {
    echo -e "\n${YELLOW}🛑 Shutting down development servers...${NC}"
    jobs -p | xargs -r kill 2>/dev/null || true
    exit 0
}

# Set up signal handlers
trap cleanup INT TERM

# Check if we're in the right directory
if [ ! -f "Cargo.toml" ] || [ ! -d "frontend" ] || [ ! -d "src-tauri" ]; then
    print_error "This script must be run from the u-forge.ai root directory"
    exit 1
fi

# Check for required tools
command -v npm >/dev/null 2>&1 || { print_error "npm is required but not installed. Please install Node.js and npm."; exit 1; }
command -v cargo >/dev/null 2>&1 || { print_error "cargo is required but not installed. Please install Rust."; exit 1; }

# Step 1: Install frontend dependencies if needed
print_step "Checking frontend dependencies..."
cd frontend
if [ ! -d "node_modules" ]; then
    print_step "Installing frontend dependencies..."
    npm install
fi
cd ..
print_success "Frontend dependencies ready"

# Step 2: Build backend library in debug mode
print_step "Building backend library (debug mode)..."
cd backend
cargo build
cd ..
print_success "Backend library built"

# Step 3: Show some system info for debugging
print_debug "System Information:"
print_debug "  - Node.js: $(node --version)"
print_debug "  - npm: $(npm --version)"
print_debug "  - Rust: $(rustc --version)"
print_debug "  - Cargo: $(cargo --version)"
print_debug "  - Working directory: $(pwd)"
print_debug "  - Schema dir: $UFORGE_SCHEMA_DIR"
print_debug "  - Data file: $UFORGE_DATA_FILE"
echo ""

# Step 4: Parse command line arguments for different modes
FRONTEND_ONLY=false
TAURI_ONLY=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --frontend-only)
            FRONTEND_ONLY=true
            shift
            ;;
        --tauri-only)
            TAURI_ONLY=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Debug Development Script - Shows all logs together"
            echo ""
            echo "Options:"
            echo "  --frontend-only    Start only the frontend dev server"
            echo "  --tauri-only       Start only Tauri dev mode (requires frontend on port 1420)"
            echo "  --help, -h         Show this help message"
            echo ""
            echo "Default: Starts frontend dev server and waits for it to be ready,"
            echo "         then starts Tauri app in same terminal"
            exit 0
            ;;
        *)
            print_error "Unknown option: $1"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
done

if [ "$FRONTEND_ONLY" = true ]; then
    print_step "Starting frontend development server in foreground..."
    print_info "All frontend logs will appear below:"
    echo ""
    cd frontend
    npm run dev
    
elif [ "$TAURI_ONLY" = true ]; then
    print_step "Starting Tauri development mode..."
    print_warning "Make sure frontend dev server is running on port 1420"
    print_info "All Tauri logs will appear below:"
    echo ""
    cd src-tauri
    cargo tauri dev
    
else
    # Default: Start frontend first, wait for it, then start Tauri
    print_step "Starting frontend development server..."
    print_info "Starting frontend in background, will wait for it to be ready..."
    
    cd frontend
    npm run dev &
    FRONTEND_PID=$!
    cd ..
    
    print_info "Frontend PID: $FRONTEND_PID"
    print_info "Waiting for frontend server to start on port 1420..."
    
    # Wait for frontend to start with better detection
    MAX_WAIT=30
    WAIT_COUNT=0
    while [ $WAIT_COUNT -lt $MAX_WAIT ]; do
        if command -v curl >/dev/null 2>&1; then
            if curl -s http://localhost:1420 >/dev/null 2>&1; then
                print_success "Frontend server is ready on port 1420"
                break
            fi
        else
            # Fallback to netstat/ss
            if command -v netstat >/dev/null 2>&1; then
                if netstat -ln 2>/dev/null | grep -q ":1420 "; then
                    print_success "Frontend server detected on port 1420"
                    break
                fi
            elif command -v ss >/dev/null 2>&1; then
                if ss -ln 2>/dev/null | grep -q ":1420 "; then
                    print_success "Frontend server detected on port 1420"
                    break
                fi
            fi
        fi
        
        WAIT_COUNT=$((WAIT_COUNT + 1))
        printf "."
        sleep 1
    done
    
    if [ $WAIT_COUNT -ge $MAX_WAIT ]; then
        print_warning "Frontend server didn't start within ${MAX_WAIT} seconds"
        print_warning "Proceeding anyway - check if frontend is running manually"
    fi
    
    echo ""
    print_step "Starting Tauri development mode..."
    print_info "All Tauri logs will appear below (frontend logs in background):"
    print_info "Frontend is running on: http://localhost:1420"
    print_info "To see frontend logs, check another terminal or kill this and run with --frontend-only"
    echo ""
    echo "=================================="
    echo "    TAURI APPLICATION LOGS"
    echo "=================================="
    echo ""
    
    cd src-tauri
    cargo tauri dev
fi
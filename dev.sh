#!/bin/bash

# u-forge.ai Development Script
# Starts the development environment with frontend dev server and Tauri dev mode

set -e  # Exit on any error

# CRITICAL: Set environment variables for RocksDB compilation
export CC=gcc-13
export CXX=g++-13
export WEBKIT_DISABLE_DMABUF_RENDERER=1

# Optional: Set default paths for development
# These can be overridden by command line arguments or environment variables
export UFORGE_SCHEMA_DIR="${UFORGE_SCHEMA_DIR:-./src-tauri/examples/schemas}"
export UFORGE_DATA_FILE="${UFORGE_DATA_FILE:-./src-tauri/examples/data/memory.json}"

echo "🚀 Starting u-forge.ai development environment..."

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

# Function to cleanup background processes on exit
cleanup() {
    echo -e "\n${YELLOW}🛑 Shutting down development servers...${NC}"
    if [ ! -z "$FRONTEND_PID" ]; then
        kill $FRONTEND_PID 2>/dev/null || true
    fi
    if [ ! -z "$TAURI_PID" ]; then
        kill $TAURI_PID 2>/dev/null || true
    fi
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
print_success "Backend library built"

# Step 3: Skip backend tests in dev mode (for faster startup)
print_step "Skipping backend tests in dev mode (use 'cd backend && ./dev.sh test' to run tests)"
cd ..
print_success "Backend ready for development"

# Step 4: Start development servers
print_step "Starting development environment..."

# Parse command line arguments
FRONTEND_ONLY=false
BACKEND_ONLY=false
TAURI_ONLY=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --frontend-only)
            FRONTEND_ONLY=true
            shift
            ;;
        --backend-only)
            BACKEND_ONLY=true
            shift
            ;;
        --tauri-only)
            TAURI_ONLY=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --frontend-only    Start only the frontend dev server"
            echo "  --backend-only     Run only backend tests and examples"
            echo "  --tauri-only       Start only Tauri dev mode (requires frontend build)"
            echo "  --help, -h         Show this help message"
            exit 0
            ;;
        *)
            print_error "Unknown option: $1"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
done

if [ "$BACKEND_ONLY" = true ]; then
    print_step "Running backend in development mode..."
    cd backend
    print_success "Backend ready for development"
    echo ""
    echo "Available commands:"
    echo "  cargo run --example cli_demo    # Run CLI demo"
    echo "  cargo test                      # Run tests"
    echo "  cargo doc --open               # Open documentation"
    echo ""
    echo "Press Ctrl+C to exit"
    
    # Keep the script running for backend development
    while true; do
        sleep 1
    done
    
elif [ "$FRONTEND_ONLY" = true ]; then
    print_step "Starting frontend development server..."
    cd frontend
    npm run dev &
    FRONTEND_PID=$!
    cd ..
    
    print_success "Frontend development server started in background!"
    echo ""
    echo "🌐 Frontend dev server: http://localhost:1420"
    echo "🔧 Process ID: $FRONTEND_PID"
    echo ""
    echo "📝 To stop the server:"
    echo "  kill $FRONTEND_PID"
    echo "  or press Ctrl+C and run: pkill -f 'npm run dev'"
    echo ""
    echo "Press Ctrl+C to stop"
    
    # Wait for background process
    wait $FRONTEND_PID
    
elif [ "$TAURI_ONLY" = true ]; then
    print_step "Starting Tauri development mode..."
    print_warning "Make sure frontend dev server is running on port 1420"
    cd src-tauri
    cargo tauri dev
    
else
    # Default: Start both frontend and Tauri
    print_step "Starting frontend development server..."
    cd frontend
    npm run dev &
    FRONTEND_PID=$!
    cd ..
    
    # Wait for frontend to start
    print_step "Waiting for frontend server to start..."
    sleep 5
    
    # Check if frontend is running
    if command -v curl >/dev/null 2>&1; then
        if curl -s http://localhost:1420 > /dev/null 2>&1; then
            print_success "Frontend server is running on port 1420"
        else
            print_warning "Frontend server may still be starting..."
        fi
    else
        # Use netstat or ss as fallback
        if command -v netstat >/dev/null 2>&1; then
            if netstat -ln 2>/dev/null | grep -q ":1420 "; then
                print_success "Frontend server is running on port 1420"
            else
                print_warning "Frontend server may still be starting..."
            fi
        else
            print_warning "Waiting for frontend server to start on port 1420..."
        fi
    fi
    
    print_step "Starting Tauri development mode..."
    cd src-tauri
    cargo tauri dev &
    TAURI_PID=$!
    cd ..
    
    print_success "Development environment started!"
    echo ""
    echo "🌐 Frontend dev server: http://localhost:1420"
    echo "🖥️  Tauri app: Starting..."
    echo ""
    echo "📝 Logs:"
    echo "  Frontend logs will appear above"
    echo "  Tauri logs will appear below"
    echo ""
    echo "Press Ctrl+C to stop all servers"
    
    # Wait for background processes
    wait
fi
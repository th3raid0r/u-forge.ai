#!/bin/bash

# U-Forge CXX-Qt Demo Runner
# This script runs the CXX-Qt demo application

set -e

echo "🚀 Starting U-Forge CXX-Qt Demo..."

# Check if Qt is available
if ! command -v qmake &> /dev/null; then
    echo "❌ qmake not found. Please ensure Qt is installed and qmake is in your PATH."
    echo "   You can set the QMAKE environment variable to specify the path to qmake."
    exit 1
fi

# Display Qt version info
echo "📋 Qt Version Information:"
qmake -query QT_VERSION
echo "   Installation path: $(qmake -query QT_INSTALL_PREFIX)"

# Build and run the application
echo "🔨 Building CXX-Qt demo..."
cargo build --release

echo "🎯 Running CXX-Qt demo..."
echo "   Press Ctrl+C or click 'Quit' to exit"
echo ""

# Run the application
cargo run --release

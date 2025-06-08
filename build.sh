#!/bin/bash
# This script builds the u-forge.ai rust project

# Set environment variables, particularly the GCC version to 13 for dependencies
export CC=gcc-13
export CXX=g++-13

# Cargo build
cargo build

# Path Configuration Guide

This document explains how to configure data and schema paths for u-forge.ai across different deployment scenarios.

## Overview

u-forge.ai supports flexible path configuration for:
- **Schema directories**: JSON schema files that define object types and validation rules
- **Data files**: JSON data files containing nodes and edges to import

## Configuration Methods

### 1. Environment Variables (Recommended)

Set these environment variables to override default paths:

```bash
# Schema directory containing .json schema files
export UFORGE_SCHEMA_DIR="/path/to/schemas"

# Data file containing JSON objects and relationships  
export UFORGE_DATA_FILE="/path/to/data.json"
```

### 2. Command Line Arguments (CLI Demo)

The CLI demo accepts paths as positional arguments:

```bash
# Usage: cargo run --example cli_demo [DATA_FILE] [SCHEMA_DIR]
cargo run --example cli_demo                                    # Use defaults
cargo run --example cli_demo custom.json                       # Custom data file
cargo run --example cli_demo custom.json ./schemas             # Custom data and schema
cargo run --example cli_demo /abs/path/data.json /abs/path/schemas  # Absolute paths
```

## Default Path Resolution

The system tries paths in this order:

### Schema Directory
1. `$UFORGE_SCHEMA_DIR` (if set)
2. `./examples/schemas`
3. `../examples/schemas`  
4. `./src-tauri/examples/schemas`

### Data File
1. `$UFORGE_DATA_FILE` (if set)
2. `./examples/data/memory.json`
3. `../examples/data/memory.json`
4. `./src-tauri/examples/data/memory.json`

## Development Scenarios

### Backend CLI Development

```bash
# Set environment for backend CLI
export UFORGE_SCHEMA_DIR="./examples/schemas"
export UFORGE_DATA_FILE="./examples/data/memory.json"
source env.sh
cd backend
cargo run --example cli_demo
```

### Custom Data Development

```bash
# Using custom paths
export UFORGE_SCHEMA_DIR="/home/user/my-campaign/schemas"
export UFORGE_DATA_FILE="/home/user/my-campaign/world.json"
source env.sh
cargo run --example cli_demo
```

## Production Deployment

### Binary Distribution

For distributed binaries, embed or ship data files alongside the executable:

```bash
# Directory structure
my-app/
├── my-app                    # Executable
├── data/
│   ├── schemas/             # Schema files
│   │   ├── character.json
│   │   ├── location.json
│   │   └── ...
│   └── world.json           # Default data
└── run.sh                   # Launch script

# Launch script (run.sh)
#!/bin/bash
export UFORGE_SCHEMA_DIR="./data/schemas"
export UFORGE_DATA_FILE="./data/world.json"
./my-app
```


## Data File Format

Data files should contain line-delimited JSON with this format:

```json
{"type":"node","name":"Example Character","nodeType":"character","metadata":["tag1","property:value"]}
{"type":"edge","from":"Character A","to":"Location B","edgeType":"located_at"}
```

## Schema File Format

Schema files should be valid JSON Schema with additional u-forge.ai extensions:

```json
{
  "name": "Character",
  "description": "A character in the game world",
  "properties": {
    "name": {"type": "string", "required": true},
    "level": {"type": "integer", "minimum": 1},
    "faction": {"type": "string", "enum": ["empire", "rebels", "neutral"]}
  }
}
```

## Troubleshooting

### Common Issues

1. **File not found errors**
   - Check that paths exist and are readable
   - Verify environment variables are set correctly
   - Use absolute paths to avoid working directory issues

2. **Permission errors**
   - Ensure the application has read access to data files
   - Check directory permissions for schema folders

3. **Invalid JSON errors**
   - Validate JSON files with a JSON linter
   - Check that the format matches expected structure

### Debug Commands

```bash
# Check current configuration
cd backend
cargo run --example cli_demo -- --help

# Test with verbose logging
RUST_LOG=debug cargo run --example cli_demo

# Verify environment variables
echo "Schema: $UFORGE_SCHEMA_DIR"
echo "Data: $UFORGE_DATA_FILE"
```

## Best Practices

1. **Use absolute paths** in production to avoid working directory issues
2. **Set environment variables** in deployment scripts rather than hardcoding
3. **Validate paths exist** before starting the application
4. **Use consistent directory structures** across environments
5. **Document your path configuration** for team members
6. **Test path configuration** in all deployment environments
7. **Consider security** - don't expose sensitive paths in logs or error messages

## Examples

See the `examples/` directory for sample data and schema files that demonstrate the expected formats.
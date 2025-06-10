# u-forge.ai UI System Guide

> **A comprehensive guide to the 4-panel VSCode-inspired interface for the local-first TTRPG worldbuilding tool**

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Framework](https://img.shields.io/badge/UI-Tauri%20%2B%20Svelte-green.svg)
![Status](https://img.shields.io/badge/status-Ready%20for%20Development-brightgreen.svg)

## Overview

The u-forge.ai UI system provides a modern, responsive interface designed for TTRPG worldbuilding workflows. Built with **Tauri** (Rust backend) and **Svelte** (TypeScript frontend), it offers native performance with web-based flexibility.

### Key Features

- **🖥️ Client Side Decorations (CSD)** - Native window controls on Linux/GTK, adaptable to other platforms
- **📱 4-Panel Layout** - Inspired by VSCode/Zed for familiar developer experience
- **🌙 Dark Theme First** - Optimized for extended use with proper contrast ratios
- **⚡ High Performance** - Rust backend with reactive Svelte frontend
- **🔧 Configurable** - Resizable panels, customizable layouts, extensible architecture

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│ Custom Title Bar (CSD Support)                             │
├─────────────────────────────────────────────────────────────┤
│ Toolbar (New, Save, Search, View Controls)                 │
├──────────┬────────────────────────────┬────────────────────┤
│          │                            │                    │
│ Sidebar  │     Content Editor         │    AI Panel        │
│          │                            │                    │
│ (Tree    │  ┌─ Tab Bar ─────────────┐  │  (Conversations)   │
│  Nav)    │  │ [Tab1] [Tab2] [+]     │  │                    │
│          │  ├───────────────────────┤  │                    │
│          │  │                       │  │                    │
│          │  │   Object Editor       │  │                    │
│          │  │                       │  │                    │
│          │  └───────────────────────┘  │                    │
│          │  ┌───────────────────────┐  │                    │
│          │  │                       │  │                    │
│          │  │   Graph View          │  │                    │
│          │  │                       │  │                    │
│          │  └───────────────────────┘  │                    │
└──────────┴────────────────────────────┴────────────────────┤
│ Status Bar (Project Info, Save Status, System Stats)       │
└─────────────────────────────────────────────────────────────┘
```

## Panel Breakdown

### 1. Sidebar (Left Panel)
**Width**: 280px (resizable, collapsible)
**Purpose**: Navigation and project management

**Features**:
- **Explorer Tab**: Hierarchical object tree with grouping options
- **Search Tab**: Real-time semantic and exact search
- **Recent Tab**: Quick access to recent projects
- **Project Management**: New/Open project actions

**Component**: `src/lib/components/Sidebar.svelte`

### 2. Content Editor (Center Panel)
**Purpose**: Main editing interface with tabbed workflow

**Features**:
- **Tab Management**: Multiple objects open simultaneously
- **Object Editor**: Form-based editing with schema validation
- **Auto-save**: Periodic saving with dirty state tracking
- **Keyboard Shortcuts**: Standard editor shortcuts (Ctrl+S, Ctrl+N, etc.)

**Component**: `src/lib/components/ContentEditor.svelte`

### 3. Graph View (Bottom of Center)
**Height**: 300px (resizable, collapsible)
**Purpose**: Interactive knowledge graph visualization

**Features**:
- **D3.js Integration**: Force-directed graph layout
- **Interactive Controls**: Zoom, pan, node selection
- **Physics Simulation**: Configurable force parameters
- **Filtering**: By object type, relationships, tags
- **Legend & Settings**: Visual customization options

**Component**: `src/lib/components/GraphView.svelte`

### 4. AI Panel (Right Panel)
**Width**: 320px (resizable, collapsible)
**Purpose**: AI-assisted worldbuilding

**Features**:
- **Conversation Management**: Multiple AI chat sessions
- **Quick Actions**: Pre-defined prompts for common tasks
- **Context Awareness**: Integration with current project data
- **Provider Agnostic**: Support for local and cloud AI services

**Component**: `src/lib/components/AIPanel.svelte`

### 5. Title Bar & Status Bar
**Title Bar**: Custom CSD implementation with window controls
**Status Bar**: Project info, save status, system metrics

**Components**: 
- `src/lib/components/TitleBar.svelte`
- `src/lib/components/StatusBar.svelte`

## State Management

The UI uses **Svelte stores** for reactive state management:

### Core Stores

#### `projectStore.ts`
```typescript
// Project lifecycle and metadata
currentProject: ProjectInfo | null
settings: ProjectSettings | null
stats: ProjectStats | null
recentProjects: RecentProject[]
unsavedChanges: boolean
```

#### `uiStore.ts`
```typescript
// Interface state and layout
sidebarVisible: boolean
aiPanelVisible: boolean
graphPanelVisible: boolean
currentView: ViewType
editorTabs: EditorTab[]
panelLayout: PanelLayout
```

#### `graphStore.ts`
```typescript
// Graph visualization state
graphData: GraphData
selectedNodes: string[]
selectedEdges: string[]
layout: GraphLayout
filter: GraphFilter
simulation: SimulationState
```

### Store Architecture Benefits

- **Reactive Updates**: Automatic UI updates when data changes
- **Persistence**: Auto-save to localStorage with restoration
- **Type Safety**: Full TypeScript integration
- **Performance**: Efficient subscription model

## Styling System

### CSS Architecture

The UI uses a **CSS Custom Properties** based theming system:

```css
:root {
  /* Color Palette */
  --bg-primary: #1e1e1e;
  --bg-secondary: #252526;
  --bg-tertiary: #2d2d30;
  --text-primary: #cccccc;
  --text-secondary: #969696;
  --accent-color: #007acc;
  
  /* Layout */
  --titlebar-height: 30px;
  --sidebar-width: 280px;
  --header-height: 48px;
  
  /* Spacing Scale */
  --space-xs: 0.25rem;
  --space-sm: 0.5rem;
  --space-md: 1rem;
  --space-lg: 1.5rem;
}
```

### Component Styling Guidelines

1. **Use CSS Custom Properties** for all colors and dimensions
2. **Follow BEM-like naming** for CSS classes
3. **Mobile-first responsive design** with progressive enhancement
4. **Accessibility considerations** (contrast ratios, focus indicators)
5. **Consistent spacing** using the defined scale

## Client Side Decorations (CSD)

### Linux/GTK Implementation

The title bar is designed for CSD environments:

```typescript
// Tauri configuration for CSD
"decorations": false,
"titleBarStyle": "Overlay",
"hiddenTitle": true,
```

```css
.titlebar {
  -webkit-app-region: drag;
  /* Custom window controls */
}

.titlebar-controls {
  -webkit-app-region: no-drag;
  /* Minimize, maximize, close buttons */
}
```

### Platform Adaptations

- **Linux**: Full CSD with custom controls
- **macOS**: Traffic light integration
- **Windows**: Titlebar customization where supported

## Development Workflow

### Getting Started

1. **Prerequisites**:
   ```bash
   # Install Rust
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   
   # Install Node.js dependencies
   npm install
   ```

2. **Development Server**:
   ```bash
   # Start development server
   npm run tauri:dev
   
   # Or use the helper script
   ./dev.sh dev
   ```

3. **Building**:
   ```bash
   # Production build
   npm run tauri:build
   
   # Or use helper script
   ./dev.sh build
   ```

### File Structure

```
u-forge.ai/
├── src/                          # Frontend source
│   ├── lib/
│   │   ├── components/           # Svelte components
│   │   │   ├── Sidebar.svelte
│   │   │   ├── ContentEditor.svelte
│   │   │   ├── AIPanel.svelte
│   │   │   ├── GraphView.svelte
│   │   │   ├── TitleBar.svelte
│   │   │   └── StatusBar.svelte
│   │   ├── stores/              # State management
│   │   │   ├── projectStore.ts
│   │   │   ├── uiStore.ts
│   │   │   └── graphStore.ts
│   │   ├── types.ts             # TypeScript definitions
│   │   └── utils/               # Utility functions
│   ├── App.svelte               # Main application
│   ├── main.ts                  # Entry point
│   └── app.css                  # Global styles
├── src-tauri/                   # Rust backend
│   ├── src/
│   │   └── main.rs              # Tauri commands
│   ├── Cargo.toml               # Rust dependencies
│   └── tauri.conf.json          # Tauri configuration
├── package.json                 # Node.js dependencies
├── vite.config.ts              # Vite configuration
└── svelte.config.js            # Svelte configuration
```

### Component Development

Each component follows this structure:

```svelte
<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { someStore } from '../stores/someStore';
  import type { SomeType } from '../types';
  
  export let prop: SomeType;
  
  const dispatch = createEventDispatcher();
  
  // Component logic
</script>

<!-- Template -->
<div class="component-name">
  <!-- Content -->
</div>

<style>
  .component-name {
    /* Scoped styles */
  }
</style>
```

## Keyboard Shortcuts

### Global Shortcuts

| Shortcut | Action |
|----------|--------|
| `Ctrl+B` | Toggle Sidebar |
| `Ctrl+J` | Toggle AI Panel |
| `Ctrl+G` | Toggle Graph Panel |
| `Ctrl+N` | New Object |
| `Ctrl+S` | Save Current |
| `Ctrl+F` | Focus Search |
| `Ctrl+T` | New Tab |
| `Ctrl+W` | Close Tab |

### Panel-Specific Shortcuts

| Panel | Shortcut | Action |
|-------|----------|--------|
| Editor | `Ctrl+Shift+S` | Save All |
| Graph | `Ctrl+0` | Fit to View |
| Graph | `Ctrl+1` | Reset Zoom |
| AI | `Enter` | Send Message |
| AI | `Shift+Enter` | New Line |

## Accessibility Features

### Keyboard Navigation
- **Tab order**: Logical focus flow through UI elements
- **Focus indicators**: Clear visual focus states
- **Skip links**: Quick navigation for screen readers

### Visual Accessibility
- **High contrast mode**: Support for `prefers-contrast: high`
- **Reduced motion**: Respects `prefers-reduced-motion: reduce`
- **Color coding**: Never rely solely on color for information

### Screen Reader Support
- **ARIA labels**: Descriptive labels for interactive elements
- **Landmark roles**: Proper semantic structure
- **Live regions**: Dynamic content announcements

## Performance Considerations

### Frontend Optimization
- **Virtual scrolling**: For large object lists
- **Lazy loading**: Components loaded on demand
- **Debounced search**: Reduced API calls
- **Memoization**: Cached computed values

### Backend Integration
- **Command batching**: Reduce IPC overhead
- **Background processing**: Non-blocking operations
- **Memory management**: Efficient data structures

### Bundle Size
- **Tree shaking**: Remove unused code
- **Code splitting**: Lazy load features
- **Asset optimization**: Compressed images and fonts

## Testing Strategy

### Component Testing
```typescript
import { render } from '@testing-library/svelte';
import Sidebar from '../components/Sidebar.svelte';

test('sidebar renders project tree', () => {
  const { getByText } = render(Sidebar, {
    currentProject: mockProject
  });
  
  expect(getByText('Characters')).toBeInTheDocument();
});
```

### Integration Testing
- **Tauri commands**: Test Rust ↔ Frontend communication
- **Store interactions**: Verify state management
- **User workflows**: End-to-end scenarios

### E2E Testing
- **Playwright/Puppeteer**: Browser automation
- **Real user scenarios**: Complete workflows
- **Cross-platform**: Test on multiple OS

## Deployment

### Development Builds
```bash
# Debug build with hot reloading
npm run tauri:dev
```

### Production Builds
```bash
# Optimized build
npm run tauri:build

# Platform-specific bundles
npm run tauri:bundle
```

### Distribution
- **Linux**: AppImage, .deb, .rpm packages
- **macOS**: .dmg bundle with code signing
- **Windows**: .msi installer with signature

## Customization Guide

### Theming

Create custom themes by overriding CSS custom properties:

```css
/* themes/custom.css */
:root {
  --bg-primary: #0d1117;
  --bg-secondary: #161b22;
  --accent-color: #58a6ff;
  /* ... */
}
```

### Adding New Panels

1. **Create component**: `src/lib/components/NewPanel.svelte`
2. **Add to layout**: Update `App.svelte`
3. **Store integration**: Add state to `uiStore.ts`
4. **Keyboard shortcuts**: Register in event handlers

### Custom Object Types

1. **Define schema**: Add to schema system
2. **Update UI**: Object type selectors and icons
3. **Validation**: Property validation rules
4. **Templates**: Default property sets

## Troubleshooting

### Common Issues

#### "Failed to initialize KnowledgeGraph"
- Check Rust backend compilation
- Verify database path permissions
- Ensure FastEmbed model download

#### UI Not Responsive
- Check CSS custom properties
- Verify Svelte store subscriptions
- Debug component reactivity

#### Graph Not Rendering
- Check D3.js integration
- Verify SVG element creation
- Debug simulation parameters

### Debug Tools

- **Svelte DevTools**: Component inspection
- **Tauri DevTools**: Backend debugging
- **Browser DevTools**: Network and performance

### Performance Profiling

```bash
# Frontend profiling
npm run dev -- --mode=profile

# Backend profiling
cargo build --release --features=profiling
```

## Contributing Guidelines

### Code Style
- **Prettier**: Automatic formatting
- **ESLint**: JavaScript/TypeScript linting
- **Clippy**: Rust linting
- **Type checking**: Strict TypeScript

### Pull Request Process
1. **Feature branch**: Create from main
2. **Tests**: Add/update tests for changes
3. **Documentation**: Update relevant docs
4. **Review**: Code review before merge

### Component Guidelines
- **Single responsibility**: One purpose per component
- **Props validation**: TypeScript interfaces
- **Event driven**: Use dispatchers for communication
- **Accessible**: Follow a11y guidelines

## Future Enhancements

### Planned Features
- **Plugin system**: Third-party extensions
- **Advanced theming**: Visual theme editor
- **Collaboration**: Real-time multi-user editing
- **Mobile app**: React Native or Flutter version

### Technical Debt
- **D3.js integration**: Full implementation vs. mock
- **Test coverage**: Increase to >90%
- **Documentation**: Component storybook
- **Performance**: Advanced optimization

## Resources

### Documentation
- [Tauri Documentation](https://tauri.app/v1/guides/)
- [Svelte Documentation](https://svelte.dev/docs)
- [TypeScript Handbook](https://www.typescriptlang.org/docs/)

### Design System
- [Material Design](https://material.io/design) - Color and spacing principles
- [Human Interface Guidelines](https://developer.apple.com/design/human-interface-guidelines/) - macOS integration
- [GNOME HIG](https://developer.gnome.org/hig/) - Linux/GTK guidelines

### Communities
- [Tauri Discord](https://discord.com/invite/tauri)
- [Svelte Discord](https://discord.com/invite/yy75DKs)
- [r/gamedev](https://reddit.com/r/gamedev) - TTRPG community

---

**Ready to build worlds? Start with `./dev.sh setup` and dive into the codebase!** 🚀

*This UI system provides the foundation for an exceptional worldbuilding experience, combining modern web technologies with native performance.*
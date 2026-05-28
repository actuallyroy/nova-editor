# PRD — Nova Editor

## GPU-Native VSCode-Compatible Code Editor

---

# 1. Vision

Nova is a next-generation GPU-native code editor designed to deliver:

* VSCode-compatible workflows
* dramatically lower memory usage
* lower CPU overhead
* near-instant UI responsiveness
* scalable multi-window performance
* modern rendering architecture

Nova should feel visually and behaviorally identical to VSCode while replacing Electron’s heavyweight browser rendering stack with a custom GPU-native UI architecture.

Core philosophy:

> “VSCode UX and ecosystem without Electron overhead.”

---

# 2. Product Goals

---

## Primary Goals

### User Experience

* Exact VSCode-like interface
* Familiar workflows
* Minimal migration friction
* Familiar keyboard shortcuts
* Familiar settings behavior
* Familiar extension workflows

---

## Performance

* GPU-first rendering
* Low idle CPU
* Low memory footprint
* Efficient multi-window scaling
* Shared infrastructure architecture

---

## Compatibility

* Partial VSCode extension compatibility
* Support top 10–20% most-used extensions
* Support major language tooling
* Support VSCode themes and settings

---

# 3. Strategic Positioning

Nova is NOT:

* a VSCode fork
* an Electron clone
* a browser application

Nova IS:

* a native GPU editor platform
* a VSCode workflow-compatible editor
* an architecture-first performance-focused editor

---

# 4. Core Product Principles

---

## 4.1 Interface Compatibility

The interface should closely mirror VSCode:

* activity bar
* sidebars
* explorer
* tabs
* command palette
* terminal layout
* settings UI
* keyboard shortcuts
* themes

Goal:

> Existing VSCode users should feel immediately comfortable.

---

## 4.2 Architecture Over Frameworks

Performance should come primarily from:

* shared infrastructure
* GPU rendering
* minimal process duplication
* lightweight runtime architecture

NOT from:

* micro-optimizations
* aggressive caching hacks

---

## 4.3 GPU-First Rendering

Avoid:

* HTML rendering
* DOM trees
* CSS layout engines
* browser rendering pipelines

Use:

* native GPU rendering
* batched rendering
* dirty-region updates
* glyph atlases

---

# 5. High-Level Architecture

```text id="y8h6go"
+------------------------------------------------------+
|                    NOVA DAEMON                       |
|------------------------------------------------------|
| Workspace Manager                                    |
| File Indexer                                         |
| Git Services                                         |
| Extension Runtime Manager                            |
| VSCode Compatibility Layer                           |
| Shared Cache                                         |
| AI Services                                          |
| Shared Worker Pools                                  |
+------------------------------------------------------+

                    ↑ IPC ↓

+------------------------------------------------------+
|               WINDOW RENDERERS                       |
|------------------------------------------------------|
| GPU Renderer                                         |
| Text Rendering                                       |
| Editor Viewports                                     |
| Layout Engine                                        |
| Input System                                         |
| Terminal Surface                                     |
| Native UI Components                                 |
+------------------------------------------------------+
```

---

# 6. Technology Stack

---

## Core Language

### Rust

Reason:

* memory safety
* concurrency
* low-level control
* async ecosystem
* performance

---

## Rendering

### WGPU

wgpu

Backend support:

* Vulkan
* Metal
* DX12

---

## Windowing

### Winit

winit

---

## Text Engine

### cosmic-text

cosmic-text

Requirements:

* ligatures
* Unicode
* IME
* bidi text
* incremental layout

---

## Text Storage

### Ropey

Ropey

---

## Parsing

### Tree-sitter

Tree-sitter

---

## Async Runtime

### Tokio

Tokio

---

## JS Runtime

### QuickJS

QuickJS

Purpose:

* lightweight extension sandbox
* capability-based execution
* low-memory runtime

---

# 7. UI/UX Requirements

---

# 7.1 VSCode Interface Parity

The following must visually and behaviorally match VSCode:

---

## Layout

* activity bar
* sidebar
* panel system
* editor tabs
* status bar
* command palette
* quick open
* split editors
* terminal panel

---

## Interactions

* keyboard shortcuts
* drag/drop tabs
* tab pinning
* command palette behavior
* explorer interactions
* settings search
* minimap
* breadcrumbs

---

## Visual Compatibility

Support:

* VSCode themes
* icon packs
* syntax themes
* font settings

---

# 7.2 Rendering Requirements

Mandatory:

* smooth scrolling
* high refresh rendering
* dirty-region rendering
* GPU batching
* low-latency input

Target:

* 120 FPS scrolling

---

# 8. Extension Architecture

---

# 8.1 Philosophy

Nova will NOT attempt full VSCode binary compatibility.

Instead:

* selectively support high-value extension capabilities
* translate/adapt extension behavior
* progressively reduce compatibility overhead

---

# 8.2 Supported Extension Classes

---

## Tier 1 (MVP)

High priority support:

* themes
* icon packs
* syntax extensions
* snippets
* formatter extensions
* linter integrations
* language support
* autocomplete providers
* diagnostics
* command providers

Examples:

* Prettier
* ESLint
* Tailwind IntelliSense
* Rust Analyzer
* Python
* Go
* Docker

---

## Tier 2

Partial support:

* tree views
* custom sidebars
* git enhancements
* AI integrations

---

## Unsupported Initially

Avoid:

* webview-heavy extensions
* notebook APIs
* remote containers
* browser-embedded dashboards
* heavy Electron assumptions

---

# 8.3 Extension Processing Pipeline

```text id="1h2m4m"
VSCode Extension
        ↓
Manifest Analyzer
        ↓
Capability Classifier
        ↓
API Adapter Layer
        ↓
QuickJS Sandbox
        ↓
Native Nova APIs
```

---

# 8.4 Compatibility Model

Nova should:

* analyze extension APIs used
* classify compatibility level
* adapt supported APIs
* reject unsupported APIs gracefully

Goal:

> Support the most useful extension ecosystem subset efficiently.

---

# 9. Shared Infrastructure Architecture

---

# 9.1 Shared Daemon

All heavy systems run centrally:

* indexing
* syntax parsing
* extension workers
* file watching
* git operations
* AI processing
* caches

Windows remain lightweight renderer clients.

---

# 9.2 Shared Worker Pools

Avoid:

* extension host per window
* duplicated parsers
* duplicated caches

Instead:

* shared workers
* shared LSP instances
* shared syntax trees

---

# 10. Rendering Architecture

---

# 10.1 GPU Rendering Pipeline

```text id="tfujns"
Text Buffer
    ↓
Incremental Parser
    ↓
Layout Engine
    ↓
Glyph Atlas
    ↓
GPU Batch Renderer
    ↓
Frame Output
```

---

# 10.2 Rendering Rules

Avoid:

* full-screen redraws
* layout thrashing
* immediate-mode redraw loops

Use:

* retained rendering
* dirty-region updates
* batched draw calls

---

# 11. Performance Targets

| Metric                  | Target      |
| ----------------------- | ----------- |
| Idle CPU per window     | <0.2%       |
| Idle RAM per window     | <20MB       |
| Cold startup            | <150ms      |
| Window open time        | <50ms       |
| Smooth scrolling        | Mandatory   |
| 50 simultaneous windows | Design goal |

---

# 12. Memory Strategy

---

## Shared Memory

Shared globally:

* font atlases
* syntax trees
* extension workers
* git state
* file caches

Per-window:

* viewport state
* cursors
* UI state

---

# 13. AI Integration

Architecture should support:

* inline completion
* chat panels
* code actions
* semantic indexing

AI systems must remain isolated from renderer processes.

---

# 14. MVP Scope

---

# Phase 1 — Rendering Core

Build:

* windowing
* GPU renderer
* text rendering
* scrolling
* cursor system
* selection
* tabs
* panels

Goal:

* VSCode-like UI shell

---

# Phase 2 — Editor Engine

Build:

* Rope editor
* Tree-sitter integration
* syntax highlighting
* minimap
* command palette
* split panes

---

# Phase 3 — Workspace Features

Build:

* explorer
* terminal
* git integration
* settings
* theme system

---

# Phase 4 — Extension Runtime

Build:

* QuickJS sandbox
* manifest analyzer
* compatibility layer
* extension API adapters

---

# Phase 5 — Ecosystem Compatibility

Support:

* popular extensions
* language tooling
* formatter ecosystem
* AI plugins

---

# 15. Engineering Priorities

Priority order:

1. Rendering smoothness
2. Input responsiveness
3. Memory efficiency
4. CPU efficiency
5. Extension compatibility
6. Feature completeness

Never sacrifice:

* responsiveness
* frame pacing
* low idle overhead

for:

* rapid feature expansion

---

# 16. Non-Goals (Initial Versions)

Avoid initially:

* browser embedding
* full webview support
* full Node.js runtime
* remote container support
* notebook systems
* browser-based UI rendering

---

# 17. Success Criteria

Nova is successful when:

* VSCode users can switch with minimal friction
* common workflows function smoothly
* top extensions work reliably
* memory usage is drastically lower than VSCode
* UI responsiveness exceeds Electron editors
* many windows remain responsive simultaneously

---

# 18. Suggested Repository Structure

```text id="jlwmn6"
nova/
├── daemon/
├── renderer/
├── editor-core/
├── text-engine/
├── extension-runtime/
├── vscode-compat/
├── workspace/
├── terminal/
├── protocol/
├── ui/
├── plugins/
└── benchmarks/
```

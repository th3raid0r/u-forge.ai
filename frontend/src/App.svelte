<script lang="ts">
    import { onMount } from "svelte";
    import { invoke } from "@tauri-apps/api/tauri";
    import { appWindow } from "@tauri-apps/api/window";

    // Import components
    import TitleBar from "./lib/components/TitleBar.svelte";
    import Sidebar from "./lib/components/Sidebar.svelte";
    import ContentEditor from "./lib/components/ContentEditor.svelte";
    import GraphView from "./lib/components/GraphView.svelte";
    import AIPanel from "./lib/components/AIPanel.svelte";
    import StatusBar from "./lib/components/StatusBar.svelte";

    // Import stores
    import { uiStore } from "./lib/stores/uiStore";
    import { graphStore } from "./lib/stores/graphStore";
    import { initializeSchemaStore } from "./lib/stores/schemaStore";
    import { projectStore } from "./lib/stores/projectStore";

    // Import types
    import type { ProjectInfo, ApiResponse, DatabaseStatus } from "./lib/types";

    let isLoading = true;
    let error: string | null = null;
    let currentProject: ProjectInfo | null = null;

    // Panel visibility toggles
    let showSidebar = true;
    let showAIPanel = true;
    let showGraphPanel = true;

    // Panel sizes (for resizing)
    let sidebarWidth = 280;
    let aiPanelWidth = 320;
    let graphPanelHeight = 300;

    // Resize handling
    let isResizing = false;
    let resizeType = "";

    onMount(async () => {
        try {
            // Initialize the application
            console.log("🚀 [App] Starting u-forge.ai initialization...");

            // Set up window controls for Client Side Decorations
            console.log("🪟 [App] Setting up window controls...");
            await setupWindowControls();
            console.log("✅ [App] Window controls ready");

            // Try to restore existing project connection first (completely optional)
            try {
                console.log(
                    "🔄 [App] Attempting to restore project connection...",
                );
                const restoreResponse = (await invoke(
                    "restore_project_connection",
                )) as ApiResponse<string>;
                if (restoreResponse.success) {
                    console.log(
                        "✅ [App] Project connection restored:",
                        restoreResponse.data,
                    );
                    // Get the current project info after restoration
                    try {
                        const statsResponse = (await invoke(
                            "get_project_stats",
                        )) as ApiResponse<ProjectInfo>;
                        if (statsResponse.success && statsResponse.data) {
                            currentProject = statsResponse.data;
                            projectStore.setProject(currentProject);
                            console.log(
                                "✅ [App] Project info loaded after restoration",
                            );

                            // Save to localStorage for future refreshes
                            localStorage.setItem(
                                "currentProject",
                                JSON.stringify(currentProject),
                            );
                        }
                    } catch (statsError) {
                        console.warn(
                            "⚠️ [App] Failed to get project stats after restoration:",
                            statsError,
                        );
                    }
                } else {
                    console.log(
                        "ℹ️ [App] No existing project to restore:",
                        restoreResponse.error,
                    );
                }
            } catch (restoreError) {
                console.warn(
                    "⚠️ [App] Project restoration failed (non-critical):",
                    restoreError,
                );
                // Continue with normal initialization
            }

            // Check if there's a stored current project from previous session
            // if (!currentProject) {
            //     const storedProject = localStorage.getItem("currentProject");
            //     if (storedProject) {
            //         try {
            //             const projectInfo = JSON.parse(storedProject);
            //             currentProject = projectInfo;
            //             if (currentProject) {
            //                 projectStore.setProject(currentProject);
            //                 console.log(
            //                     "✅ [App] Restored project from localStorage",
            //                 );
            //             }
            //         } catch (parseError) {
            //             console.error(
            //                 "❌ [App] Failed to parse stored project:",
            //                 parseError,
            //             );
            //             localStorage.removeItem("currentProject");
            //         }
            //     }
            // }

            // Check if there's a recent project to load (fallback)
            const recentProject = localStorage.getItem("recentProject");
            //if (recentProject && !currentProject) {
            if (recentProject) {
                try {
                    const projectInfo = JSON.parse(recentProject);
                    await loadProject(projectInfo.name, projectInfo.path);
                    console.log("✅ [App] Recent project loaded successfully");
                } catch (projectError) {
                    console.error(
                        "❌ [App] Failed to load recent project:",
                        projectError,
                    );
                    // Clear invalid recent project
                    localStorage.removeItem("recentProject");
                }
            }

            // Initialize default project if no project loaded
            if (!currentProject) {
                console.log("📦 [App] Initializing default project...");
                await initializeDefaultProject();
                console.log("✅ [App] Default project initialized");
            }

            // Initialize schema store first (required for everything else)
            console.log("📋 [App] Initializing schema store...");
            await initializeSchemaStore();
            console.log("✅ [App] Schema store initialized");

            isLoading = false;
            console.log("🎉 [App] u-forge.ai initialization complete!");
        } catch (initError) {
            console.error("❌ [App] Failed to initialize:", initError);
            error = `Failed to initialize u-forge.ai: ${initError}`;
            isLoading = false;
        }
    });

    // Set up window controls for Client Side Decorations
    async function setupWindowControls() {
        try {
            // Configure window properties
            await appWindow.setDecorations(false);
            await appWindow.setResizable(true);
            await appWindow.setMinimizable(true);
            await appWindow.setMaximizable(true);

            // Set minimum window size
            await appWindow.setMinSize(
                new (await import("@tauri-apps/api/window")).LogicalSize(
                    800,
                    600,
                ),
            );

            console.log("✅ [setupWindowControls] Window controls configured");
        } catch (error) {
            console.error(
                "❌ [setupWindowControls] Failed to configure window:",
                error,
            );
            throw error;
        }
    }

    async function loadProject(name: string, path?: string) {
        try {
            console.log(`📂 [loadProject] Loading project: ${name}`);

            const response = (await invoke("load_project", {
                name,
                path: path || null,
            })) as ApiResponse<ProjectInfo>;

            if (response.success && response.data) {
                currentProject = response.data;
                projectStore.setProject(currentProject);

                // Save to recent projects
                localStorage.setItem(
                    "recentProject",
                    JSON.stringify({ name, path }),
                );

                // Store current project for refresh persistence
                localStorage.setItem(
                    "currentProject",
                    JSON.stringify(currentProject),
                );

                console.log(
                    "✅ [loadProject] Project loaded successfully:",
                    currentProject,
                );

                // Refresh graph data after project load
                await refreshGraphData();
            } else {
                throw new Error(response.error || "Failed to load project");
            }
        } catch (loadError) {
            console.error(
                "❌ [loadProject] Failed to load project:",
                loadError,
            );
            error = `Failed to load project: ${loadError}`;
            throw loadError;
        }
    }

    async function refreshGraphData() {
        try {
            console.log("🔄 [refreshGraphData] Refreshing graph data...");

            const response = (await invoke(
                "get_graph_data",
            )) as ApiResponse<any>;

            if (response.success && response.data) {
                graphStore.setGraphData(response.data);
                console.log("✅ [refreshGraphData] Graph data refreshed");
            } else {
                console.error(
                    "❌ [refreshGraphData] Failed to get graph data:",
                    response.error,
                );
            }
        } catch (refreshError) {
            console.error(
                "❌ [refreshGraphData] Error refreshing graph data:",
                refreshError,
            );
        }
    }

    async function initializeDefaultProject() {
        try {
            console.log(
                "📦 [initializeDefaultProject] Setting up default project...",
            );

            // Initialize a default project
            const initResponse = (await invoke("initialize_project", {
                projectName: "Default Project",
                projectPath: "./default-project",
            })) as ApiResponse<ProjectInfo>;

            if (!initResponse.success || !initResponse.data) {
                throw new Error(
                    initResponse.error ||
                        "Failed to initialize default project",
                );
            }

            currentProject = initResponse.data;
            projectStore.setProject(currentProject);

            // Store current project for refresh persistence
            localStorage.setItem(
                "currentProject",
                JSON.stringify(currentProject),
            );

            console.log(
                "✅ [initializeDefaultProject] Default project initialized",
            );

            // Check if database already has data before importing
            console.log(
                "🔍 [initializeDefaultProject] Checking database status...",
            );
            const statusResponse = (await invoke(
                "check_database_status",
            )) as ApiResponse<DatabaseStatus>;

            if (statusResponse.success && statusResponse.data) {
                const status = statusResponse.data;
                console.log(
                    `📊 [initializeDefaultProject] Database status - Objects: ${status.object_count}, Relationships: ${status.relationship_count}, Schemas: ${status.schema_count}`,
                );

                if (status.has_data && status.has_schemas) {
                    console.log(
                        "✅ [initializeDefaultProject] Database already contains data and schemas, skipping import",
                    );
                    // Still refresh to show existing data
                    await refreshGraphData();
                    await projectStore.refreshStats();
                    return;
                }
            }

            // Import default data into the project
            console.log(
                "📦 [initializeDefaultProject] Importing default data...",
            );
            const importResponse = (await invoke(
                "import_default_data",
            )) as ApiResponse<string>;

            if (!importResponse.success) {
                throw new Error(
                    importResponse.error || "Failed to import default data",
                );
            }

            console.log(
                "✅ [initializeDefaultProject] Default data imported:",
                importResponse.data,
            );

            // Refresh graph data and project stats after import
            await refreshGraphData();
            await projectStore.refreshStats();
        } catch (initError) {
            console.error(
                "❌ [initializeDefaultProject] Failed to initialize default project:",
                initError,
            );
            throw initError;
        }
    }

    function toggleSidebar() {
        showSidebar = !showSidebar;
        uiStore.setSidebarVisible(showSidebar);
    }

    function toggleAIPanel() {
        showAIPanel = !showAIPanel;
        uiStore.setAIPanelVisible(showAIPanel);
    }

    function toggleGraphPanel() {
        showGraphPanel = !showGraphPanel;
        uiStore.setGraphPanelVisible(showGraphPanel);
    }

    function startResize(event: MouseEvent, type: string) {
        isResizing = true;
        resizeType = type;
        event.preventDefault();

        document.addEventListener("mousemove", handleResize);
        document.addEventListener("mouseup", stopResize);
    }

    function handleResize(event: MouseEvent) {
        if (!isResizing) return;

        switch (resizeType) {
            case "sidebar":
                sidebarWidth = Math.max(200, Math.min(500, event.clientX));
                break;
            case "ai":
                aiPanelWidth = Math.max(
                    250,
                    Math.min(600, window.innerWidth - event.clientX),
                );
                break;
            case "graph":
                graphPanelHeight = Math.max(
                    200,
                    Math.min(600, window.innerHeight - event.clientY),
                );
                break;
        }
    }

    function stopResize() {
        isResizing = false;
        resizeType = "";
        document.removeEventListener("mousemove", handleResize);
        document.removeEventListener("mouseup", stopResize);
    }

    function createNewObject() {
        // Dispatch custom event to ContentEditor for navigation protection
        const event = new CustomEvent("requestNewObject");
        document.dispatchEvent(event);
    }

    function handleKeydown(event: KeyboardEvent) {
        const isMac = navigator.platform.toUpperCase().indexOf("MAC") >= 0;
        const cmdOrCtrl = isMac ? event.metaKey : event.ctrlKey;

        if (cmdOrCtrl) {
            switch (event.key) {
                case "b":
                    event.preventDefault();
                    toggleSidebar();
                    break;
                case "j":
                    event.preventDefault();
                    toggleAIPanel();
                    break;
                case "g":
                    event.preventDefault();
                    toggleGraphPanel();
                    break;
                case "n":
                    event.preventDefault();
                    createNewObject();
                    break;
                case "s":
                    event.preventDefault();
                    // TODO: Save
                    break;
                case "f":
                    event.preventDefault();
                    // TODO: Search
                    break;
            }
        }
    }
</script>

<svelte:window on:keydown={handleKeydown} />

<div class="app-container">
    <!-- Custom Title Bar for Client Side Decorations -->
    <TitleBar />

    {#if isLoading}
        <div class="loading-screen">
            <div class="loading-spinner"></div>
            <p>Initializing u-forge.ai...</p>
        </div>
    {:else if error}
        <div class="error-screen">
            <div class="error-icon">⚠️</div>
            <h2>Initialization Error</h2>
            <p>{error}</p>
            <button
                class="btn primary"
                on:click={() => window.location.reload()}
            >
                Retry
            </button>
        </div>
    {:else}
        <!-- Main Application Layout -->
        <div class="app-content">
            <!-- Left Sidebar - Navigation Tree -->
            {#if showSidebar}
                <div
                    class="sidebar"
                    style="width: {sidebarWidth}px"
                    class:collapsed={sidebarWidth < 100}
                >
                    <Sidebar
                        {currentProject}
                        on:toggleCollapse={toggleSidebar}
                        on:projectLoad={(e) =>
                            loadProject(e.detail.name, e.detail.path)}
                        on:objectSelected={(e) =>
                            uiStore.setSelectedObject(e.detail.objectId)}
                        on:createNewObject={createNewObject}
                    />
                </div>

                <!-- Sidebar Resize Handle -->
                <div
                    class="resize-handle vertical"
                    role="separator"
                    on:mousedown={(e) => startResize(e, "sidebar")}
                ></div>
            {/if}

            <!-- Center Content Area -->
            <div class="main-content">
                <!-- Content Header with toolbar -->
                <div class="content-header">
                    <div class="toolbar">
                        <button
                            class="btn icon-only"
                            title="Toggle Sidebar (Ctrl+B)"
                            on:click={toggleSidebar}
                        >
                            📁
                        </button>

                        <div class="separator"></div>

                        <button
                            class="btn icon-only"
                            title="New Object (Ctrl+N)"
                            on:click={createNewObject}
                        >
                            ➕
                        </button>

                        <button class="btn icon-only" title="Save (Ctrl+S)">
                            💾
                        </button>

                        <div class="separator"></div>

                        <input
                            type="search"
                            placeholder="Search knowledge graph..."
                            class="search-input"
                        />
                    </div>

                    <div class="view-controls">
                        <button
                            class="btn icon-only"
                            title="Toggle AI Panel (Ctrl+J)"
                            class:active={showAIPanel}
                            on:click={toggleAIPanel}
                        >
                            🤖
                        </button>

                        <button
                            class="btn icon-only"
                            title="Toggle Graph View (Ctrl+G)"
                            class:active={showGraphPanel}
                            on:click={toggleGraphPanel}
                        >
                            🕸️
                        </button>
                    </div>
                </div>

                <!-- Content Body -->
                <div class="content-body">
                    <!-- Main Editor Area -->
                    <div class="editor-area">
                        <ContentEditor {currentProject} />

                        <!-- Graph Panel (Bottom) -->
                        {#if showGraphPanel}
                            <div
                                class="resize-handle horizontal"
                                role="separator"
                                on:mousedown={(e) => startResize(e, "graph")}
                            ></div>

                            <div
                                class="graph-panel"
                                style="height: {graphPanelHeight}px"
                            >
                                <GraphView on:refreshData={refreshGraphData} />
                            </div>
                        {/if}
                    </div>

                    <!-- Right AI Panel -->
                    {#if showAIPanel}
                        <div
                            class="resize-handle vertical"
                            role="separator"
                            on:mousedown={(e) => startResize(e, "ai")}
                        ></div>

                        <div class="ai-panel" style="width: {aiPanelWidth}px">
                            <AIPanel {currentProject} />
                        </div>
                    {/if}
                </div>
            </div>
        </div>

        <!-- Status Bar -->
        <StatusBar {currentProject} />
    {/if}
</div>

<style>
    .app-container {
        display: flex;
        flex-direction: column;
        height: 100vh;
        background: var(--bg-primary);
        color: var(--text-primary);
    }

    .loading-screen,
    .error-screen {
        display: flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        height: 100vh;
        gap: var(--space-lg);
    }

    .loading-spinner {
        width: 40px;
        height: 40px;
        border: 3px solid var(--border-color);
        border-top: 3px solid var(--accent-color);
        border-radius: 50%;
        animation: spin 1s linear infinite;
    }

    @keyframes spin {
        0% {
            transform: rotate(0deg);
        }
        100% {
            transform: rotate(360deg);
        }
    }

    .error-screen {
        text-align: center;
    }

    .error-icon {
        font-size: 3rem;
    }

    .app-content {
        display: flex;
        flex: 1;
        overflow: hidden;
    }

    .sidebar {
        background: var(--bg-secondary);
        border-right: 1px solid var(--border-color);
        display: flex;
        flex-direction: column;
        min-width: 200px;
        max-width: 500px;
        transition: width var(--transition-normal);
    }

    .main-content {
        flex: 1;
        display: flex;
        flex-direction: column;
        overflow: hidden;
        min-width: 0; /* Allows flex item to shrink */
    }

    .content-header {
        height: var(--header-height);
        background: var(--bg-secondary);
        border-bottom: 1px solid var(--border-color);
        display: flex;
        align-items: center;
        justify-content: space-between;
        padding: 0 var(--space-md);
        gap: var(--space-md);
    }

    .toolbar {
        display: flex;
        align-items: center;
        gap: var(--space-sm);
    }

    .separator {
        width: 1px;
        height: 20px;
        background: var(--border-color);
        margin: 0 var(--space-sm);
    }

    .search-input {
        background: var(--bg-tertiary);
        border: 1px solid var(--border-color);
        border-radius: var(--radius-sm);
        color: var(--text-primary);
        padding: var(--space-sm) var(--space-md);
        font-size: var(--font-sm);
        width: 300px;
        transition: border-color var(--transition-fast);
    }

    .search-input:focus {
        outline: none;
        border-color: var(--accent-color);
    }

    .view-controls {
        display: flex;
        gap: var(--space-sm);
    }

    .btn.active {
        background: var(--accent-color);
        border-color: var(--accent-color);
        color: white;
    }

    .content-body {
        flex: 1;
        display: flex;
        overflow: hidden;
    }

    .editor-area {
        flex: 1;
        display: flex;
        flex-direction: column;
        overflow: hidden;
        background: var(--bg-primary);
    }

    .ai-panel {
        background: var(--bg-secondary);
        border-left: 1px solid var(--border-color);
        display: flex;
        flex-direction: column;
        min-width: 250px;
        max-width: 600px;
    }

    .graph-panel {
        background: var(--bg-tertiary);
        border-top: 1px solid var(--border-color);
        min-height: 150px;
        max-height: 50vh;
    }

    .resize-handle {
        background: transparent;
        transition: background-color var(--transition-fast);
    }

    .resize-handle:hover {
        background: var(--accent-color);
    }

    .resize-handle.vertical {
        width: 4px;
        cursor: ew-resize;
    }

    .resize-handle.horizontal {
        height: 4px;
        cursor: ns-resize;
    }

    /* Responsive design */
    @media (max-width: 1024px) {
        .search-input {
            width: 200px;
        }
    }

    @media (max-width: 768px) {
        .sidebar {
            position: absolute;
            left: 0;
            top: var(--titlebar-height, 30px);
            bottom: 0;
            z-index: 100;
            box-shadow: 2px 0 8px rgba(0, 0, 0, 0.2);
            transform: translateX(-100%);
            transition: transform var(--transition-normal);
        }

        .sidebar.show {
            transform: translateX(0);
        }

        .ai-panel {
            display: none; /* Hide on mobile */
        }

        .search-input {
            width: 150px;
        }
    }
</style>

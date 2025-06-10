<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/tauri';
  import { appWindow } from '@tauri-apps/api/window';
  
  // Import components
  import Sidebar from './lib/components/Sidebar.svelte';
  import ContentEditor from './lib/components/ContentEditor.svelte';
  import AIPanel from './lib/components/AIPanel.svelte';
  import GraphView from './lib/components/GraphView.svelte';
  import TitleBar from './lib/components/TitleBar.svelte';
  import StatusBar from './lib/components/StatusBar.svelte';
  
  // Import stores
  import { projectStore } from './lib/stores/projectStore';
  import { uiStore } from './lib/stores/uiStore';
  import { graphStore } from './lib/stores/graphStore';
  import { initializeSchemaStore } from './lib/stores/schemaStore';
  
  // Import types
  import type { ProjectInfo, ApiResponse } from './lib/types';
  
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
  
  onMount(async () => {
    try {
      // Initialize the application
      console.log('🚀 [App] Starting u-forge.ai initialization...');
      
      // Set up window controls for Client Side Decorations
      console.log('🪟 [App] Setting up window controls...');
      await setupWindowControls();
      console.log('✅ [App] Window controls ready');
      
      // Check if there's a recent project to load
      const recentProject = localStorage.getItem('recentProject');
      if (recentProject) {
        console.log('📂 [App] Found recent project, loading...');
        const projectData = JSON.parse(recentProject);
        console.log(`📂 [App] Recent project data:`, projectData);
        await loadProject(projectData.name, projectData.path);
      } else {
        // Initialize a default project if none exists
        console.log('📂 [App] No recent project found, creating default...');
        const defaultProjectPath = './default-project';
        await loadProject('Default Project', defaultProjectPath);
        
        // Import sample data after project initialization
        console.log('📊 [App] Importing sample data...');
        await importSampleData();
      }
      
      // Initialize schema store after project is loaded (non-blocking)
      console.log('📋 [App] Starting schema store initialization...');
      initializeSchemaStore().then(() => {
        console.log('✅ [App] Schema store initialization completed');
      }).catch(err => {
        console.warn('⚠️ [App] Schema initialization failed, but continuing app startup:', err);
      });
      
      console.log('✅ [App] Application initialization complete');
      isLoading = false;
    } catch (err) {
      console.error('💥 [App] Failed to initialize application:', err);
      error = err instanceof Error ? err.message : 'Unknown error occurred';
      isLoading = false;
    }
  });
  
  async function setupWindowControls() {
    // Set up window control event listeners for CSD
    const titlebarButtons = document.querySelectorAll('.titlebar-button');
    
    titlebarButtons.forEach(button => {
      button.addEventListener('click', async (e) => {
        const action = (e.target as HTMLElement).dataset.action;
        
        switch (action) {
          case 'close':
            await appWindow.close();
            break;
          case 'minimize':
            await appWindow.minimize();
            break;
          case 'maximize':
            await appWindow.toggleMaximize();
            break;
        }
      });
    });
  }
  
  async function loadProject(name: string, path: string) {
    try {
      console.log(`📁 [App] Loading project: ${name} at ${path}`);
      const response = await invoke('initialize_project', { 
        projectName: name, 
        projectPath: path 
      }) as ApiResponse<ProjectInfo>;
      
      console.log(`📁 [App] initialize_project response:`, response);
      
      if (response.success && response.data) {
        currentProject = response.data;
        projectStore.setProject(response.data);
        console.log(`✅ [App] Project loaded successfully: ${response.data.name}`);
        console.log(`   - Objects: ${response.data.object_count}`);
        console.log(`   - Relationships: ${response.data.relationship_count}`);
        
        // Save to recent projects
        localStorage.setItem('recentProject', JSON.stringify({ name, path }));
        console.log(`💾 [App] Project saved to recent projects`);
        
        // Load initial graph data
        console.log(`🕸️ [App] Loading initial graph data...`);
        await refreshGraphData();
        
        // If this is a new project, try to import sample data
        if (response.data.object_count === 0) {
          console.log(`📊 [App] Empty project detected, importing sample data...`);
          await importSampleData();
        }
      } else {
        throw new Error(response.error || 'Failed to load project');
      }
    } catch (err) {
      console.error('❌ [App] Failed to load project:', err);
      error = err instanceof Error ? err.message : 'Failed to load project';
    }
  }
  
  async function refreshGraphData() {
    try {
      console.log(`🕸️ [App] Refreshing graph data...`);
      const response = await invoke('get_graph_data', { limit: 100 }) as ApiResponse<any>;
      console.log(`🕸️ [App] get_graph_data response:`, response);
      if (response.success && response.data) {
        graphStore.setGraphData(response.data);
        console.log(`✅ [App] Graph data refreshed successfully`);
      } else {
        console.warn(`⚠️ [App] Graph data refresh failed:`, response.error);
      }
    } catch (err) {
      console.error('❌ [App] Failed to refresh graph data:', err);
    }
  }
  
  async function importSampleData() {
    try {
      console.log('📊 [App] Importing sample data...');
      const response = await invoke('import_sample_data', { 
        dataFilePath: './examples/data/memory.json' 
      }) as ApiResponse<any>;
      
      console.log('📊 [App] import_sample_data response:', response);
      
      if (response.success && response.data) {
        console.log('✅ [App] Sample data imported successfully:', response.data);
        // Refresh the graph data and project stats after import
        console.log('🔄 [App] Refreshing data after sample import...');
        await refreshGraphData();
        
        // Update project stats
        console.log('📈 [App] Updating project stats...');
        const statsResponse = await invoke('get_project_stats') as ApiResponse<ProjectInfo>;
        console.log('📈 [App] get_project_stats response:', statsResponse);
        if (statsResponse.success && statsResponse.data) {
          currentProject = statsResponse.data;
          projectStore.setProject(statsResponse.data);
          console.log('✅ [App] Project stats updated successfully');
        }
      } else {
        console.warn('⚠️ [App] Failed to import sample data:', response.error);
      }
    } catch (err) {
      console.error('❌ [App] Error importing sample data:', err);
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

  // Handle panel resizing
  let isResizing = false;
  let resizeType = '';

  function startResize(event: MouseEvent, type: string) {
    isResizing = true;
    resizeType = type;
    event.preventDefault();
    
    document.addEventListener('mousemove', handleResize);
    document.addEventListener('mouseup', stopResize);
  }

  function handleResize(event: MouseEvent) {
    if (!isResizing) return;
    
    switch (resizeType) {
      case 'sidebar':
        sidebarWidth = Math.max(200, Math.min(500, event.clientX));
        break;
      case 'ai':
        aiPanelWidth = Math.max(250, Math.min(600, window.innerWidth - event.clientX));
        break;
      case 'graph':
        graphPanelHeight = Math.max(200, Math.min(600, window.innerHeight - event.clientY));
        break;
    }
  }

  function stopResize() {
    isResizing = false;
    resizeType = '';
    document.removeEventListener('mousemove', handleResize);
    document.removeEventListener('mouseup', stopResize);
  }
  
  // Handle keyboard shortcuts
  function handleKeydown(event: KeyboardEvent) {
    const isMac = navigator.platform.toUpperCase().indexOf('MAC') >= 0;
    const cmdOrCtrl = isMac ? event.metaKey : event.ctrlKey;
    
    if (cmdOrCtrl) {
      switch (event.key) {
        case 'b':
          event.preventDefault();
          toggleSidebar();
          break;
        case 'j':
          event.preventDefault();
          toggleAIPanel();
          break;
        case 'g':
          event.preventDefault();
          toggleGraphPanel();
          break;
        case 'n':
          event.preventDefault();
          // TODO: New object
          break;
        case 's':
          event.preventDefault();
          // TODO: Save
          break;
        case 'f':
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
  <TitleBar {currentProject} />
  
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
      <button class="btn primary" on:click={() => window.location.reload()}>
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
            on:projectLoad={(e) => loadProject(e.detail.name, e.detail.path)}
          />
        </div>
        
        <!-- Sidebar Resize Handle -->
        <div class="resize-handle vertical" 
             on:mousedown={(e) => startResize(e, 'sidebar')}></div>
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
            
            <button class="btn icon-only" title="New Object (Ctrl+N)">
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
              <div class="resize-handle horizontal" 
                   on:mousedown={(e) => startResize(e, 'graph')}></div>
              
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
            <div class="resize-handle vertical" 
                 on:mousedown={(e) => startResize(e, 'ai')}></div>
            
            <div 
              class="ai-panel" 
              style="width: {aiPanelWidth}px"
            >
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
    0% { transform: rotate(0deg); }
    100% { transform: rotate(360deg); }
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
      top: var(--titlebar-height);
      bottom: 0;
      z-index: 100;
      box-shadow: 2px 0 8px var(--shadow-medium);
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
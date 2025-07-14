<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/tauri';
  import { recentProjects } from '../stores/projectStore';
  import { uiStore } from '../stores/uiStore';
  import { availableObjectTypes } from '../stores/schemaStore';
  import type { ProjectInfo, TreeNode, ObjectSummary, ApiResponse, SearchResult } from '../types';
  
  export let currentProject: ProjectInfo | null = null;
  
  const dispatch = createEventDispatcher();
  
  let collapsed = false;
  let activeTab = 'explorer'; // 'explorer', 'search', 'recent'
  let searchQuery = '';
  let searchResults: ObjectSummary[] = [];
  let isSearching = false;
  let objects: ObjectSummary[] = [];
  let treeNodes: TreeNode[] = [];
  let selectedNodeId: string | null = null;
  let expandedNodes = new Set<string>();
  
  // Tree structure for organizing objects
  let groupBy = 'type'; // 'type', 'tags', 'recent'
  
  onMount(async () => {
    console.log('🎯 [Sidebar] Component mounted');
    if (currentProject) {
      console.log('🎯 [Sidebar] Current project detected, loading objects...');
      await loadObjects();
    } else {
      console.log('⚠️ [Sidebar] No current project on mount');
    }
  });
  
  async function loadObjects() {
    console.log('📁 [Sidebar] Starting loadObjects...');
    try {
      console.log('📁 [Sidebar] Calling search_knowledge_graph with empty query...');
      const response = await invoke('search_knowledge_graph', {
        request: {
          query: '',
          object_types: null,
          limit: 1000,
          use_semantic: false,
          use_exact: false,
        }
      }) as ApiResponse<SearchResult>;
      
      console.log('📁 [Sidebar] search_knowledge_graph response:', response);
      
      if (response.success && response.data) {
        console.log(`📁 [Sidebar] Found ${response.data.objects.length} objects`);
        objects = response.data.objects;
        console.log('📁 [Sidebar] Objects loaded:', objects);
        buildTreeStructure();
        console.log('✅ [Sidebar] Tree structure built successfully');
      } else {
        console.error('❌ [Sidebar] Search failed:', response.error);
      }
    } catch (error) {
      console.error('❌ [Sidebar] Failed to load objects:', error);
    }
  }
  
  function buildTreeStructure() {
    console.log(`🌳 [Sidebar] Building tree structure with ${objects.length} objects, grouped by: ${groupBy}`);
    const groups = new Map<string, ObjectSummary[]>();
    
    // Group objects based on current groupBy setting
    objects.forEach(obj => {
      let groupKey = '';
      
      switch (groupBy) {
        case 'type':
          groupKey = obj.object_type;
          break;
        case 'tags':
          groupKey = obj.tags.length > 0 ? obj.tags[0] : 'Untagged';
          break;
        case 'recent':
          const date = new Date(obj.created_at);
          const now = new Date();
          const diffDays = Math.floor((now.getTime() - date.getTime()) / (1000 * 60 * 60 * 24));
          
          if (diffDays === 0) groupKey = 'Today';
          else if (diffDays === 1) groupKey = 'Yesterday';
          else if (diffDays <= 7) groupKey = 'This Week';
          else if (diffDays <= 30) groupKey = 'This Month';
          else groupKey = 'Older';
          break;
      }
      
      if (!groups.has(groupKey)) {
        groups.set(groupKey, []);
      }
      groups.get(groupKey)!.push(obj);
    });
    
    console.log(`🌳 [Sidebar] Created ${groups.size} groups:`, Array.from(groups.keys()));
    
    // Convert to tree structure
    treeNodes = Array.from(groups.entries()).map(([groupName, groupObjects]) => {
      const displayGroupName = groupBy === 'type' ? getObjectTypeDisplayName(groupName) : groupName;
      console.log(`🌳 [Sidebar] Processing group: ${groupName} -> ${displayGroupName} (${groupObjects.length} objects)`);
      return {
        id: `group-${groupName}`,
        name: `${displayGroupName} (${groupObjects.length})`,
        type: 'group',
        expanded: expandedNodes.has(`group-${groupName}`),
        children: groupObjects.map(obj => ({
          id: obj.id,
          name: obj.name,
          type: obj.object_type,
          icon: getObjectIcon(obj.object_type),
          selected: selectedNodeId === obj.id,
        }))
      };
    });
    
    console.log(`✅ [Sidebar] Tree structure complete with ${treeNodes.length} groups`);
  }
  
  function getObjectTypeDisplayName(type: string): string {
    // Try to get display name from schema store first
    const objectTypes = $availableObjectTypes;
    const schemaType = objectTypes.find(t => t.value === type);
    if (schemaType?.label) {
      return schemaType.label;
    }
    
    // Fallback to formatted type name
    return type.charAt(0).toUpperCase() + type.slice(1).replace(/_/g, ' ');
  }

  function getObjectIcon(type: string): string {
    // Try to get icon from schema store first
    const objectTypes = $availableObjectTypes;
    const schemaType = objectTypes.find(t => t.value === type);
    if (schemaType?.icon) {
      return schemaType.icon;
    }
    
    // Fallback to static icons
    const icons: Record<string, string> = {
      character: '👤',
      location: '🗺️',
      faction: '⚔️',
      item: '🎒',
      event: '📅',
      session: '🎲',
      custom: '📄',
    };
    return icons[type] || '📄';
  }
  
  function toggleNode(nodeId: string) {
    if (expandedNodes.has(nodeId)) {
      expandedNodes.delete(nodeId);
    } else {
      expandedNodes.add(nodeId);
    }
    expandedNodes = expandedNodes; // Trigger reactivity
    buildTreeStructure();
  }
  
  function selectNode(nodeId: string, nodeType: string) {
    if (nodeType === 'group') {
      toggleNode(nodeId);
      return;
    }
    
    selectedNodeId = nodeId;
    buildTreeStructure();
    
    // Dispatch event to open object in editor
    dispatch('objectSelected', { objectId: nodeId });
    
    // Update UI store
    uiStore.setSelectedObject(nodeId);
  }
  
  async function performSearch() {
    console.log(`🔍 [Sidebar] Performing search for: "${searchQuery}"`);
    if (!searchQuery.trim()) {
      console.log('🔍 [Sidebar] Empty search query, clearing results');
      searchResults = [];
      return;
    }
    
    isSearching = true;
    
    try {
      console.log('🔍 [Sidebar] Calling search_knowledge_graph with query...');
      const response = await invoke('search_knowledge_graph', {
        request: {
          query: searchQuery,
          object_types: null,
          limit: 50,
          use_semantic: true,
          use_exact: true,
        }
      }) as ApiResponse<SearchResult>;
      
      console.log('🔍 [Sidebar] Search response:', response);
      
      if (response.success && response.data) {
        console.log(`✅ [Sidebar] Search found ${response.data.objects.length} results`);
        searchResults = response.data.objects;
      } else {
        console.warn('⚠️ [Sidebar] Search failed:', response.error);
        searchResults = [];
      }
    } catch (error) {
      console.error('❌ [Sidebar] Search failed:', error);
      searchResults = [];
    } finally {
      isSearching = false;
    }
  }
  
  function toggleCollapse() {
    collapsed = !collapsed;
    dispatch('toggleCollapse', { collapsed });
  }
  
  function handleNewProject() {
    dispatch('newProject');
  }
  
  function handleOpenProject() {
    dispatch('openProject');
  }
  
  function handleRecentProject(project: any) {
    dispatch('projectLoad', { name: project.name, path: project.path });
  }
  
  // Auto-search with debouncing
  let searchTimeout: ReturnType<typeof setTimeout>;
  $: {
    if (searchTimeout) clearTimeout(searchTimeout);
    if (searchQuery) {
      searchTimeout = setTimeout(performSearch, 300);
    } else {
      searchResults = [];
    }
  }
  
  // Refresh objects when project changes
  $: if (currentProject) {
    loadObjects();
  }
</script>

<div class="sidebar" class:collapsed>
  <div class="sidebar-header">
    <div class="sidebar-tabs">
      <button 
        class="tab" 
        class:active={activeTab === 'explorer'}
        on:click={() => activeTab = 'explorer'}
        title="Explorer"
      >
        📁
      </button>
      <button 
        class="tab" 
        class:active={activeTab === 'search'}
        on:click={() => activeTab = 'search'}
        title="Search"
      >
        🔍
      </button>
      <button 
        class="tab" 
        class:active={activeTab === 'recent'}
        on:click={() => activeTab = 'recent'}
        title="Recent Projects"
      >
        🕒
      </button>
    </div>
    
    <button 
      class="collapse-btn" 
      on:click={toggleCollapse}
      title={collapsed ? 'Expand Sidebar' : 'Collapse Sidebar'}
    >
      {collapsed ? '▶' : '◀'}
    </button>
  </div>
  
  <div class="sidebar-content">
    {#if activeTab === 'explorer'}
      <div class="explorer-panel">
        {#if currentProject}
          <div class="project-header">
            <div class="project-name" title={currentProject.path}>
              {currentProject.name}
            </div>
            <div class="project-stats">
              {currentProject.object_count} objects
            </div>
          </div>
          
          <div class="explorer-controls">
            <select 
              bind:value={groupBy} 
              on:change={buildTreeStructure}
              class="group-select"
            >
              <option value="type">Group by Type</option>
              <option value="tags">Group by Tags</option>
              <option value="recent">Group by Date</option>
            </select>
            
            <button class="btn icon-only" title="Refresh" on:click={loadObjects}>
              🔄
            </button>
            
            <button class="btn icon-only" title="New Object" on:click={() => dispatch('createNewObject')}>
              ➕
            </button>
          </div>
          
          <div class="tree-view">
            {#each treeNodes as node (node.id)}
              <div class="tree-group">
                <button 
                  class="tree-node group-node"
                  on:click={() => selectNode(node.id, node.type)}
                >
                  <span class="node-icon">
                    {node.expanded ? '▼' : '▶'}
                  </span>
                  <span class="node-name">{node.name}</span>
                </button>
                
                {#if node.expanded && node.children}
                  <div class="tree-children">
                    {#each node.children as child (child.id)}
                      <button 
                        class="tree-node object-node"
                        class:selected={child.selected}
                        on:click={() => selectNode(child.id, child.type)}
                      >
                        <span class="node-icon">{child.icon}</span>
                        <span class="node-name">{child.name}</span>
                      </button>
                    {/each}
                  </div>
                {/if}
              </div>
            {/each}
            
            {#if treeNodes.length === 0}
              <div class="empty-state">
                <div class="empty-icon">📝</div>
                <div class="empty-message">No objects found</div>
                <button class="btn primary" on:click={() => {}}>
                  Create First Object
                </button>
              </div>
            {/if}
          </div>
        {:else}
          <div class="no-project">
            <div class="empty-icon">📁</div>
            <div class="empty-message">No project open</div>
            <div class="project-actions">
              <button class="btn primary" on:click={handleNewProject}>
                New Project
              </button>
              <button class="btn secondary" on:click={handleOpenProject}>
                Open Project
              </button>
            </div>
          </div>
        {/if}
      </div>
    
    {:else if activeTab === 'search'}
      <div class="search-panel">
        <div class="search-input-container">
          <input 
            type="search"
            bind:value={searchQuery}
            placeholder="Search objects..."
            class="search-input"
          />
          {#if isSearching}
            <div class="search-spinner">⏳</div>
          {/if}
        </div>
        
        <div class="search-results">
          {#if searchResults.length > 0}
            {#each searchResults as result (result.id)}
              <button 
                class="search-result"
                on:click={() => selectNode(result.id, result.object_type)}
              >
                <div class="result-icon">
                  {getObjectIcon(result.object_type)}
                </div>
                <div class="result-content">
                  <div class="result-name">{result.name}</div>
                  <div class="result-type">{result.object_type}</div>
                  {#if result.description}
                    <div class="result-description">
                      {result.description.slice(0, 100)}...
                    </div>
                  {/if}
                </div>
              </button>
            {/each}
          {:else if searchQuery && !isSearching}
            <div class="empty-state">
              <div class="empty-icon">🔍</div>
              <div class="empty-message">No results found</div>
            </div>
          {:else if !searchQuery}
            <div class="search-tips">
              <h4>Search Tips</h4>
              <ul>
                <li>Use quotes for exact matches</li>
                <li>Search by name, description, or tags</li>
                <li>Use semantic search for related concepts</li>
              </ul>
            </div>
          {/if}
        </div>
      </div>
    
    {:else if activeTab === 'recent'}
      <div class="recent-panel">
        <div class="recent-header">
          <h3>Recent Projects</h3>
          <button class="btn icon-only" title="Clear Recent">
            🗑️
          </button>
        </div>
        
        <div class="recent-list">
          {#each $recentProjects as project (project.path)}
            <button 
              class="recent-project"
              on:click={() => handleRecentProject(project)}
            >
              <div class="project-icon">📁</div>
              <div class="project-info">
                <div class="project-name">{project.name}</div>
                <div class="project-path">{project.path}</div>
                <div class="project-date">
                  {new Date(project.last_opened).toLocaleDateString()}
                </div>
              </div>
            </button>
          {:else}
            <div class="empty-state">
              <div class="empty-icon">📁</div>
              <div class="empty-message">No recent projects</div>
            </div>
          {/each}
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg-secondary);
    border-right: 1px solid var(--border-color);
    transition: width var(--transition-normal);
    min-width: 200px;
  }
  
  .sidebar.collapsed {
    min-width: var(--sidebar-collapsed-width);
  }
  
  .sidebar-header {
    display: flex;
    align-items: center;
    padding: var(--space-sm);
    border-bottom: 1px solid var(--border-color);
    gap: var(--space-sm);
  }
  
  .sidebar-tabs {
    display: flex;
    gap: 2px;
    flex: 1;
  }
  
  .tab {
    background: transparent;
    border: none;
    padding: var(--space-sm);
    border-radius: var(--radius-sm);
    cursor: pointer;
    color: var(--text-secondary);
    transition: all var(--transition-fast);
    font-size: var(--font-md);
  }
  
  .tab:hover {
    background: var(--bg-tertiary);
    color: var(--text-primary);
  }
  
  .tab.active {
    background: var(--accent-color);
    color: white;
  }
  
  .collapse-btn {
    background: transparent;
    border: none;
    padding: var(--space-xs);
    border-radius: var(--radius-sm);
    cursor: pointer;
    color: var(--text-secondary);
    transition: all var(--transition-fast);
  }
  
  .collapse-btn:hover {
    background: var(--bg-tertiary);
    color: var(--text-primary);
  }
  
  .sidebar-content {
    flex: 1;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
  
  .explorer-panel,
  .search-panel,
  .recent-panel {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  
  /* Project Header */
  .project-header {
    padding: var(--space-md);
    border-bottom: 1px solid var(--border-color);
  }
  
  .project-name {
    font-weight: 600;
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  
  .project-stats {
    font-size: var(--font-xs);
    color: var(--text-muted);
    margin-top: var(--space-xs);
  }
  
  /* Explorer Controls */
  .explorer-controls {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm);
    border-bottom: 1px solid var(--border-color);
  }
  
  .group-select {
    flex: 1;
    background: var(--bg-tertiary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    padding: var(--space-xs) var(--space-sm);
    font-size: var(--font-sm);
  }
  
  /* Tree View */
  .tree-view {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-sm);
  }
  
  .tree-group {
    margin-bottom: var(--space-xs);
  }
  
  .tree-node {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    width: 100%;
    padding: var(--space-xs) var(--space-sm);
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    cursor: pointer;
    color: var(--text-primary);
    transition: background-color var(--transition-fast);
    text-align: left;
  }
  
  .tree-node:hover {
    background: var(--bg-tertiary);
  }
  
  .tree-node.selected {
    background: var(--accent-light);
    color: white;
  }
  
  .group-node {
    font-weight: 500;
    color: var(--text-secondary);
  }
  
  .object-node {
    font-size: var(--font-sm);
  }
  
  .tree-children {
    margin-left: var(--space-lg);
    border-left: 1px solid var(--border-color);
    padding-left: var(--space-sm);
  }
  
  .node-icon {
    flex-shrink: 0;
    font-size: var(--font-sm);
  }
  
  .node-name {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  
  /* Search Panel */
  .search-input-container {
    position: relative;
    padding: var(--space-md);
    border-bottom: 1px solid var(--border-color);
  }
  
  .search-input {
    width: 100%;
    background: var(--bg-tertiary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    padding: var(--space-sm);
    font-size: var(--font-sm);
  }
  
  .search-spinner {
    position: absolute;
    right: var(--space-lg);
    top: 50%;
    transform: translateY(-50%);
    font-size: var(--font-sm);
  }
  
  .search-results {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-sm);
  }
  
  .search-result {
    display: flex;
    align-items: flex-start;
    gap: var(--space-sm);
    width: 100%;
    padding: var(--space-sm);
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    cursor: pointer;
    text-align: left;
    transition: background-color var(--transition-fast);
  }
  
  .search-result:hover {
    background: var(--bg-tertiary);
  }
  
  .result-icon {
    flex-shrink: 0;
    font-size: var(--font-lg);
  }
  
  .result-content {
    flex: 1;
    min-width: 0;
  }
  
  .result-name {
    font-weight: 500;
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  
  .result-type {
    font-size: var(--font-xs);
    color: var(--text-muted);
    text-transform: capitalize;
  }
  
  .result-description {
    font-size: var(--font-xs);
    color: var(--text-secondary);
    margin-top: var(--space-xs);
    line-height: 1.3;
  }
  
  .search-tips {
    padding: var(--space-md);
  }
  
  .search-tips h4 {
    margin: 0 0 var(--space-sm) 0;
    color: var(--text-primary);
    font-size: var(--font-sm);
  }
  
  .search-tips ul {
    margin: 0;
    padding-left: var(--space-lg);
    color: var(--text-secondary);
    font-size: var(--font-xs);
  }
  
  .search-tips li {
    margin-bottom: var(--space-xs);
  }
  
  /* Recent Panel */
  .recent-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-md);
    border-bottom: 1px solid var(--border-color);
  }
  
  .recent-header h3 {
    margin: 0;
    font-size: var(--font-md);
    color: var(--text-primary);
  }
  
  .recent-list {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-sm);
  }
  
  .recent-project {
    display: flex;
    align-items: flex-start;
    gap: var(--space-sm);
    width: 100%;
    padding: var(--space-sm);
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    cursor: pointer;
    text-align: left;
    transition: background-color var(--transition-fast);
    margin-bottom: var(--space-xs);
  }
  
  .recent-project:hover {
    background: var(--bg-tertiary);
  }
  
  .project-icon {
    flex-shrink: 0;
    font-size: var(--font-lg);
  }
  
  .project-info {
    flex: 1;
    min-width: 0;
  }
  
  .project-info .project-name {
    font-weight: 500;
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  
  .project-path {
    font-size: var(--font-xs);
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    margin-top: 2px;
  }
  
  .project-date {
    font-size: var(--font-xs);
    color: var(--text-secondary);
    margin-top: 2px;
  }
  
  /* Empty States */
  .empty-state,
  .no-project {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: var(--space-xl);
    text-align: center;
    color: var(--text-muted);
  }
  
  .empty-icon {
    font-size: 2rem;
    margin-bottom: var(--space-md);
    opacity: 0.5;
  }
  
  .empty-message {
    margin-bottom: var(--space-lg);
    font-size: var(--font-sm);
  }
  
  .project-actions {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
    width: 100%;
  }
  
  /* Collapsed state adjustments */
  .sidebar.collapsed .sidebar-tabs,
  .sidebar.collapsed .sidebar-content {
    display: none;
  }
  
  .sidebar.collapsed .sidebar-header {
    justify-content: center;
  }
  
  /* Responsive adjustments */
  @media (max-width: 768px) {
    .sidebar {
      position: absolute;
      left: 0;
      top: 0;
      bottom: 0;
      z-index: 100;
      box-shadow: 2px 0 8px var(--shadow-medium);
    }
  }
</style>
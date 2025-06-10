<script lang="ts">
  import { createEventDispatcher, onMount, onDestroy } from 'svelte';
  import { graphStore, filteredGraphData, selectedNodes, selectedEdges, graphSettings } from '../stores/graphStore';
  import { uiStore } from '../stores/uiStore';
  import type { GraphNode, GraphEdge, GraphData } from '../types';
  
  export let width = 800;
  export let height = 400;
  
  const dispatch = createEventDispatcher();
  
  let svgElement: SVGSVGElement;
  let containerElement: HTMLDivElement;
  let simulation: any = null;
  let nodes: any[] = [];
  let links: any[] = [];
  let transform = { x: 0, y: 0, k: 1 };
  let isDragging = false;
  let draggedNode: any = null;
  let isLoading = false;
  let error: string | null = null;
  
  // Graph layout settings
  let forceStrength = -300;
  let linkDistance = 100;
  let linkStrength = 0.1;
  let centeringStrength = 0.1;
  let showLabels = true;
  let labelMinZoom = 0.5;
  
  // Filter controls
  let filterByType = '';
  let filterByTag = '';
  let showLegend = true;
  
  // D3 will be loaded dynamically
  let d3: any = null;
  
  onMount(async () => {
    await loadD3();
    await initializeGraph();
    setupEventListeners();
  });
  
  onDestroy(() => {
    if (simulation) {
      simulation.stop();
    }
  });
  
  async function loadD3() {
    try {
      // In a real implementation, you'd import D3 modules
      // For now, we'll create a mock D3 object for the UI structure
      d3 = {
        select: (selector: string) => ({ 
          selectAll: () => ({ 
            data: () => ({ 
              enter: () => ({ append: () => ({}) }),
              exit: () => ({ remove: () => {} }),
              attr: () => ({}),
              style: () => ({}),
              on: () => ({})
            })
          })
        }),
        forceSimulation: () => ({
          nodes: () => ({}),
          force: () => ({}),
          on: () => ({}),
          stop: () => ({}),
          restart: () => ({})
        }),
        forceManyBody: () => ({ strength: () => ({}) }),
        forceLink: () => ({ 
          id: () => ({}), 
          distance: () => ({}), 
          strength: () => ({}) 
        }),
        forceCenter: () => ({}),
        zoom: () => ({
          scaleExtent: () => ({}),
          on: () => ({})
        }),
        drag: () => ({
          on: () => ({})
        }),
        event: { transform: { x: 0, y: 0, k: 1 } }
      };
    } catch (err) {
      console.error('Failed to load D3:', err);
      error = 'Failed to load graph visualization library';
    }
  }
  
  async function initializeGraph() {
    if (!d3 || !svgElement) return;
    
    isLoading = true;
    error = null;
    
    try {
      // Subscribe to graph data changes
      const unsubscribe = filteredGraphData.subscribe(data => {
        updateGraphData(data);
      });
      
      // Set up SVG dimensions
      updateDimensions();
      
      // Initialize simulation
      setupSimulation();
      
      isLoading = false;
    } catch (err) {
      console.error('Failed to initialize graph:', err);
      error = 'Failed to initialize graph visualization';
      isLoading = false;
    }
  }
  
  function updateDimensions() {
    if (containerElement) {
      const rect = containerElement.getBoundingClientRect();
      width = rect.width;
      height = rect.height;
    }
  }
  
  function setupSimulation() {
    if (!d3) return;
    
    simulation = d3.forceSimulation(nodes)
      .force('link', d3.forceLink(links)
        .id((d: any) => d.id)
        .distance(linkDistance)
        .strength(linkStrength))
      .force('charge', d3.forceManyBody().strength(forceStrength))
      .force('center', d3.forceCenter(width / 2, height / 2))
      .on('tick', tick);
  }
  
  function updateGraphData(data: GraphData) {
    if (!data) return;
    
    // Convert graph data to D3 format
    nodes = data.nodes.map(node => ({
      ...node,
      x: Math.random() * width,
      y: Math.random() * height,
      fx: null,
      fy: null,
      radius: Math.max(8, Math.min(32, node.size * 2)),
    }));
    
    links = data.edges.map(edge => ({
      ...edge,
      source: edge.source,
      target: edge.target,
      width: edge.weight ? Math.max(1, edge.weight * 3) : 2,
    }));
    
    if (simulation) {
      simulation.nodes(nodes);
      simulation.force('link').links(links);
      simulation.restart();
    }
    
    render();
  }
  
  function tick() {
    render();
  }
  
  function render() {
    if (!svgElement || !d3) return;
    
    // In a real implementation, this would update SVG elements
    // For now, we'll trigger a reactive update
    nodes = [...nodes];
    links = [...links];
  }
  
  function setupEventListeners() {
    if (!svgElement || !d3) return;
    
    // Set up zoom behavior
    const zoom = d3.zoom()
      .scaleExtent([0.1, 10])
      .on('zoom', handleZoom);
    
    d3.select(svgElement).call(zoom);
    
    // Set up drag behavior for nodes
    const drag = d3.drag()
      .on('start', handleDragStart)
      .on('drag', handleDrag)
      .on('end', handleDragEnd);
    
    // Apply drag to node elements (would be done in real D3 implementation)
  }
  
  function handleZoom(event: any) {
    if (event && event.transform) {
      transform = event.transform;
      render();
    }
  }
  
  function handleDragStart(event: any, d: any) {
    if (!event.active && simulation) simulation.alphaTarget(0.3).restart();
    d.fx = d.x;
    d.fy = d.y;
    draggedNode = d;
    isDragging = true;
  }
  
  function handleDrag(event: any, d: any) {
    d.fx = event.x;
    d.fy = event.y;
  }
  
  function handleDragEnd(event: any, d: any) {
    if (!event.active && simulation) simulation.alphaTarget(0);
    d.fx = null;
    d.fy = null;
    draggedNode = null;
    isDragging = false;
  }
  
  function selectNode(node: any, addToSelection = false) {
    graphStore.selectNode(node.id, addToSelection);
    dispatch('nodeSelected', { node });
  }
  
  function selectEdge(edge: any, addToSelection = false) {
    const edgeId = `${edge.source.id || edge.source}-${edge.target.id || edge.target}-${edge.edge_type}`;
    graphStore.selectEdge(edgeId, addToSelection);
    dispatch('edgeSelected', { edge });
  }
  
  function handleNodeClick(event: MouseEvent, node: any) {
    const addToSelection = event.ctrlKey || event.metaKey;
    selectNode(node, addToSelection);
  }
  
  function handleEdgeClick(event: MouseEvent, edge: any) {
    const addToSelection = event.ctrlKey || event.metaKey;
    selectEdge(edge, addToSelection);
  }
  
  function centerGraph() {
    graphStore.fitToView();
  }
  
  function resetZoom() {
    transform = { x: 0, y: 0, k: 1 };
    render();
  }
  
  function toggleSimulation() {
    if (simulation) {
      if (simulation.alpha() > 0) {
        simulation.stop();
      } else {
        simulation.restart();
      }
    }
  }
  
  function refreshData() {
    dispatch('refreshData');
  }
  
  function exportGraph() {
    // Export functionality would be implemented here
    console.log('Export graph functionality');
  }
  
  function getNodeColor(node: any): string {
    const colors = $graphSettings.colors.nodes;
    return colors[node.node_type] || colors.custom || '#666666';
  }
  
  function getEdgeColor(edge: any): string {
    const colors = $graphSettings.colors.edges;
    return colors[edge.edge_type] || colors.custom || '#666666';
  }
  
  function isNodeSelected(nodeId: string): boolean {
    return $selectedNodes.includes(nodeId);
  }
  
  function isEdgeSelected(edge: any): boolean {
    const edgeId = `${edge.source.id || edge.source}-${edge.target.id || edge.target}-${edge.edge_type}`;
    return $selectedEdges.includes(edgeId);
  }
  
  // Reactive statements
  $: if (simulation) {
    simulation.force('charge').strength(forceStrength);
    simulation.force('link').distance(linkDistance).strength(linkStrength);
    simulation.restart();
  }
  
  let resizeObserver;
  
  $: if (containerElement) {
    resizeObserver?.disconnect();
    resizeObserver = new ResizeObserver(updateDimensions);
    resizeObserver.observe(containerElement);
  }
  
  onDestroy(() => {
    resizeObserver?.disconnect();
  });
</script>

<div class="graph-view" bind:this={containerElement}>
  <!-- Graph Controls -->
  <div class="graph-controls">
    <div class="control-group">
      <button 
        class="btn icon-only" 
        on:click={centerGraph}
        title="Center Graph"
      >
        🎯
      </button>
      
      <button 
        class="btn icon-only" 
        on:click={resetZoom}
        title="Reset Zoom"
      >
        🔍
      </button>
      
      <button 
        class="btn icon-only" 
        on:click={toggleSimulation}
        title="Toggle Physics"
      >
        ⚡
      </button>
      
      <button 
        class="btn icon-only" 
        on:click={refreshData}
        title="Refresh Data"
      >
        🔄
      </button>
      
      <button 
        class="btn icon-only" 
        on:click={exportGraph}
        title="Export Graph"
      >
        📤
      </button>
    </div>
    
    <div class="control-group">
      <label class="control-label">
        <input 
          type="checkbox" 
          bind:checked={showLabels}
          class="checkbox"
        />
        Labels
      </label>
      
      <label class="control-label">
        <input 
          type="checkbox" 
          bind:checked={showLegend}
          class="checkbox"
        />
        Legend
      </label>
    </div>
    
    <div class="control-group">
      <select bind:value={filterByType} class="filter-select">
        <option value="">All Types</option>
        <option value="character">Characters</option>
        <option value="location">Locations</option>
        <option value="faction">Factions</option>
        <option value="item">Items</option>
        <option value="event">Events</option>
        <option value="session">Sessions</option>
      </select>
    </div>
  </div>
  
  <!-- Main Graph Area -->
  <div class="graph-container">
    {#if isLoading}
      <div class="graph-loading">
        <div class="loading-spinner"></div>
        <p>Loading graph...</p>
      </div>
    {:else if error}
      <div class="graph-error">
        <div class="error-icon">⚠️</div>
        <h4>Graph Error</h4>
        <p>{error}</p>
        <button class="btn primary" on:click={initializeGraph}>
          Retry
        </button>
      </div>
    {:else if nodes.length === 0}
      <div class="graph-empty">
        <div class="empty-icon">🕸️</div>
        <h4>No Graph Data</h4>
        <p>Create some objects and relationships to see your knowledge graph.</p>
      </div>
    {:else}
      <svg 
        bind:this={svgElement}
        {width}
        {height}
        class="graph-svg"
      >
        <!-- Graph Background -->
        <rect 
          width="100%" 
          height="100%" 
          fill="var(--bg-primary)"
        />
        
        <!-- Graph Content Group (for zoom/pan) -->
        <g transform="translate({transform.x}, {transform.y}) scale({transform.k})">
          <!-- Edges -->
          {#each links as link}
            <line
              x1={link.source.x || 0}
              y1={link.source.y || 0}
              x2={link.target.x || 0}
              y2={link.target.y || 0}
              stroke={getEdgeColor(link)}
              stroke-width={link.width || 2}
              stroke-opacity={isEdgeSelected(link) ? 1 : 0.6}
              class="graph-edge"
              class:selected={isEdgeSelected(link)}
              on:click={(e) => handleEdgeClick(e, link)}
            />
            
            <!-- Edge Labels -->
            {#if showLabels && transform.k >= labelMinZoom}
              <text
                x={(link.source.x + link.target.x) / 2}
                y={(link.source.y + link.target.y) / 2}
                text-anchor="middle"
                class="edge-label"
                font-size={Math.max(8, 12 / transform.k)}
              >
                {link.edge_type}
              </text>
            {/if}
          {/each}
          
          <!-- Nodes -->
          {#each nodes as node}
            <circle
              cx={node.x || 0}
              cy={node.y || 0}
              r={node.radius || 10}
              fill={getNodeColor(node)}
              stroke={isNodeSelected(node.id) ? 'var(--accent-color)' : 'var(--border-color)'}
              stroke-width={isNodeSelected(node.id) ? 3 : 1}
              class="graph-node"
              class:selected={isNodeSelected(node.id)}
              on:click={(e) => handleNodeClick(e, node)}
              style="cursor: pointer;"
            />
            
            <!-- Node Labels -->
            {#if showLabels && transform.k >= labelMinZoom}
              <text
                x={node.x || 0}
                y={(node.y || 0) + (node.radius || 10) + 15}
                text-anchor="middle"
                class="node-label"
                font-size={Math.max(10, 14 / transform.k)}
              >
                {node.name.length > 20 ? node.name.slice(0, 20) + '...' : node.name}
              </text>
            {/if}
          {/each}
        </g>
        
        <!-- Selection Rectangle (for multi-select) -->
        <!-- Would be implemented for advanced selection -->
      </svg>
    {/if}
  </div>
  
  <!-- Graph Legend -->
  {#if showLegend && !isLoading && !error}
    <div class="graph-legend">
      <h5>Node Types</h5>
      <div class="legend-items">
        {#each Object.entries($graphSettings.colors.nodes) as [type, color]}
          <div class="legend-item">
            <div 
              class="legend-color" 
              style="background-color: {color}"
            ></div>
            <span class="legend-label">{type}</span>
          </div>
        {/each}
      </div>
      
      <h5>Selection</h5>
      <div class="selection-info">
        <div class="selection-stat">
          Nodes: {$selectedNodes.length}
        </div>
        <div class="selection-stat">
          Edges: {$selectedEdges.length}
        </div>
      </div>
    </div>
  {/if}
  
  <!-- Graph Settings Panel (collapsible) -->
  <div class="graph-settings">
    <details>
      <summary>Physics Settings</summary>
      <div class="setting-item">
        <label>Force Strength</label>
        <input 
          type="range" 
          bind:value={forceStrength}
          min="-1000"
          max="0"
          step="10"
          class="range-input"
        />
        <span class="setting-value">{forceStrength}</span>
      </div>
      
      <div class="setting-item">
        <label>Link Distance</label>
        <input 
          type="range" 
          bind:value={linkDistance}
          min="10"
          max="300"
          step="10"
          class="range-input"
        />
        <span class="setting-value">{linkDistance}</span>
      </div>
      
      <div class="setting-item">
        <label>Link Strength</label>
        <input 
          type="range" 
          bind:value={linkStrength}
          min="0"
          max="1"
          step="0.1"
          class="range-input"
        />
        <span class="setting-value">{linkStrength}</span>
      </div>
    </details>
  </div>
</div>

<style>
  .graph-view {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg-primary);
    position: relative;
    overflow: hidden;
  }
  
  /* Graph Controls */
  .graph-controls {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-sm) var(--space-md);
    background: var(--bg-secondary);
    border-bottom: 1px solid var(--border-color);
    gap: var(--space-md);
    flex-wrap: wrap;
  }
  
  .control-group {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }
  
  .control-label {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    font-size: var(--font-sm);
    color: var(--text-primary);
    cursor: pointer;
  }
  
  .filter-select {
    background: var(--bg-tertiary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    padding: var(--space-xs) var(--space-sm);
    font-size: var(--font-sm);
  }
  
  /* Graph Container */
  .graph-container {
    flex: 1;
    position: relative;
    overflow: hidden;
  }
  
  .graph-svg {
    width: 100%;
    height: 100%;
    display: block;
  }
  
  /* Graph Elements */
  .graph-node {
    transition: stroke var(--transition-fast);
  }
  
  .graph-node:hover {
    stroke: var(--accent-color);
    stroke-width: 2;
  }
  
  .graph-node.selected {
    stroke: var(--accent-color);
    stroke-width: 3;
  }
  
  .graph-edge {
    transition: stroke-opacity var(--transition-fast);
    cursor: pointer;
  }
  
  .graph-edge:hover {
    stroke-opacity: 1;
  }
  
  .graph-edge.selected {
    stroke-opacity: 1;
    stroke-width: 3;
  }
  
  .node-label,
  .edge-label {
    fill: var(--text-primary);
    font-family: inherit;
    pointer-events: none;
    user-select: none;
  }
  
  .edge-label {
    fill: var(--text-muted);
    font-size: 10px;
  }
  
  /* Loading and Error States */
  .graph-loading,
  .graph-error,
  .graph-empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    text-align: center;
    padding: var(--space-xl);
  }
  
  .loading-spinner {
    width: 40px;
    height: 40px;
    border: 3px solid var(--border-color);
    border-top: 3px solid var(--accent-color);
    border-radius: 50%;
    animation: spin 1s linear infinite;
    margin-bottom: var(--space-lg);
  }
  
  .error-icon,
  .empty-icon {
    font-size: 3rem;
    margin-bottom: var(--space-lg);
    opacity: 0.7;
  }
  
  @keyframes spin {
    0% { transform: rotate(0deg); }
    100% { transform: rotate(360deg); }
  }
  
  /* Graph Legend */
  .graph-legend {
    position: absolute;
    top: var(--space-md);
    right: var(--space-md);
    background: var(--bg-secondary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius-md);
    padding: var(--space-md);
    min-width: 150px;
    box-shadow: 0 4px 12px var(--shadow-medium);
  }
  
  .graph-legend h5 {
    margin: 0 0 var(--space-sm) 0;
    font-size: var(--font-sm);
    color: var(--text-primary);
  }
  
  .legend-items {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    margin-bottom: var(--space-md);
  }
  
  .legend-item {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }
  
  .legend-color {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 1px solid var(--border-color);
  }
  
  .legend-label {
    font-size: var(--font-xs);
    color: var(--text-secondary);
    text-transform: capitalize;
  }
  
  .selection-info {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }
  
  .selection-stat {
    font-size: var(--font-xs);
    color: var(--text-muted);
  }
  
  /* Graph Settings */
  .graph-settings {
    position: absolute;
    bottom: var(--space-md);
    left: var(--space-md);
    background: var(--bg-secondary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius-md);
    box-shadow: 0 4px 12px var(--shadow-medium);
  }
  
  .graph-settings summary {
    padding: var(--space-sm) var(--space-md);
    cursor: pointer;
    font-size: var(--font-sm);
    color: var(--text-primary);
    user-select: none;
  }
  
  .graph-settings[open] {
    min-width: 200px;
  }
  
  .setting-item {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    border-top: 1px solid var(--border-color);
  }
  
  .setting-item label {
    flex: 1;
    font-size: var(--font-xs);
    color: var(--text-secondary);
  }
  
  .range-input {
    flex: 2;
    margin: 0;
  }
  
  .setting-value {
    flex: 0 0 auto;
    font-size: var(--font-xs);
    color: var(--text-muted);
    min-width: 30px;
    text-align: right;
  }
  
  /* Responsive adjustments */
  @media (max-width: 768px) {
    .graph-legend,
    .graph-settings {
      position: static;
      margin: var(--space-sm);
    }
    
    .graph-controls {
      flex-direction: column;
      align-items: stretch;
      gap: var(--space-sm);
    }
  }
</style>
<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { projectStore, hasUnsavedChanges } from '../stores/projectStore';
  import { uiStore, hasUnsavedEditorChanges } from '../stores/uiStore';
  import { graphStore, graphStats } from '../stores/graphStore';
  import type { ProjectInfo } from '../types';
  
  export let currentProject: ProjectInfo | null = null;
  
  let connectionStatus = 'connected';
  let lastSaveTime: Date | null = null;
  let memoryUsage = 0;
  let processingQueue = 0;
  let currentTime = new Date();
  let timeInterval: number;
  
  // Status indicators
  let saveStatus = 'saved'; // 'saved', 'saving', 'unsaved', 'error'
  let aiStatus = 'connected'; // 'connected', 'disconnected', 'processing'
  let graphStatus = 'ready'; // 'ready', 'loading', 'error'
  
  onMount(() => {
    // Update current time every second
    timeInterval = setInterval(() => {
      currentTime = new Date();
    }, 1000);
    
    // Mock status updates (in real app, these would come from actual services)
    updateStatuses();
    
    return () => {
      if (timeInterval) clearInterval(timeInterval);
    };
  });
  
  onDestroy(() => {
    if (timeInterval) clearInterval(timeInterval);
  });
  
  function updateStatuses() {
    // Mock periodic status updates
    setInterval(() => {
      // Simulate memory usage
      memoryUsage = Math.floor(Math.random() * 100) + 200; // 200-300 MB
      
      // Simulate processing queue
      processingQueue = Math.floor(Math.random() * 5);
    }, 5000);
  }
  
  function formatTime(date: Date): string {
    return date.toLocaleTimeString([], { 
      hour: '2-digit', 
      minute: '2-digit',
      second: '2-digit'
    });
  }
  
  function formatDate(date: Date): string {
    return date.toLocaleDateString();
  }
  
  function formatMemory(mb: number): string {
    if (mb < 1024) {
      return `${mb} MB`;
    }
    return `${(mb / 1024).toFixed(1)} GB`;
  }
  
  function getStatusColor(status: string): string {
    switch (status) {
      case 'connected':
      case 'saved':
      case 'ready':
        return 'var(--success-color)';
      case 'saving':
      case 'processing':
      case 'loading':
        return 'var(--warning-color)';
      case 'disconnected':
      case 'unsaved':
      case 'error':
        return 'var(--error-color)';
      default:
        return 'var(--text-muted)';
    }
  }
  
  function getStatusIcon(status: string): string {
    switch (status) {
      case 'connected':
        return '🟢';
      case 'disconnected':
        return '🔴';
      case 'saving':
      case 'processing':
      case 'loading':
        return '🟡';
      case 'saved':
        return '✅';
      case 'unsaved':
        return '⚠️';
      case 'error':
        return '❌';
      case 'ready':
        return '🟢';
      default:
        return '⚪';
    }
  }
  
  function handleSaveAll() {
    // Trigger save all action
    console.log('Save all triggered');
  }
  
  function handleRefreshProject() {
    // Trigger project refresh
    console.log('Refresh project triggered');
  }
  
  function toggleVerboseMode() {
    // Toggle detailed status information
    console.log('Toggle verbose mode');
  }
  
  // Reactive statements
  $: if ($hasUnsavedChanges || $hasUnsavedEditorChanges) {
    saveStatus = 'unsaved';
  } else {
    saveStatus = 'saved';
    lastSaveTime = new Date();
  }
</script>

<div class="status-bar">
  <!-- Left Section - Project Info -->
  <div class="status-section status-left">
    {#if currentProject}
      <div class="status-item project-info">
        <span class="project-name" title={currentProject.path}>
          {currentProject.name}
        </span>
        <span class="project-stats">
          {currentProject.object_count} objects • {currentProject.relationship_count} connections
        </span>
      </div>
      
      <div class="status-separator"></div>
      
      <div class="status-item save-status">
        <span 
          class="status-indicator"
          style="color: {getStatusColor(saveStatus)}"
          title="Save status"
        >
          {getStatusIcon(saveStatus)}
        </span>
        <span class="status-text">
          {#if saveStatus === 'saved' && lastSaveTime}
            Saved {formatTime(lastSaveTime)}
          {:else if saveStatus === 'saving'}
            Saving...
          {:else if saveStatus === 'unsaved'}
            Unsaved changes
          {:else}
            {saveStatus}
          {/if}
        </span>
        {#if saveStatus === 'unsaved'}
          <button 
            class="save-button"
            on:click={handleSaveAll}
            title="Save all changes"
          >
            Save
          </button>
        {/if}
      </div>
    {:else}
      <div class="status-item">
        <span class="status-text">No project open</span>
      </div>
    {/if}
  </div>
  
  <!-- Center Section - Processing Info -->
  <div class="status-section status-center">
    {#if $graphStats}
      <div class="status-item graph-stats">
        <span class="status-icon">🕸️</span>
        <span class="status-text">
          Graph: {$graphStats.totalNodes}N {$graphStats.totalEdges}E
        </span>
      </div>
    {/if}
    
    {#if processingQueue > 0}
      <div class="status-separator"></div>
      <div class="status-item processing-queue">
        <span class="status-icon">⏳</span>
        <span class="status-text">
          Processing: {processingQueue}
        </span>
      </div>
    {/if}
  </div>
  
  <!-- Right Section - System Status -->
  <div class="status-section status-right">
    <!-- AI Status -->
    <div class="status-item ai-status">
      <span 
        class="status-indicator"
        style="color: {getStatusColor(aiStatus)}"
        title="AI connection status"
      >
        {getStatusIcon(aiStatus)}
      </span>
      <span class="status-text">AI</span>
    </div>
    
    <div class="status-separator"></div>
    
    <!-- Memory Usage -->
    <div class="status-item memory-usage" title="Memory usage">
      <span class="status-icon">💾</span>
      <span class="status-text">{formatMemory(memoryUsage)}</span>
    </div>
    
    <div class="status-separator"></div>
    
    <!-- Current Time -->
    <div class="status-item current-time" title={formatDate(currentTime)}>
      <span class="status-icon">🕐</span>
      <span class="status-text">{formatTime(currentTime)}</span>
    </div>
    
    <!-- Settings Button -->
    <button 
      class="status-button"
      on:click={toggleVerboseMode}
      title="Toggle detailed status"
    >
      ⚙️
    </button>
  </div>
</div>

<style>
  .status-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: var(--footer-height);
    background: var(--bg-secondary);
    border-top: 1px solid var(--border-color);
    padding: 0 var(--space-md);
    font-size: var(--font-xs);
    color: var(--text-secondary);
    user-select: none;
  }
  
  .status-section {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }
  
  .status-left {
    flex: 1;
    justify-content: flex-start;
  }
  
  .status-center {
    flex: 0 0 auto;
    justify-content: center;
  }
  
  .status-right {
    flex: 1;
    justify-content: flex-end;
  }
  
  .status-item {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    white-space: nowrap;
  }
  
  .status-separator {
    width: 1px;
    height: 16px;
    background: var(--border-color);
    margin: 0 var(--space-xs);
  }
  
  .status-text {
    color: var(--text-secondary);
    font-size: var(--font-xs);
  }
  
  .status-icon,
  .status-indicator {
    font-size: var(--font-xs);
    display: flex;
    align-items: center;
  }
  
  /* Project Info */
  .project-info {
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
  }
  
  .project-name {
    font-weight: 500;
    color: var(--text-primary);
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  
  .project-stats {
    color: var(--text-muted);
    font-size: 10px;
  }
  
  /* Save Status */
  .save-status {
    gap: var(--space-sm);
  }
  
  .save-button {
    background: var(--accent-color);
    border: none;
    border-radius: var(--radius-sm);
    color: white;
    padding: 2px var(--space-xs);
    font-size: 10px;
    cursor: pointer;
    transition: background-color var(--transition-fast);
  }
  
  .save-button:hover {
    background: var(--accent-hover);
  }
  
  /* Graph Stats */
  .graph-stats .status-text {
    font-family: monospace;
  }
  
  /* Memory Usage */
  .memory-usage .status-text {
    font-family: monospace;
    min-width: 40px;
    text-align: right;
  }
  
  /* Current Time */
  .current-time .status-text {
    font-family: monospace;
    min-width: 60px;
    text-align: right;
  }
  
  /* Status Button */
  .status-button {
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    padding: var(--space-xs);
    border-radius: var(--radius-sm);
    font-size: var(--font-xs);
    transition: all var(--transition-fast);
    margin-left: var(--space-xs);
  }
  
  .status-button:hover {
    background: var(--bg-tertiary);
    color: var(--text-primary);
  }
  
  /* Status indicators animation */
  .status-indicator {
    animation: pulse 2s infinite;
  }
  
  @keyframes pulse {
    0%, 100% {
      opacity: 1;
    }
    50% {
      opacity: 0.7;
    }
  }
  
  /* Processing queue animation */
  .processing-queue .status-icon {
    animation: spin 2s linear infinite;
  }
  
  @keyframes spin {
    from {
      transform: rotate(0deg);
    }
    to {
      transform: rotate(360deg);
    }
  }
  
  /* Responsive adjustments */
  @media (max-width: 1024px) {
    .status-center {
      display: none;
    }
    
    .project-stats {
      display: none;
    }
  }
  
  @media (max-width: 768px) {
    .status-bar {
      padding: 0 var(--space-sm);
      gap: var(--space-xs);
    }
    
    .status-section {
      gap: var(--space-xs);
    }
    
    .memory-usage,
    .current-time {
      display: none;
    }
    
    .project-name {
      max-width: 120px;
    }
  }
  
  @media (max-width: 480px) {
    .status-right .status-item:not(.ai-status) {
      display: none;
    }
    
    .status-separator {
      display: none;
    }
  }
  
  /* High contrast mode */
  @media (prefers-contrast: high) {
    .status-bar {
      border-top-color: var(--text-primary);
    }
    
    .status-separator {
      background: var(--text-primary);
    }
  }
  
  /* Reduced motion */
  @media (prefers-reduced-motion: reduce) {
    .status-indicator,
    .processing-queue .status-icon {
      animation: none;
    }
  }
</style>
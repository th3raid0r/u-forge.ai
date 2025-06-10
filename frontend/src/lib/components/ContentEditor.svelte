<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';
  import { get } from 'svelte/store';
  import { invoke } from '@tauri-apps/api/tauri';
  import { uiStore, activeTab } from '../stores/uiStore';
  import { schemaStore, availableObjectTypes, isSchemaLoading, schemaError } from '../stores/schemaStore';
  import DynamicPropertyEditor from './DynamicPropertyEditor.svelte';
  

  import type { ProjectInfo, EditorTab, Object, ApiResponse } from '../types';
  
  export let currentProject: ProjectInfo | null = null;
  
  const dispatch = createEventDispatcher();
  
  let activeObject: Object | null = null;
  let isLoading = false;
  let error: string | null = null;
  let isDirty = false;
  let saveTimeout: ReturnType<typeof setTimeout>;
  let validationErrors: Array<{ property: string; message: string }> = [];
  
  // Editor state
  let objectName = '';
  let objectType = '';
  let objectDescription = '';
  let objectTags: string[] = [];
  let objectProperties: Record<string, any> = {};
  let newTag = '';
  
  // Component state
  let showAdvancedOptions = false;
  
  // Initialize when component mounts
  onMount(() => {
    console.log('🎯 [ContentEditor] Initializing...');
    
    // Set default object type from available types
    const unsubscribe = availableObjectTypes.subscribe(types => {
      console.log(`🎯 [ContentEditor] Available object types changed:`, types);
      if (types.length > 0 && !objectType) {
        console.log(`🎯 [ContentEditor] Setting default object type: ${types[0].value}`);
        objectType = types[0].value;
        loadDefaultPropertiesForType(objectType);
      }
    });
    
    // Subscribe to active tab changes
    const tabUnsubscribe = activeTab.subscribe(tab => {
      console.log(`🎯 [ContentEditor] Active tab changed:`, tab);
      if (tab && tab.object_id) {
        loadObject(tab.object_id);
      } else {
        resetEditor();
      }
    });
    
    console.log('✅ [ContentEditor] Initialization complete');
    return () => {
      unsubscribe();
      tabUnsubscribe();
    };
  });
  
  // Watch for object type changes
  $: if (objectType) {
    loadDefaultPropertiesForType(objectType);
  }
  
  async function loadDefaultPropertiesForType(type: string) {
    console.log(`🏗️ [ContentEditor] Loading default properties for type: ${type}`);
    if (!type || activeObject) {
      console.log(`🏗️ [ContentEditor] Skipping default properties - type: ${type}, activeObject: ${!!activeObject}`);
      return; // Don't override properties when editing existing object
    }
    
    try {
      console.log(`🏗️ [ContentEditor] Calling schemaStore.createDefaultProperties for: ${type}`);
      const defaultProps = await schemaStore.createDefaultProperties(type);
      console.log(`🏗️ [ContentEditor] Got default properties:`, defaultProps);
      objectProperties = { ...defaultProps };
    } catch (err) {
      console.warn('⚠️ [ContentEditor] Failed to load default properties for type:', type, err);
      objectProperties = {};
    }
  }
  
  async function loadObject(objectId: string) {
    console.log(`📖 [ContentEditor] Loading object: ${objectId}`);
    isLoading = true;
    error = null;
    
    try {
      const response: ApiResponse<Object> = await invoke('get_object_details', { objectId });
      console.log(`📖 [ContentEditor] get_object_details response:`, response);
      
      if (response.success && response.data) {
        activeObject = response.data;
        populateEditor(activeObject);
        console.log(`✅ [ContentEditor] Object loaded successfully: ${activeObject.name}`);
      } else {
        throw new Error(response.error || 'Failed to load object');
      }
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load object';
      console.error('❌ [ContentEditor] Failed to load object:', err);
    } finally {
      isLoading = false;
    }
  }
  
  function populateEditor(obj: Object) {
    objectName = obj.name;
    objectType = obj.object_type.toString();
    objectDescription = obj.description || '';
    objectTags = [...obj.tags];
    objectProperties = { ...obj.properties };
    isDirty = false;
    validationErrors = [];
  }
  
  function resetEditor() {
    console.log('🔄 [ContentEditor] Resetting editor');
    activeObject = null;
    objectName = '';
    objectDescription = '';
    objectTags = [];
    objectProperties = {};
    validationErrors = [];
    error = null;
    isDirty = false;
    
    // Set default object type if available
    const types = get(availableObjectTypes);
    console.log(`🔄 [ContentEditor] Available types for reset:`, types);
    if (types.length > 0) {
      console.log(`🔄 [ContentEditor] Setting default type to: ${types[0].value}`);
      objectType = types[0].value;
      loadDefaultPropertiesForType(objectType);
    }
  }
  
  function markDirty() {
    if (!isDirty) {
      isDirty = true;
      
      // Update tab dirty state
      const currentTab = $activeTab;
      if (currentTab) {
        uiStore.setTabDirty(currentTab.id, true);
      }
    }
    
    // Auto-save after 2 seconds of inactivity
    if (saveTimeout) clearTimeout(saveTimeout);
    saveTimeout = setTimeout(saveObject, 2000);
  }
  
  async function saveObject() {
    if (!isDirty || !currentProject) return;
    
    // Validate before saving
    if (objectType) {
      const validation = await schemaStore.validateObjectProperties(objectType, objectProperties);
      if (!validation.valid) {
        validationErrors = validation.errors;
        error = 'Please fix validation errors before saving';
        return;
      }
    }
    
    try {
      if (activeObject) {
        // Update existing object
        await updateObject();
      } else {
        // Create new object
        await createObject();
      }
      
      isDirty = false;
      validationErrors = [];
      error = null;
      
      const currentTab = $activeTab;
      if (currentTab) {
        uiStore.setTabDirty(currentTab.id, false);
      }
      
      // Dispatch save event
      dispatch('objectSaved', { 
        object: activeObject, 
        isNew: !activeObject 
      });
      
    } catch (err) {
      console.error('Failed to save object:', err);
      error = err instanceof Error ? err.message : 'Failed to save object';
    }
  }
  
  async function createObject() {
    const response: ApiResponse<string> = await invoke('create_object', {
      request: {
        name: objectName,
        object_type: objectType,
        description: objectDescription,
        properties: objectProperties,
        tags: objectTags,
      }
    });
    
    if (!response.success) {
      throw new Error(response.error || 'Failed to create object');
    }
    
    // Update tab with new object ID
    const currentTab = $activeTab;
    if (currentTab) {
      uiStore.updateTab(currentTab.id, {
        object_id: response.data,
        title: objectName || 'Untitled',
      });
    }
    
    // Load the newly created object
    await loadObject(response.data!);
  }
  
  async function updateObject() {
    if (!activeObject) return;
    
    // For now, we'll recreate the object since we don't have an update endpoint
    // In a real implementation, you'd want a proper update endpoint
    console.log('Update object functionality would be implemented here');
    // This would call something like:
    // const response = await invoke('update_object', { object_id: activeObject.id, updates: { ... } });
  }
  
  function handleObjectTypeChange() {
    console.log(`🔄 [ContentEditor] Object type changed to: ${objectType}`);
    // Only load default properties if this is a new object
    if (!activeObject) {
      console.log(`🔄 [ContentEditor] Loading default properties for new object type: ${objectType}`);
      loadDefaultPropertiesForType(objectType);
    } else {
      console.log(`🔄 [ContentEditor] Skipping default properties load for existing object`);
    }
    markDirty();
  }
  
  function handlePropertiesChange(event: CustomEvent) {
    objectProperties = event.detail.properties;
    markDirty();
  }
  
  function handlePropertiesValidation(event: CustomEvent) {
    validationErrors = event.detail.errors;
  }
  
  function addTag() {
    if (newTag.trim() && !objectTags.includes(newTag.trim())) {
      objectTags = [...objectTags, newTag.trim()];
      newTag = '';
      markDirty();
    }
  }
  
  function removeTag(tag: string) {
    objectTags = objectTags.filter(t => t !== tag);
    markDirty();
  }
  
  function handleTagKeydown(event: KeyboardEvent) {
    if (event.key === 'Enter') {
      event.preventDefault();
      addTag();
    }
  }
  
  async function createNewObject() {
    const newTab: EditorTab = {
      id: `new-${Date.now()}`,
      title: 'New Object',
      content_type: 'object',
      dirty: false,
      active: true,
    };
    
    uiStore.addTab(newTab);
    uiStore.setActiveTab(newTab.id);
  }
  
  async function saveManually() {
    clearTimeout(saveTimeout);
    await saveObject();
  }
  
  function handleKeydown(event: KeyboardEvent) {
    const isMac = navigator.platform.toUpperCase().indexOf('MAC') >= 0;
    const cmdOrCtrl = isMac ? event.metaKey : event.ctrlKey;
    
    if (cmdOrCtrl && event.key === 's') {
      event.preventDefault();
      saveManually();
    }
  }
</script>

<svelte:window on:keydown={handleKeydown} />

<div class="content-editor">
  {#if $isSchemaLoading}
    <div class="loading-state">
      <div class="loading-spinner"></div>
      <span>Loading schemas...</span>
    </div>
  {:else if $schemaError}
    <div class="error-state">
      <div class="error-icon">⚠️</div>
      <h3>Schema Error</h3>
      <p>{$schemaError}</p>
      <button class="btn primary" on:click={() => schemaStore.retry()}>
        Retry
      </button>
    </div>
  {:else if isLoading}
    <div class="loading-state">
      <div class="loading-spinner"></div>
      <span>Loading object...</span>
    </div>
  {:else}
    <!-- Editor Header -->
    <div class="editor-header">
      <div class="editor-title">
        <h2>{activeObject ? 'Edit Object' : 'Create New Object'}</h2>
        {#if isDirty}
          <span class="dirty-indicator" title="Unsaved changes">●</span>
        {/if}
      </div>
      
      <div class="editor-actions">
        <button 
          class="btn secondary"
          on:click={() => showAdvancedOptions = !showAdvancedOptions}
        >
          {showAdvancedOptions ? 'Hide' : 'Show'} Advanced
        </button>
        
        <button 
          class="btn primary" 
          disabled={!isDirty || validationErrors.length > 0}
          on:click={saveManually}
        >
          {activeObject ? 'Update' : 'Create'}
        </button>
      </div>
    </div>
    
    <!-- Error Display -->
    {#if error}
      <div class="error-banner">
        <span class="error-icon">⚠️</span>
        <span>{error}</span>
        <button class="btn-close" on:click={() => error = null}>✕</button>
      </div>
    {/if}
    
    <!-- Validation Errors -->
    {#if validationErrors.length > 0}
      <div class="validation-banner">
        <div class="validation-header">
          <span class="validation-icon">🔍</span>
          <span>Please fix the following issues:</span>
        </div>
        <ul class="validation-list">
          {#each validationErrors as error}
            <li>{error.property}: {error.message}</li>
          {/each}
        </ul>
      </div>
    {/if}
    
    <!-- Editor Content -->
    <div class="editor-content">
      <!-- Basic Information Panel -->
      <div class="editor-panel">
        <h3 class="panel-title">Basic Information</h3>
        
        <div class="form-group">
          <label for="object-name">Name *</label>
          <input
            id="object-name"
            type="text"
            bind:value={objectName}
            placeholder="Enter object name..."
            class="form-input"
            class:error={!objectName.trim()}
            on:input={markDirty}
          />
        </div>
        
        <div class="form-group">
          <label for="object-type">Type *</label>
          <select
            id="object-type"
            bind:value={objectType}
            class="form-select"
            on:change={handleObjectTypeChange}
            disabled={!!activeObject} 
          >
            <option value="">Select object type...</option>
            {#each $availableObjectTypes as type}
              <option value={type.value}>
                {type.icon} {type.label}
              </option>
            {/each}
          </select>
          {#if activeObject}
            <small class="help-text">Object type cannot be changed after creation</small>
          {/if}
        </div>
        
        <div class="form-group">
          <label for="object-description">Description</label>
          <textarea
            id="object-description"
            bind:value={objectDescription}
            placeholder="Enter object description..."
            class="form-textarea"
            rows="3"
            on:input={markDirty}
          ></textarea>
        </div>
        
        <!-- Tags -->
        <div class="form-group">
          <label>Tags</label>
          <div class="tags-container">
            <div class="tags-list">
              {#each objectTags as tag}
                <span class="tag">
                  {tag}
                  <button 
                    class="tag-remove" 
                    on:click={() => removeTag(tag)}
                    type="button"
                    title="Remove tag"
                  >
                    ✕
                  </button>
                </span>
              {/each}
            </div>
            
            <div class="tag-input-container">
              <input
                type="text"
                bind:value={newTag}
                placeholder="Add tag..."
                class="tag-input"
                on:keydown={handleTagKeydown}
              />
              <button 
                class="btn-add-tag" 
                on:click={addTag}
                type="button"
                disabled={!newTag.trim()}
              >
                Add
              </button>
            </div>
          </div>
        </div>
      </div>
      
      <!-- Properties Panel -->
      <div class="editor-panel">
        <h3 class="panel-title">Properties</h3>
        
        {#if objectType}
          <DynamicPropertyEditor
            {objectType}
            bind:properties={objectProperties}
            readonly={false}
            compact={false}
            on:change={handlePropertiesChange}
            on:validate={handlePropertiesValidation}
          />
        {:else}
          <div class="no-type-message">
            <p>Please select an object type to configure properties.</p>
          </div>
        {/if}
      </div>
      
      <!-- Advanced Options Panel -->
      {#if showAdvancedOptions}
        <div class="editor-panel">
          <h3 class="panel-title">Advanced Options</h3>
          
          <div class="form-group">
            <label>Object ID</label>
            <input
              type="text"
              value={activeObject?.id || 'Will be generated'}
              readonly
              class="form-input readonly"
            />
          </div>
          
          <div class="form-group">
            <label>Created</label>
            <input
              type="text"
              value={activeObject?.created_at || 'Not yet created'}
              readonly
              class="form-input readonly"
            />
          </div>
          
          <div class="form-group">
            <label>Last Modified</label>
            <input
              type="text"
              value={activeObject?.updated_at || 'Not yet created'}
              readonly
              class="form-input readonly"
            />
          </div>
        </div>
      {/if}
    </div>
    
    <!-- Footer -->
    <div class="editor-footer">
      <div class="footer-info">
        {#if isDirty}
          <span class="auto-save-info">Auto-saving in progress...</span>
        {:else}
          <span class="save-status">All changes saved</span>
        {/if}
      </div>
      
      <div class="footer-actions">
        <button class="btn secondary" on:click={createNewObject}>
          New Object
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .content-editor {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg-primary);
    overflow: hidden;
  }
  
  /* Loading and error states */
  .loading-state,
  .error-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    gap: var(--space-md);
    color: var(--text-secondary);
  }
  
  .loading-spinner {
    width: 32px;
    height: 32px;
    border: 3px solid var(--border-color);
    border-top: 3px solid var(--accent-color);
    border-radius: 50%;
    animation: spin 1s linear infinite;
  }
  
  .error-state {
    color: var(--error-color);
  }
  
  .error-icon {
    font-size: 2rem;
  }
  
  /* Editor Header */
  .editor-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-md);
    background: var(--bg-secondary);
    border-bottom: 1px solid var(--border-color);
  }
  
  .editor-title {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }
  
  .editor-title h2 {
    margin: 0;
    font-size: var(--font-lg);
    font-weight: 600;
    color: var(--text-primary);
  }
  
  .dirty-indicator {
    color: var(--warning-color);
    font-size: var(--font-lg);
    line-height: 1;
  }
  
  .editor-actions {
    display: flex;
    gap: var(--space-sm);
  }
  
  /* Error and validation banners */
  .error-banner,
  .validation-banner {
    display: flex;
    align-items: flex-start;
    gap: var(--space-sm);
    padding: var(--space-md);
    margin: 0 var(--space-md);
    border-radius: var(--radius-sm);
    font-size: var(--font-sm);
  }
  
  .error-banner {
    background: var(--error-color-alpha);
    border: 1px solid var(--error-color);
    color: var(--error-color);
  }
  
  .validation-banner {
    background: var(--warning-color-alpha);
    border: 1px solid var(--warning-color);
    color: var(--warning-color);
    flex-direction: column;
    align-items: stretch;
  }
  
  .validation-header {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    font-weight: 600;
  }
  
  .validation-list {
    margin: var(--space-sm) 0 0 0;
    padding-left: var(--space-lg);
  }
  
  .validation-list li {
    margin-bottom: var(--space-xs);
  }
  
  .btn-close {
    background: none;
    border: none;
    color: inherit;
    cursor: pointer;
    font-size: var(--font-sm);
    margin-left: auto;
  }
  
  /* Editor Content */
  .editor-content {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-md);
    display: flex;
    flex-direction: column;
    gap: var(--space-lg);
  }
  
  /* Editor Panels */
  .editor-panel {
    background: var(--bg-secondary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius-md);
    padding: var(--space-md);
  }
  
  .panel-title {
    margin: 0 0 var(--space-md) 0;
    font-size: var(--font-md);
    font-weight: 600;
    color: var(--text-primary);
    border-bottom: 1px solid var(--border-color);
    padding-bottom: var(--space-sm);
  }
  
  /* Form Elements */
  .form-group {
    margin-bottom: var(--space-md);
  }
  
  .form-group:last-child {
    margin-bottom: 0;
  }
  
  .form-group label {
    display: block;
    font-size: var(--font-sm);
    font-weight: 500;
    color: var(--text-primary);
    margin-bottom: var(--space-xs);
  }
  
  .form-input,
  .form-select,
  .form-textarea {
    width: 100%;
    background: var(--bg-tertiary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    padding: var(--space-sm);
    font-size: var(--font-sm);
    transition: border-color var(--transition-fast), box-shadow var(--transition-fast);
  }
  
  .form-input:focus,
  .form-select:focus,
  .form-textarea:focus {
    outline: none;
    border-color: var(--accent-color);
    box-shadow: 0 0 0 2px var(--accent-color-alpha);
  }
  
  .form-input.error {
    border-color: var(--error-color);
  }
  
  .form-input.readonly {
    background: var(--bg-disabled);
    color: var(--text-disabled);
    cursor: not-allowed;
  }
  
  .form-textarea {
    resize: vertical;
    min-height: 80px;
  }
  
  .help-text {
    display: block;
    font-size: var(--font-xs);
    color: var(--text-secondary);
    margin-top: var(--space-xs);
    font-style: italic;
  }
  
  /* Tags */
  .tags-container {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
  }
  
  .tags-list {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-xs);
    min-height: 32px;
  }
  
  .tag {
    display: inline-flex;
    align-items: center;
    gap: var(--space-xs);
    background: var(--accent-color-alpha);
    color: var(--accent-color);
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    font-size: var(--font-xs);
    border: 1px solid var(--accent-color);
  }
  
  .tag-remove {
    background: none;
    border: none;
    color: inherit;
    cursor: pointer;
    font-size: var(--font-xs);
    line-height: 1;
    padding: 0;
    width: 14px;
    height: 14px;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  
  .tag-input-container {
    display: flex;
    gap: var(--space-sm);
  }
  
  .tag-input {
    flex: 1;
    background: var(--bg-tertiary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    padding: var(--space-sm);
    font-size: var(--font-sm);
  }
  
  .btn-add-tag {
    background: var(--accent-color);
    border: none;
    border-radius: var(--radius-sm);
    color: white;
    padding: var(--space-sm) var(--space-md);
    font-size: var(--font-sm);
    cursor: pointer;
    transition: background-color var(--transition-fast);
  }
  
  .btn-add-tag:hover:not(:disabled) {
    background: var(--accent-color-dark);
  }
  
  .btn-add-tag:disabled {
    background: var(--bg-disabled);
    color: var(--text-disabled);
    cursor: not-allowed;
  }
  
  /* No type message */
  .no-type-message {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--space-lg);
    color: var(--text-secondary);
    font-style: italic;
  }
  
  /* Editor Footer */
  .editor-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-md);
    background: var(--bg-secondary);
    border-top: 1px solid var(--border-color);
  }
  
  .footer-info {
    font-size: var(--font-xs);
    color: var(--text-secondary);
  }
  
  .auto-save-info {
    color: var(--warning-color);
  }
  
  .save-status {
    color: var(--success-color);
  }
  
  .footer-actions {
    display: flex;
    gap: var(--space-sm);
  }
  
  /* Button styles */
  .btn {
    background: var(--accent-color);
    border: 1px solid var(--accent-color);
    border-radius: var(--radius-sm);
    color: white;
    padding: var(--space-sm) var(--space-md);
    font-size: var(--font-sm);
    font-weight: 500;
    cursor: pointer;
    transition: all var(--transition-fast);
    text-decoration: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-xs);
  }
  
  .btn:hover:not(:disabled) {
    background: var(--accent-color-dark);
    border-color: var(--accent-color-dark);
  }
  
  .btn:disabled {
    background: var(--bg-disabled);
    border-color: var(--border-color);
    color: var(--text-disabled);
    cursor: not-allowed;
  }
  
  .btn.secondary {
    background: var(--bg-tertiary);
    border-color: var(--border-color);
    color: var(--text-primary);
  }
  
  .btn.secondary:hover:not(:disabled) {
    background: var(--bg-secondary);
    border-color: var(--accent-color);
  }
  
  .btn.primary {
    background: var(--accent-color);
    border-color: var(--accent-color);
    color: white;
  }
  
  /* Animations */
  @keyframes spin {
    0% { transform: rotate(0deg); }
    100% { transform: rotate(360deg); }
  }
  
  /* Responsive design */
  @media (max-width: 768px) {
    .editor-header {
      flex-direction: column;
      gap: var(--space-md);
      align-items: stretch;
    }
    
    .editor-actions {
      justify-content: space-between;
    }
    
    .editor-content {
      padding: var(--space-sm);
    }
    
    .editor-panel {
      padding: var(--space-sm);
    }
    
    .editor-footer {
      flex-direction: column;
      gap: var(--space-sm);
      align-items: stretch;
    }
    
    .tags-list {
      min-height: auto;
    }
    
    .tag-input-container {
      flex-direction: column;
    }
  }
</style>
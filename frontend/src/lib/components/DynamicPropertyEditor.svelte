<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import type { DynamicObjectTypeSchema } from '../stores/schemaStore';
  import { schemaStore, isSchemaLoading, schemaError } from '../stores/schemaStore';
  
  export let objectType: string;
  export let properties: Record<string, any> = {};
  export let readonly: boolean = false;
  export let compact: boolean = false;
  
  const dispatch = createEventDispatcher<{
    change: { property: string; value: any; properties: Record<string, any> };
    validate: { valid: boolean; errors: Array<{ property: string; message: string }> };
  }>();
  
  let objectSchema: DynamicObjectTypeSchema | null = null;
  let loading = true;
  let error: string | null = null;
  let validationErrors: Record<string, string> = {};
  let validationWarnings: Record<string, string> = {};
  
  // Load schema on mount or when objectType changes, or when schema system becomes available
  $: if (objectType && !$isSchemaLoading && !$schemaError) {
    console.log(`🔧 [DynamicPropertyEditor] Schema conditions met, loading schema for: ${objectType}`);
    console.log(`   - isSchemaLoading: ${$isSchemaLoading}, schemaError: ${$schemaError}`);
    loadSchema(objectType);
  }
  
  // Update error state based on schema store error
  $: if ($schemaError) {
    console.log(`❌ [DynamicPropertyEditor] Schema store error detected: ${$schemaError}`);
    error = $schemaError;
    loading = false;
  }
  
  async function retrySchemaLoad() {
    console.log(`🔄 [DynamicPropertyEditor] Retrying schema load for: ${objectType}`);
    try {
      await schemaStore.retry();
      await loadSchema(objectType);
      console.log(`✅ [DynamicPropertyEditor] Schema retry successful`);
    } catch (err) {
      console.error('❌ [DynamicPropertyEditor] Retry failed:', err);
    }
  }
  
  async function loadSchema(type: string) {
    console.log(`📋 [DynamicPropertyEditor] Starting loadSchema for type: ${type}`);
    if (!type) {
      console.log(`❌ [DynamicPropertyEditor] No type provided, aborting load`);
      return;
    }
    
    loading = true;
    error = null;
    
    try {
      console.log(`📋 [DynamicPropertyEditor] Calling schemaStore.getObjectTypeSchema for: ${type}`);
      objectSchema = await schemaStore.getObjectTypeSchema(type);
      console.log(`📋 [DynamicPropertyEditor] Schema result:`, objectSchema);
      if (!objectSchema) {
        error = `Schema not found for object type: ${type}`;
        console.error(`❌ [DynamicPropertyEditor] Schema not found for type: ${type}`);
        return;
      }
      
      // Initialize missing properties with defaults
      const updatedProperties = { ...properties };
      let hasChanges = false;
      
      console.log(`📋 [DynamicPropertyEditor] Processing ${Object.keys(objectSchema.properties).length} properties`);
      Object.entries(objectSchema.properties).forEach(([key, propSchema]) => {
        if (!(key in updatedProperties)) {
          console.log(`📋 [DynamicPropertyEditor] Adding default value for property: ${key} = ${propSchema.default_value}`);
          updatedProperties[key] = propSchema.default_value;
          hasChanges = true;
        }
      });
      
      if (hasChanges) {
        console.log(`📋 [DynamicPropertyEditor] Updating properties with defaults:`, updatedProperties);
        properties = updatedProperties;
        dispatch('change', { 
          property: '', 
          value: null, 
          properties: updatedProperties 
        });
      }
      
      // Validate current properties
      console.log(`📋 [DynamicPropertyEditor] Validating properties for: ${type}`);
      await validateProperties();
      console.log(`✅ [DynamicPropertyEditor] Schema loaded successfully for: ${type}`);
    } catch (err) {
      error = err instanceof Error ? err.message : 'Failed to load schema';
      console.error('❌ [DynamicPropertyEditor] Error loading object schema:', err);
    } finally {
      loading = false;
    }
  }
  
  async function validateProperties() {
    if (!objectSchema) {
      console.log(`⚠️ [DynamicPropertyEditor] No schema available for validation`);
      return;
    }
    
    console.log(`🔍 [DynamicPropertyEditor] Validating properties for: ${objectType}`, properties);
    try {
      const result = await schemaStore.validateObjectProperties(objectType, properties);
      console.log(`🔍 [DynamicPropertyEditor] Validation result:`, result);
      
      validationErrors = {};
      validationWarnings = {};
      
      result.errors.forEach(error => {
        validationErrors[error.property] = error.message;
      });
      
      result.warnings.forEach(warning => {
        validationWarnings[warning.property] = warning.message;
      });
      
      dispatch('validate', {
        valid: result.valid,
        errors: result.errors,
      });
      
      console.log(`✅ [DynamicPropertyEditor] Validation complete - valid: ${result.valid}, errors: ${result.errors.length}, warnings: ${result.warnings.length}`);
    } catch (err) {
      console.error('❌ [DynamicPropertyEditor] Validation failed:', err);
    }
  }
  
  function handlePropertyChange(propertyName: string, value: any) {
    properties = { ...properties, [propertyName]: value };
    
    // Clear validation error for this property
    if (validationErrors[propertyName]) {
      validationErrors = { ...validationErrors };
      delete validationErrors[propertyName];
    }
    
    dispatch('change', { 
      property: propertyName, 
      value, 
      properties 
    });
    
    // Re-validate after a short delay
    setTimeout(validateProperties, 300);
  }
  

  
  function getPropertyValue(propertyName: string, defaultValue: any): any {
    return properties[propertyName] ?? defaultValue;
  }
  
  // Event value extraction helpers
  function getInputValue(event: Event): string {
    return (event.target as HTMLInputElement)?.value || '';
  }
  
  function getTextAreaValue(event: Event): string {
    return (event.target as HTMLTextAreaElement)?.value || '';
  }
  
  function getSelectValue(event: Event): string {
    return (event.target as HTMLSelectElement)?.value || '';
  }
  
  function getCheckboxValue(event: Event): boolean {
    return (event.target as HTMLInputElement)?.checked || false;
  }
  
  // Array manipulation functions
  
  function addArrayItem(propertyName: string) {
    const currentValue = getPropertyValue(propertyName, []);
    const newValue = Array.isArray(currentValue) ? [...currentValue, ''] : [''];
    handlePropertyChange(propertyName, newValue);
  }
  
  function removeArrayItem(propertyName: string, index: number) {
    const currentValue = getPropertyValue(propertyName, []);
    if (Array.isArray(currentValue)) {
      const newValue = currentValue.filter((_, i) => i !== index);
      handlePropertyChange(propertyName, newValue);
    }
  }
  
  function updateArrayItem(propertyName: string, index: number, newItemValue: any) {
    const currentValue = getPropertyValue(propertyName, []);
    if (Array.isArray(currentValue)) {
      const newValue = [...currentValue];
      newValue[index] = newItemValue;
      handlePropertyChange(propertyName, newValue);
    }
  }
</script>

<div class="dynamic-property-editor" class:compact class:readonly>
  {#if loading}
    <div class="loading-state">
      <div class="loading-spinner"></div>
      <span>Loading schema...</span>
    </div>
  {:else if error}
    <div class="error-state">
      <div class="error-icon">⚠️</div>
      <span>{error}</span>
      <button class="btn retry-btn" on:click={retrySchemaLoad}>
        Retry
      </button>
    </div>
  {:else if objectSchema}
    <div class="property-form">
      {#each Object.entries(objectSchema.properties) as [propertyName, propertySchema]}
        <div class="property-field" class:required={objectSchema.required_properties.includes(propertyName)}>
          <!-- Property Label -->
          <label for="prop-{propertyName}" class="property-label">
            {propertyName.charAt(0).toUpperCase() + propertyName.slice(1).replace(/_/g, ' ')}{#if objectSchema.required_properties.includes(propertyName)} *{/if}
            {#if propertySchema.description && !compact}
              <span class="property-description">{propertySchema.description}</span>
            {/if}
          </label>
          
          <!-- Property Input -->
          <div class="property-input-container">
            {#if propertySchema.ui_component === 'textarea' || propertySchema.property_type.toString() === 'Text'}
              <textarea
                id="prop-{propertyName}"
                value={getPropertyValue(propertyName, propertySchema.default_value || '')}
                placeholder={propertySchema.ui_options?.placeholder || propertySchema.description}
                rows={propertySchema.ui_options?.rows || (compact ? 2 : 4)}
                maxlength={propertySchema.validation?.max_length}
                {readonly}
                class="property-textarea"
                class:error={validationErrors[propertyName]}
                class:warning={validationWarnings[propertyName]}
                on:input={e => handlePropertyChange(propertyName, getTextAreaValue(e))}
              ></textarea>
              
            {:else if propertySchema.ui_component === 'number' || propertySchema.property_type.toString() === 'Number'}
              <input
                id="prop-{propertyName}"
                type="number"
                value={getPropertyValue(propertyName, propertySchema.default_value || 0)}
                min={propertySchema.validation?.min_value}
                max={propertySchema.validation?.max_value}
                {readonly}
                class="property-input"
                class:error={validationErrors[propertyName]}
                class:warning={validationWarnings[propertyName]}
                on:input={e => handlePropertyChange(propertyName, parseFloat(getInputValue(e)) || 0)}
              />
              
            {:else if propertySchema.ui_component === 'checkbox' || propertySchema.property_type.toString() === 'Boolean'}
              <div class="checkbox-container">
                <input
                  id="prop-{propertyName}"
                  type="checkbox"
                  checked={Boolean(getPropertyValue(propertyName, propertySchema.default_value || false))}
                  disabled={readonly}
                  class="property-checkbox"
                  class:error={validationErrors[propertyName]}
                  on:change={e => handlePropertyChange(propertyName, getCheckboxValue(e))}
                />
                <span class="checkbox-label">Enable</span>
              </div>
              
            {:else if propertySchema.ui_component === 'select' || (propertySchema.validation?.allowed_values && propertySchema.validation.allowed_values.length > 0)}
              <select
                id="prop-{propertyName}"
                value={getPropertyValue(propertyName, propertySchema.default_value)}
                disabled={readonly}
                class="property-select"
                class:error={validationErrors[propertyName]}
                class:warning={validationWarnings[propertyName]}
                on:change={e => handlePropertyChange(propertyName, getSelectValue(e))}
              >
                <option value="">Select...</option>
                {#each propertySchema.validation?.allowed_values || [] as option}
                  <option value={option}>{option}</option>
                {/each}
              </select>
              
            {:else if propertySchema.property_type.toString() === 'Array'}
              <div class="array-input">
                {#each getPropertyValue(propertyName, propertySchema.default_value || []) as item, index}
                  <div class="array-item">
                    <input
                      type="text"
                      value={item}
                      {readonly}
                      class="array-item-input"
                      placeholder="Item {index + 1}"
                      on:input={e => updateArrayItem(propertyName, index, getInputValue(e))}
                    />
                    {#if !readonly}
                      <button
                        type="button"
                        class="btn-remove-item"
                        title="Remove item"
                        on:click={() => removeArrayItem(propertyName, index)}
                      >
                        ✕
                      </button>
                    {/if}
                  </div>
                {/each}
                
                {#if !readonly}
                  <button
                    type="button"
                    class="btn-add-item"
                    on:click={() => addArrayItem(propertyName)}
                  >
                    + Add Item
                  </button>
                {/if}
              </div>
              
            {:else}
              <!-- Default string input -->
              <input
                id="prop-{propertyName}"
                type="text"
                value={getPropertyValue(propertyName, propertySchema.default_value || '')}
                placeholder={propertySchema.ui_options?.placeholder || propertySchema.description}
                maxlength={propertySchema.validation?.max_length}
                pattern={propertySchema.validation?.pattern}
                {readonly}
                class="property-input"
                class:error={validationErrors[propertyName]}
                class:warning={validationWarnings[propertyName]}
                on:input={e => handlePropertyChange(propertyName, getInputValue(e))}
              />
            {/if}
            
            <!-- Validation Messages -->
            {#if validationErrors[propertyName]}
              <div class="validation-message error">
                {validationErrors[propertyName]}
              </div>
            {:else if validationWarnings[propertyName]}
              <div class="validation-message warning">
                {validationWarnings[propertyName]}
              </div>
            {/if}
          </div>
        </div>
      {/each}
    </div>
  {:else}
    <div class="no-schema-state">
      <span>No schema available for object type: {objectType}</span>
    </div>
  {/if}
</div>

<style>
  .dynamic-property-editor {
    display: flex;
    flex-direction: column;
    gap: var(--space-md);
    padding: var(--space-md);
    background: var(--bg-primary);
    border-radius: var(--radius-md);
  }
  
  .dynamic-property-editor.compact {
    gap: var(--space-sm);
    padding: var(--space-sm);
  }
  
  .dynamic-property-editor.readonly {
    background: var(--bg-secondary);
  }
  
  /* Loading and error states */
  .loading-state,
  .error-state,
  .no-schema-state {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-sm);
    padding: var(--space-lg);
    color: var(--text-secondary);
    font-size: var(--font-sm);
  }
  
  .loading-spinner {
    width: 16px;
    height: 16px;
    border: 2px solid var(--border-color);
    border-top: 2px solid var(--accent-color);
    border-radius: 50%;
    animation: spin 1s linear infinite;
  }
  
  .error-state {
    color: var(--danger-color);
    gap: var(--space-sm);
  }
  
  .retry-btn {
    margin-top: var(--space-sm);
    padding: var(--space-xs) var(--space-sm);
    background: var(--accent-color);
    color: white;
    border: none;
    border-radius: var(--radius-sm);
    cursor: pointer;
    font-size: var(--font-sm);
  }
  
  .retry-btn:hover {
    background: var(--accent-color-dark);
  }
  
  .error-icon {
    font-size: 1.2em;
  }
  
  /* Property form */
  .property-form {
    display: flex;
    flex-direction: column;
    gap: var(--space-md);
  }
  
  .compact .property-form {
    gap: var(--space-sm);
  }
  
  /* Property field */
  .property-field {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }
  
  .property-field.required .property-label {
    font-weight: 600;
  }
  
  .property-label {
    font-size: var(--font-sm);
    font-weight: 500;
    color: var(--text-primary);
    margin-bottom: var(--space-xs);
  }
  
  .property-description {
    display: block;
    font-size: var(--font-xs);
    font-weight: 400;
    color: var(--text-secondary);
    margin-top: var(--space-xs);
  }
  
  /* Input container */
  .property-input-container {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
  }
  
  /* Base input styles */
  .property-input,
  .property-textarea,
  .property-select {
    background: var(--bg-tertiary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    padding: var(--space-sm);
    font-size: var(--font-sm);
    transition: border-color var(--transition-fast), box-shadow var(--transition-fast);
  }
  
  .property-input:focus,
  .property-textarea:focus,
  .property-select:focus {
    outline: none;
    border-color: var(--accent-color);
    box-shadow: 0 0 0 2px var(--accent-color-alpha);
  }
  
  .property-input.error,
  .property-textarea.error,
  .property-select.error {
    border-color: var(--error-color);
  }
  
  .property-input.warning,
  .property-textarea.warning,
  .property-select.warning {
    border-color: var(--warning-color);
  }
  
  .property-input[readonly],
  .property-textarea[readonly],
  .property-select[disabled] {
    background: var(--bg-disabled);
    color: var(--text-disabled);
    cursor: not-allowed;
  }
  
  /* Textarea specific */
  .property-textarea {
    resize: vertical;
    min-height: 80px;
  }
  
  .compact .property-textarea {
    min-height: 60px;
  }
  
  /* Checkbox styles */
  .checkbox-container {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
  }
  
  .property-checkbox {
    width: 18px;
    height: 18px;
    accent-color: var(--accent-color);
  }
  
  .checkbox-label {
    font-size: var(--font-sm);
    color: var(--text-primary);
    cursor: pointer;
  }
  
  /* Array input styles */
  .array-input {
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
  }
  
  .array-item {
    display: flex;
    gap: var(--space-sm);
    align-items: center;
  }
  
  .array-item-input {
    flex: 1;
    background: var(--bg-tertiary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    padding: var(--space-sm);
    font-size: var(--font-sm);
  }
  
  .btn-remove-item {
    background: var(--error-color);
    border: none;
    border-radius: var(--radius-sm);
    color: white;
    width: 24px;
    height: 24px;
    font-size: var(--font-xs);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background-color var(--transition-fast);
  }
  
  .btn-remove-item:hover {
    background: var(--error-color-dark);
  }
  
  .btn-add-item {
    background: var(--accent-color);
    border: none;
    border-radius: var(--radius-sm);
    color: white;
    padding: var(--space-sm) var(--space-md);
    font-size: var(--font-sm);
    cursor: pointer;
    transition: background-color var(--transition-fast);
    align-self: flex-start;
  }
  
  .btn-add-item:hover {
    background: var(--accent-color-dark);
  }
  
  /* Validation messages */
  .validation-message {
    font-size: var(--font-xs);
    padding: var(--space-xs) var(--space-sm);
    border-radius: var(--radius-sm);
    border-left: 3px solid;
  }
  
  .validation-message.error {
    color: var(--error-color);
    background: var(--error-color-alpha);
    border-left-color: var(--error-color);
  }
  
  .validation-message.warning {
    color: var(--warning-color);
    background: var(--warning-color-alpha);
    border-left-color: var(--warning-color);
  }
  
  /* Animation */
  @keyframes spin {
    0% { transform: rotate(0deg); }
    100% { transform: rotate(360deg); }
  }
  
  /* Responsive design */
  @media (max-width: 768px) {
    .dynamic-property-editor {
      padding: var(--space-sm);
    }
    
    .property-form {
      gap: var(--space-sm);
    }
    
    .array-item {
      flex-direction: column;
      align-items: stretch;
    }
    
    .btn-remove-item {
      align-self: flex-end;
    }
  }
</style>
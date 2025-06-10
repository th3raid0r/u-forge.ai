import { writable, derived, get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/tauri';
import type { 
  SchemaDefinition, 
  ObjectTypeSchema, 
  EdgeTypeSchema, 
  PropertySchema,
  ApiResponse 
} from '../types';
import { PropertyType } from '../types';

// Enhanced schema interfaces for dynamic schema management
export interface DynamicObjectTypeSchema extends ObjectTypeSchema {
  properties: Record<string, DynamicPropertySchema>;
}

export interface DynamicPropertySchema extends PropertySchema {
  property_type: PropertyType;
  default_value?: any;
  ui_component?: 'input' | 'textarea' | 'select' | 'checkbox' | 'number' | 'date' | 'file' | 'reference';
  ui_options?: {
    placeholder?: string;
    rows?: number;
    multiple?: boolean;
    accept?: string;
    reference_type?: string;
  };
}

export interface PropertyInfo {
  name: string;
  property_type: string;
  description: string;
  required: boolean;
  validation_rules?: {
    min_length?: number;
    max_length?: number;
    min_value?: number;
    max_value?: number;
    pattern?: string;
    allowed_values?: string[];
  };
}

export interface ObjectTypeSchemaResponse {
  name: string;
  description: string;
  properties: PropertyInfo[];
  required_properties: string[];
  allowed_edges: string[];
}

export interface SchemaResponse {
  name: string;
  version: string;
  description: string;
  object_types: string[];
  edge_types: string[];
}

// Schema state interface
interface SchemaState {
  // Available schemas
  availableSchemas: string[];
  currentSchemaName: string | null;
  currentSchema: SchemaDefinition | null;
  
  // Cached object type schemas
  objectTypeSchemas: Record<string, DynamicObjectTypeSchema>;
  
  // Cached edge type schemas
  edgeTypeSchemas: Record<string, EdgeTypeSchema>;
  
  // Available object types for current schema
  availableObjectTypes: string[];
  
  // Available edge types for current schema
  availableEdgeTypes: string[];
  
  // Loading states
  isLoading: boolean;
  isLoadingObjectType: boolean;
  
  // Error handling
  error: string | null;
  
  // Last updated timestamp
  lastUpdated: string | null;
}

// Initial state
const initialState: SchemaState = {
  availableSchemas: [],
  currentSchemaName: null,
  currentSchema: null,
  objectTypeSchemas: {},
  edgeTypeSchemas: {},
  availableObjectTypes: [],
  availableEdgeTypes: [],
  isLoading: false,
  isLoadingObjectType: false,
  error: null,
  lastUpdated: null,
};

// Create the main schema store
const createSchemaStore = () => {
  const { subscribe, set, update } = writable<SchemaState>(initialState);

  return {
    subscribe,

    // Initialize schema system
    initialize: async () => {
      console.log('🔄 [SchemaStore] Starting initialization...');
      update(state => ({ ...state, isLoading: true, error: null }));
      
      const maxRetries = 3;
      const retryDelay = 1000; // 1 second
      
      for (let attempt = 0; attempt < maxRetries; attempt++) {
        console.log(`🔄 [SchemaStore] Attempt ${attempt + 1}/${maxRetries}`);
        try {
          console.log('📋 [SchemaStore] Loading available schemas...');
          await schemaStore.loadAvailableSchemas();
          
          // Load default schema if available
          const state = get({ subscribe });
          console.log(`📋 [SchemaStore] Found ${state.availableSchemas.length} schemas:`, state.availableSchemas);
          
          if (state.availableSchemas.length > 0) {
            // Prefer imported_schemas if available, as it contains the actual dynamic types
            const preferredSchema = state.availableSchemas.find(s => s === 'imported_schemas') || 
                                  state.availableSchemas.find(s => s === 'default') || 
                                  state.availableSchemas[0];
            console.log(`📋 [SchemaStore] Loading schema: ${preferredSchema}`);
            await schemaStore.loadSchema(preferredSchema);
          } else if (attempt === maxRetries - 1) {
            // On final attempt, try to load schemas from directory
            console.log('📂 [SchemaStore] No schemas found, attempting to load from default directory...');
            try {
              const response = await invoke('load_schemas_from_directory', {
                schemaDir: './examples/schemas',
                schemaName: 'default',
                schemaVersion: '1.0.0'
              }) as ApiResponse<string>;
              console.log('📂 [SchemaStore] Load schemas response:', response);
              if (response.success) {
                // Retry loading after loading schemas
                console.log('📂 [SchemaStore] Retrying schema loading after directory load...');
                await schemaStore.loadAvailableSchemas();
                const newState = get({ subscribe });
                if (newState.availableSchemas.length > 0) {
                  // Prefer imported_schemas if available, as it contains the actual dynamic types
                  const preferredSchema = newState.availableSchemas.find(s => s === 'imported_schemas') || 
                                        newState.availableSchemas.find(s => s === 'default') || 
                                        newState.availableSchemas[0];
                  console.log(`📂 [SchemaStore] Loading schema after directory load: ${preferredSchema}`);
                  await schemaStore.loadSchema(preferredSchema);
                }
              }
            } catch (loadError) {
              console.warn('⚠️ [SchemaStore] Failed to load schemas from directory:', loadError);
            }
          }
          
          const finalState = get({ subscribe });
          console.log('✅ [SchemaStore] Initialization successful!');
          console.log(`   - Current schema: ${finalState.currentSchemaName}`);
          console.log(`   - Available object types: ${finalState.availableObjectTypes.join(', ')}`);
          console.log(`   - Available edge types: ${finalState.availableEdgeTypes.join(', ')}`);
          
          update(state => ({ ...state, isLoading: false, lastUpdated: new Date().toISOString() }));
          return; // Success, exit retry loop
        } catch (error) {
          console.error(`❌ [SchemaStore] Attempt ${attempt + 1} failed:`, error);
          
          if (attempt < maxRetries - 1) {
            console.log(`⏳ [SchemaStore] Waiting ${retryDelay}ms before retry...`);
            // Wait before retrying
            await new Promise(resolve => setTimeout(resolve, retryDelay));
            continue;
          }
          
          // Final attempt failed
          const errorMessage = error instanceof Error ? error.message : 'Failed to initialize schema system';
          console.error(`💥 [SchemaStore] All attempts failed: ${errorMessage}`);
          update(state => ({ 
            ...state, 
            isLoading: false, 
            error: `${errorMessage} (after ${maxRetries} attempts)` 
          }));
          throw error;
        }
      }
    },

    // Load available schemas
    loadAvailableSchemas: async () => {
      try {
        console.log('🔍 [SchemaStore] Calling list_schemas...');
        const response: ApiResponse<{ schemas: string[] }> = await invoke('list_schemas');
        console.log('🔍 [SchemaStore] list_schemas response:', response);
        
        if (response.success && response.data) {
          console.log(`🔍 [SchemaStore] Found schemas: ${response.data.schemas.join(', ')}`);
          update(state => ({
            ...state,
            availableSchemas: response.data!.schemas,
            error: null,
          }));
        } else {
          throw new Error(response.error || 'Failed to load available schemas');
        }
      } catch (error) {
        console.error('❌ [SchemaStore] loadAvailableSchemas failed:', error);
        const errorMessage = error instanceof Error ? error.message : 'Failed to load schemas';
        update(state => ({ ...state, error: errorMessage }));
        throw error;
      }
    },

    // Load a specific schema
    loadSchema: async (schemaName: string) => {
      console.log(`📖 [SchemaStore] Loading schema: ${schemaName}`);
      update(state => ({ ...state, isLoading: true, error: null }));
      
      try {
        console.log(`📖 [SchemaStore] Calling get_schema for: ${schemaName}`);
        const response: ApiResponse<SchemaResponse> = await invoke('get_schema', { schemaName: schemaName });
        console.log(`📖 [SchemaStore] get_schema response:`, response);
        
        if (response.success && response.data) {
          const schemaData = response.data;
          console.log(`📖 [SchemaStore] Schema data:`, schemaData);
          
          // Create a basic schema definition (we'll load detailed object types on demand)
          const schemaDefinition: SchemaDefinition = {
            name: schemaData.name,
            version: schemaData.version,
            description: schemaData.description,
            object_types: {},  // Will be populated on demand
            edge_types: {},    // Will be populated on demand
          };
          
          console.log(`📖 [SchemaStore] Setting up schema with ${schemaData.object_types.length} object types and ${schemaData.edge_types.length} edge types`);
          
          update(state => ({
            ...state,
            currentSchemaName: schemaName,
            currentSchema: schemaDefinition,
            availableObjectTypes: schemaData.object_types,
            availableEdgeTypes: schemaData.edge_types,
            isLoading: false,
            lastUpdated: new Date().toISOString(),
          }));
        } else {
          throw new Error(response.error || 'Failed to load schema');
        }
      } catch (error) {
        console.error(`❌ [SchemaStore] loadSchema failed for ${schemaName}:`, error);
        const errorMessage = error instanceof Error ? error.message : 'Failed to load schema';
        update(state => ({ ...state, isLoading: false, error: errorMessage }));
        throw error;
      }
    },

    // Load detailed object type schema
    loadObjectTypeSchema: async (objectType: string): Promise<DynamicObjectTypeSchema> => {
      const currentState = get({ subscribe });
      
      // Return cached version if available
      if (currentState.objectTypeSchemas[objectType]) {
        console.log(`📝 [SchemaStore] Using cached schema for: ${objectType}`);
        return currentState.objectTypeSchemas[objectType];
      }
      
      if (!currentState.currentSchemaName) {
        console.error('❌ [SchemaStore] No schema loaded when trying to get object type schema');
        throw new Error('No schema loaded');
      }
      
      console.log(`📝 [SchemaStore] Loading object type schema for: ${objectType} (schema: ${currentState.currentSchemaName})`);
      update(state => ({ ...state, isLoadingObjectType: true, error: null }));
      
      try {
        const response: ApiResponse<ObjectTypeSchemaResponse> = await invoke('get_object_type_schema', {
          schemaName: currentState.currentSchemaName,
          objectType: objectType,
        });
        console.log(`📝 [SchemaStore] get_object_type_schema response for ${objectType}:`, response);
        
        if (response.success && response.data) {
          const schemaData = response.data;
          console.log(`📝 [SchemaStore] Processing ${schemaData.properties.length} properties for ${objectType}`);
          
          // Convert PropertyInfo[] to DynamicPropertySchema format
          const properties: Record<string, DynamicPropertySchema> = {};
          schemaData.properties.forEach(prop => {
            properties[prop.name] = {
              property_type: stringToPropertyType(prop.property_type),
              description: prop.description,
              required: prop.required,
              validation: prop.validation_rules ? {
                min_length: prop.validation_rules.min_length,
                max_length: prop.validation_rules.max_length,
                min_value: prop.validation_rules.min_value,
                max_value: prop.validation_rules.max_value,
                pattern: prop.validation_rules.pattern,
                allowed_values: prop.validation_rules.allowed_values,
              } : undefined,
              ui_component: inferUIComponent(prop.property_type, prop.validation_rules),
              default_value: getDefaultValue(prop.property_type),
            };
          });
          
          const objectTypeSchema: DynamicObjectTypeSchema = {
            name: schemaData.name,
            description: schemaData.description,
            properties,
            required_properties: schemaData.required_properties,
            icon: getObjectTypeIcon(objectType),
            color: getObjectTypeColor(objectType),
          };
          
          console.log(`✅ [SchemaStore] Successfully loaded schema for ${objectType}:`, objectTypeSchema);
          
          // Cache the schema
          update(state => ({
            ...state,
            objectTypeSchemas: {
              ...state.objectTypeSchemas,
              [objectType]: objectTypeSchema,
            },
            isLoadingObjectType: false,
          }));
          
          return objectTypeSchema;
        } else {
          throw new Error(response.error || 'Failed to load object type schema');
        }
      } catch (error) {
        console.error(`❌ [SchemaStore] loadObjectTypeSchema failed for ${objectType}:`, error);
        const errorMessage = error instanceof Error ? error.message : 'Failed to load object type schema';
        update(state => ({ ...state, isLoadingObjectType: false, error: errorMessage }));
        throw error;
      }
    },

    // Get object type schema (cached or load)
    getObjectTypeSchema: async (objectType: string): Promise<DynamicObjectTypeSchema | null> => {
      const currentState = get({ subscribe });
      
      if (currentState.objectTypeSchemas[objectType]) {
        return currentState.objectTypeSchemas[objectType];
      }
      
      try {
        return await schemaStore.loadObjectTypeSchema(objectType);
      } catch (error) {
        console.error(`Failed to load object type schema for ${objectType}:`, error);
        return null;
      }
    },

    // Create default properties for an object type
    createDefaultProperties: async (objectType: string): Promise<Record<string, any>> => {
      try {
        const schema = await schemaStore.getObjectTypeSchema(objectType);
        if (!schema) {
          return {};
        }
        
        const defaultProperties: Record<string, any> = {};
        
        Object.entries(schema.properties).forEach(([key, propertySchema]) => {
          defaultProperties[key] = propertySchema.default_value;
        });
        
        return defaultProperties;
      } catch (error) {
        console.error(`Failed to create default properties for ${objectType}:`, error);
        return {};
      }
    },

    // Validate object properties against schema
    validateObjectProperties: async (objectType: string, properties: Record<string, any>): Promise<{
      valid: boolean;
      errors: Array<{ property: string; message: string }>;
      warnings: Array<{ property: string; message: string }>;
    }> => {
      try {
        const schema = await schemaStore.getObjectTypeSchema(objectType);
        if (!schema) {
          return { valid: false, errors: [{ property: 'schema', message: 'Object type schema not found' }], warnings: [] };
        }
        
        const errors: Array<{ property: string; message: string }> = [];
        const warnings: Array<{ property: string; message: string }> = [];
        
        // Check required properties
        schema.required_properties.forEach(requiredProp => {
          if (!(requiredProp in properties) || properties[requiredProp] == null || properties[requiredProp] === '') {
            errors.push({ property: requiredProp, message: 'This field is required' });
          }
        });
        
        // Validate individual properties
        Object.entries(properties).forEach(([key, value]) => {
          const propertySchema = schema.properties[key];
          if (!propertySchema) {
            warnings.push({ property: key, message: 'Unknown property' });
            return;
          }
          
          // Type validation
          const typeValid = validatePropertyType(value, propertySchema.property_type);
          if (!typeValid) {
            errors.push({ property: key, message: `Invalid type. Expected ${propertySchema.property_type}` });
            return;
          }
          
          // Validation rules
          if (propertySchema.validation) {
            const validation = propertySchema.validation;
            
            if (typeof value === 'string') {
              if (validation.min_length && value.length < validation.min_length) {
                errors.push({ property: key, message: `Minimum length is ${validation.min_length}` });
              }
              if (validation.max_length && value.length > validation.max_length) {
                errors.push({ property: key, message: `Maximum length is ${validation.max_length}` });
              }
              if (validation.pattern && !new RegExp(validation.pattern).test(value)) {
                errors.push({ property: key, message: 'Value does not match required pattern' });
              }
            }
            
            if (typeof value === 'number') {
              if (validation.min_value !== undefined && value < validation.min_value) {
                errors.push({ property: key, message: `Minimum value is ${validation.min_value}` });
              }
              if (validation.max_value !== undefined && value > validation.max_value) {
                errors.push({ property: key, message: `Maximum value is ${validation.max_value}` });
              }
            }
            
            if (validation.allowed_values && !validation.allowed_values.includes(String(value))) {
              errors.push({ property: key, message: `Value must be one of: ${validation.allowed_values.join(', ')}` });
            }
          }
        });
        
        return {
          valid: errors.length === 0,
          errors,
          warnings,
        };
      } catch (error) {
        return {
          valid: false,
          errors: [{ property: 'validation', message: 'Failed to validate properties' }],
          warnings: [],
        };
      }
    },

    // Clear cache and reload
    refresh: async () => {
      update(state => ({
        ...state,
        objectTypeSchemas: {},
        edgeTypeSchemas: {},
        error: null,
      }));
      
      await schemaStore.loadAvailableSchemas();
      
      const currentState = get({ subscribe });
      if (currentState.currentSchemaName) {
        await schemaStore.loadSchema(currentState.currentSchemaName);
      }
    },

    // Reset store to initial state
    reset: () => {
      set(initialState);
    },

    // Set error state
    setError: (error: string | null) => {
      update(state => ({ ...state, error }));
    },

    // Clear error state
    clearError: () => {
      update(state => ({ ...state, error: null }));
    },

    // Retry initialization (for manual retry from UI)
    retry: async () => {
      await schemaStore.initialize();
    },
  };
};

// Helper functions
function stringToPropertyType(typeString: string): PropertyType {
  switch (typeString.toLowerCase()) {
    case 'string': return PropertyType.String;
    case 'text': return PropertyType.Text;
    case 'number': return PropertyType.Number;
    case 'boolean': return PropertyType.Boolean;
    case 'array': return PropertyType.Array;
    case 'object': return PropertyType.Object;
    case 'reference': return PropertyType.Reference;
    case 'enum': return PropertyType.Enum;
    default: return PropertyType.String;
  }
}

function inferUIComponent(propertyType: string, validation?: any): 'input' | 'textarea' | 'select' | 'checkbox' | 'number' | 'date' | 'file' | 'reference' {
  switch (propertyType.toLowerCase()) {
    case 'text': return 'textarea';
    case 'number': return 'number';
    case 'boolean': return 'checkbox';
    case 'reference': return 'reference';
    case 'enum': return 'select';
    default:
      if (validation?.allowed_values) {
        return 'select';
      }
      return 'input';
  }
}

function getDefaultValue(propertyType: string): any {
  switch (propertyType.toLowerCase()) {
    case 'string':
    case 'text': return '';
    case 'number': return 0;
    case 'boolean': return false;
    case 'array': return [];
    case 'object': return {};
    case 'reference': return null;
    case 'enum': return null;
    default: return null;
  }
}

function validatePropertyType(value: any, propertyType: PropertyType): boolean {
  switch (propertyType) {
    case PropertyType.String:
    case PropertyType.Text:
    case PropertyType.Enum:
      return typeof value === 'string';
    case PropertyType.Number:
      return typeof value === 'number' && !isNaN(value);
    case PropertyType.Boolean:
      return typeof value === 'boolean';
    case PropertyType.Array:
      return Array.isArray(value);
    case PropertyType.Object:
      return typeof value === 'object' && value !== null && !Array.isArray(value);
    case PropertyType.Reference:
      return typeof value === 'string' || value === null;
    default:
      return true;
  }
}

function getObjectTypeIcon(objectType: string): string {
  const iconMap: Record<string, string> = {
    character: '👤',
    location: '🗺️',
    faction: '⚔️',
    item: '🎒',
    event: '📅',
    session: '🎲',
    spell: '✨',
    class: '🛡️',
    weapon: '⚔️',
    armor: '🛡️',
    vehicle: '🚗',
    building: '🏛️',
    organization: '🏢',
    creature: '🐉',
    custom: '📄',
  };
  
  return iconMap[objectType.toLowerCase()] || '📄';
}

function getObjectTypeColor(objectType: string): string {
  const colorMap: Record<string, string> = {
    character: '#4f46e5',
    location: '#059669',
    faction: '#dc2626',
    item: '#d97706',
    event: '#7c2d12',
    session: '#7c3aed',
    spell: '#ec4899',
    class: '#0891b2',
    weapon: '#b91c1c',
    armor: '#64748b',
    vehicle: '#374151',
    building: '#a3a3a3',
    organization: '#1f2937',
    creature: '#166534',
    custom: '#6b7280',
  };
  
  return colorMap[objectType.toLowerCase()] || '#6b7280';
}

export const schemaStore = createSchemaStore();

// Derived stores for convenient access
export const availableSchemas = derived(
  schemaStore,
  $schema => $schema.availableSchemas
);

export const currentSchema = derived(
  schemaStore,
  $schema => $schema.currentSchema
);

export const currentSchemaName = derived(
  schemaStore,
  $schema => $schema.currentSchemaName
);

export const availableObjectTypes = derived(
  schemaStore,
  $schema => $schema.availableObjectTypes.map(type => ({
    value: type,
    label: type.charAt(0).toUpperCase() + type.slice(1),
    icon: getObjectTypeIcon(type),
    color: getObjectTypeColor(type),
  }))
);

export const availableEdgeTypes = derived(
  schemaStore,
  $schema => $schema.availableEdgeTypes
);

export const isSchemaLoading = derived(
  schemaStore,
  $schema => $schema.isLoading || $schema.isLoadingObjectType
);

export const schemaError = derived(
  schemaStore,
  $schema => $schema.error
);

export const hasSchema = derived(
  schemaStore,
  $schema => $schema.currentSchema !== null
);

// Helper function to initialize schema store
export const initializeSchemaStore = async () => {
  try {
    await schemaStore.initialize();
  } catch (error) {
    console.error('Failed to initialize schema store:', error);
  }
};
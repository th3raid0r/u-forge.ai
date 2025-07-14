// TypeScript type definitions for u-forge.ai frontend

export interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

export interface ProjectInfo {
  name: string;
  path: string;
  created_at: string;
  last_modified: string;
  object_count: number;
  relationship_count: number;
}

export interface DatabaseStatus {
  has_data: boolean;
  has_schemas: boolean;
  object_count: number;
  relationship_count: number;
  schema_count: number;
}

export interface ObjectSummary {
  id: string;
  name: string;
  object_type: string;
  description?: string;
  created_at: string;
  tags: string[];
}

export interface Object {
  id: string;
  name: string;
  object_type: ObjectType;
  description?: string;
  created_at: string;
  updated_at: string;
  tags: string[];
  properties: Record<string, any>;
  relationships: Relationship[];
  text_chunks: TextChunk[];
}

export interface Relationship {
  target_id: string;
  edge_type: EdgeType;
  weight?: number;
  properties: Record<string, any>;
}

export interface TextChunk {
  id: string;
  content: string;
  chunk_type: string;
  metadata: Record<string, any>;
}

export interface RelationshipSummary {
  from_id: string;
  from_name: string;
  to_id: string;
  to_name: string;
  relationship_type: string;
  weight?: number;
}

export interface SearchRequest {
  query: string;
  object_types?: string[];
  limit?: number;
  use_semantic: boolean;
  use_exact: boolean;
}

export interface SearchResult {
  objects: ObjectSummary[];
  relationships: RelationshipSummary[];
  query_time_ms: number;
}

export interface CreateObjectRequest {
  name: string;
  object_type: string;
  description?: string;
  properties: Record<string, any>;
  tags: string[];
}

export interface CreateRelationshipRequest {
  from_id: string;
  to_id: string;
  relationship_type: string;
  weight?: number;
}

export interface GraphData {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export interface GraphNode {
  id: string;
  name: string;
  node_type: string;
  size: number;
  color: string;
}

export interface GraphEdge {
  source: string;
  target: string;
  edge_type: string;
  weight?: number;
}

// Object types enum
export enum ObjectType {
  Character = 'character',
  Location = 'location',
  Faction = 'faction',
  Item = 'item',
  Event = 'event',
  Session = 'session',
  Custom = 'custom'
}

// Edge types enum - Note: This is for UI purposes, backend uses string-based edge types
export enum EdgeType {
  Contains = 'contains',
  ConnectedTo = 'connected_to',
  PartOf = 'part_of',
  Owns = 'owns',
  LeadsTo = 'leads_to',
  FriendOf = 'friend_of',
  EnemyOf = 'enemy_of',
  MemberOf = 'member_of',
  Rules = 'rules',
  LocatedIn = 'located_in',
  OccurredAt = 'occurred_at',
  ParticipatedIn = 'participated_in',
  Triggers = 'triggers',
  Requires = 'requires',
  Mentions = 'mentions',
  Custom = 'custom'
}

// UI State interfaces
export interface UIState {
  sidebarVisible: boolean;
  aiPanelVisible: boolean;
  graphPanelVisible: boolean;
  currentView: ViewType;
  selectedObject: string | null;
  searchQuery: string;
  theme: ThemeType;
}

export enum ViewType {
  Welcome = 'welcome',
  ObjectEditor = 'object_editor',
  GraphView = 'graph_view',
  Search = 'search',
  Settings = 'settings'
}

export enum ThemeType {
  Dark = 'dark',
  Light = 'light',
  Auto = 'auto'
}

// Tree view interfaces
export interface TreeNode {
  id: string;
  name: string;
  type: string;
  children?: TreeNode[];
  expanded?: boolean;
  selected?: boolean;
  icon?: string;
}

// AI Panel interfaces
export interface AIMessage {
  id: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  timestamp: string;
  metadata?: Record<string, any>;
}

export interface AIConversation {
  id: string;
  title: string;
  messages: AIMessage[];
  created_at: string;
  updated_at: string;
}

// Graph visualization interfaces
export interface GraphLayout {
  type: 'force' | 'hierarchical' | 'circular' | 'grid';
  options: Record<string, any>;
}

export interface GraphFilter {
  objectTypes: string[];
  relationshipTypes: string[];
  dateRange?: {
    start: string;
    end: string;
  };
  tags: string[];
}

// Schema interfaces
export interface SchemaDefinition {
  name: string;
  version: string;
  description: string;
  object_types: Record<string, ObjectTypeSchema>;
  edge_types: Record<string, EdgeTypeSchema>;
}

export interface ObjectTypeSchema {
  name: string;
  description: string;
  properties: Record<string, PropertySchema>;
  required_properties: string[];
  icon?: string;
  color?: string;
}

export interface PropertySchema {
  property_type: PropertyType;
  description: string;
  required: boolean;
  validation?: ValidationRule;
}

export interface EdgeTypeSchema {
  name: string;
  description: string;
  source_types: string[];
  target_types: string[];
  properties: Record<string, PropertySchema>;
  bidirectional?: boolean;
}

export enum PropertyType {
  String = 'string',
  Text = 'text',
  Number = 'number',
  Boolean = 'boolean',
  Array = 'array',
  Object = 'object',
  Reference = 'reference',
  Enum = 'enum'
}

export interface ValidationRule {
  min_length?: number;
  max_length?: number;
  min_value?: number;
  max_value?: number;
  pattern?: string;
  allowed_values?: string[];
}

// Content editor interfaces
export interface EditorTab {
  id: string;
  title: string;
  object_id?: string;
  content_type: 'object' | 'search' | 'graph' | 'welcome';
  dirty: boolean;
  active: boolean;
}

export interface EditorState {
  tabs: EditorTab[];
  activeTabId: string | null;
  unsavedChanges: boolean;
}

// File and project interfaces
export interface ProjectSettings {
  name: string;
  path: string;
  auto_save: boolean;
  backup_enabled: boolean;
  schema_name: string;
  ai_provider?: string;
  ai_api_key?: string;
}

export interface RecentProject {
  name: string;
  path: string;
  last_opened: string;
}

// Error and notification interfaces
export interface NotificationMessage {
  id: string;
  type: 'info' | 'success' | 'warning' | 'error';
  title: string;
  message: string;
  timestamp: string;
  duration?: number;
  actions?: NotificationAction[];
}

export interface NotificationAction {
  label: string;
  action: () => void;
  primary?: boolean;
}

// Drag and drop interfaces
export interface DragData {
  type: 'object' | 'relationship' | 'file';
  data: any;
  source: string;
}

export interface DropTarget {
  accepts: string[];
  onDrop: (data: DragData) => void;
  onDragEnter?: () => void;
  onDragLeave?: () => void;
}

// Statistics and analytics interfaces
export interface ProjectStats {
  total_objects: number;
  total_relationships: number;
  objects_by_type: Record<string, number>;
  relationships_by_type: Record<string, number>;
  created_today: number;
  modified_today: number;
  average_connections: number;
}

export interface UsageMetrics {
  session_duration: number;
  objects_created: number;
  objects_modified: number;
  searches_performed: number;
  ai_interactions: number;
}

// Import/Export interfaces
export interface ExportOptions {
  format: 'json' | 'csv' | 'markdown' | 'pdf';
  include_relationships: boolean;
  include_metadata: boolean;
  object_types: string[];
  date_range?: {
    start: string;
    end: string;
  };
}

export interface ImportResult {
  success: boolean;
  objects_imported: number;
  relationships_imported: number;
  errors: ImportError[];
  warnings: ImportWarning[];
}

export interface ImportError {
  line?: number;
  object_id?: string;
  message: string;
  severity: 'error' | 'warning';
}

export interface ImportWarning extends ImportError {
  suggestion?: string;
}

// Plugin and extension interfaces
export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  author: string;
  enabled: boolean;
  settings: Record<string, any>;
}

export interface PluginAPI {
  registerCommand: (command: string, handler: Function) => void;
  addMenuItem: (menu: string, item: MenuItem) => void;
  registerObjectType: (type: ObjectTypeSchema) => void;
  registerEdgeType: (type: EdgeTypeSchema) => void;
}

export interface MenuItem {
  id: string;
  label: string;
  icon?: string;
  shortcut?: string;
  action: () => void;
  separator?: boolean;
  submenu?: MenuItem[];
}

// Window and layout interfaces
export interface WindowState {
  maximized: boolean;
  minimized: boolean;
  fullscreen: boolean;
  focused: boolean;
  bounds: {
    x: number;
    y: number;
    width: number;
    height: number;
  };
}

export interface PanelLayout {
  sidebar: {
    visible: boolean;
    width: number;
    collapsed: boolean;
  };
  editor: {
    tabs: EditorTab[];
    splitView: boolean;
  };
  aiPanel: {
    visible: boolean;
    width: number;
    docked: boolean;
  };
  graphPanel: {
    visible: boolean;
    height: number;
    layout: GraphLayout;
  };
}
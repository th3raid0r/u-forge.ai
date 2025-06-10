import { writable, derived } from 'svelte/store';
import type { GraphData, GraphNode, GraphEdge, GraphLayout, GraphFilter } from '../types';

// Graph state interface
interface GraphState {
  data: GraphData;
  filteredData: GraphData;
  layout: GraphLayout;
  filter: GraphFilter;
  selectedNodes: string[];
  selectedEdges: string[];
  hoveredNode: string | null;
  hoveredEdge: string | null;
  isLoading: boolean;
  error: string | null;
  viewBox: {
    x: number;
    y: number;
    width: number;
    height: number;
    scale: number;
  };
  simulation: {
    running: boolean;
    alpha: number;
    alphaTarget: number;
    velocityDecay: number;
  };
  settings: {
    nodeSize: {
      min: number;
      max: number;
      scale: number;
    };
    edgeWidth: {
      min: number;
      max: number;
      scale: number;
    };
    labels: {
      visible: boolean;
      minZoom: number;
      maxLength: number;
    };
    physics: {
      enabled: boolean;
      strength: number;
      distance: number;
      charge: number;
      gravity: number;
    };
    colors: {
      nodes: Record<string, string>;
      edges: Record<string, string>;
      background: string;
    };
  };
}

// Initial state
const initialState: GraphState = {
  data: {
    nodes: [],
    edges: [],
  },
  filteredData: {
    nodes: [],
    edges: [],
  },
  layout: {
    type: 'force',
    options: {
      iterations: 300,
      linkDistance: 100,
      linkStrength: 0.1,
      charge: -300,
      gravity: 0.1,
      theta: 0.8,
      alpha: 0.1,
    },
  },
  filter: {
    objectTypes: [],
    relationshipTypes: [],
    tags: [],
  },
  selectedNodes: [],
  selectedEdges: [],
  hoveredNode: null,
  hoveredEdge: null,
  isLoading: false,
  error: null,
  viewBox: {
    x: 0,
    y: 0,
    width: 800,
    height: 600,
    scale: 1,
  },
  simulation: {
    running: false,
    alpha: 1,
    alphaTarget: 0,
    velocityDecay: 0.4,
  },
  settings: {
    nodeSize: {
      min: 8,
      max: 32,
      scale: 1,
    },
    edgeWidth: {
      min: 1,
      max: 8,
      scale: 1,
    },
    labels: {
      visible: true,
      minZoom: 0.5,
      maxLength: 20,
    },
    physics: {
      enabled: true,
      strength: 0.3,
      distance: 100,
      charge: -300,
      gravity: 0.1,
    },
    colors: {
      nodes: {
        character: '#4CAF50',
        location: '#2196F3',
        faction: '#FF9800',
        item: '#9C27B0',
        event: '#F44336',
        session: '#607D8B',
        custom: '#795548',
      },
      edges: {
        contains: '#666666',
        connected_to: '#888888',
        part_of: '#999999',
        owns: '#AAA',
        leads_to: '#BBB',
        friend_of: '#4CAF50',
        enemy_of: '#F44336',
        member_of: '#2196F3',
        rules: '#FF9800',
        located_in: '#9C27B0',
        occurred_at: '#607D8B',
        participated_in: '#795548',
        triggers: '#FF5722',
        requires: '#FFC107',
        mentions: '#9E9E9E',
        custom: '#666666',
      },
      background: '#1e1e1e',
    },
  },
};

// Create the main graph store
const createGraphStore = () => {
  const { subscribe, set, update } = writable<GraphState>(initialState);

  return {
    subscribe,
    
    // Data management
    setGraphData: (data: GraphData) => {
      update(state => {
        const filteredData = applyFilters(data, state.filter);
        return {
          ...state,
          data,
          filteredData,
          error: null,
          isLoading: false,
        };
      });
    },
    
    updateGraphData: (partial: Partial<GraphData>) => {
      update(state => {
        const newData = {
          nodes: partial.nodes || state.data.nodes,
          edges: partial.edges || state.data.edges,
        };
        const filteredData = applyFilters(newData, state.filter);
        return {
          ...state,
          data: newData,
          filteredData,
        };
      });
    },
    
    addNode: (node: GraphNode) => {
      update(state => {
        const newData = {
          ...state.data,
          nodes: [...state.data.nodes, node],
        };
        const filteredData = applyFilters(newData, state.filter);
        return {
          ...state,
          data: newData,
          filteredData,
        };
      });
    },
    
    updateNode: (nodeId: string, updates: Partial<GraphNode>) => {
      update(state => {
        const newData = {
          ...state.data,
          nodes: state.data.nodes.map(node => 
            node.id === nodeId ? { ...node, ...updates } : node
          ),
        };
        const filteredData = applyFilters(newData, state.filter);
        return {
          ...state,
          data: newData,
          filteredData,
        };
      });
    },
    
    removeNode: (nodeId: string) => {
      update(state => {
        const newData = {
          nodes: state.data.nodes.filter(node => node.id !== nodeId),
          edges: state.data.edges.filter(edge => 
            edge.source !== nodeId && edge.target !== nodeId
          ),
        };
        const filteredData = applyFilters(newData, state.filter);
        return {
          ...state,
          data: newData,
          filteredData,
          selectedNodes: state.selectedNodes.filter(id => id !== nodeId),
        };
      });
    },
    
    addEdge: (edge: GraphEdge) => {
      update(state => {
        const newData = {
          ...state.data,
          edges: [...state.data.edges, edge],
        };
        const filteredData = applyFilters(newData, state.filter);
        return {
          ...state,
          data: newData,
          filteredData,
        };
      });
    },
    
    updateEdge: (edgeId: string, updates: Partial<GraphEdge>) => {
      update(state => {
        const newData = {
          ...state.data,
          edges: state.data.edges.map(edge => {
            const id = `${edge.source}-${edge.target}-${edge.edge_type}`;
            return id === edgeId ? { ...edge, ...updates } : edge;
          }),
        };
        const filteredData = applyFilters(newData, state.filter);
        return {
          ...state,
          data: newData,
          filteredData,
        };
      });
    },
    
    removeEdge: (edgeId: string) => {
      update(state => {
        const newData = {
          ...state.data,
          edges: state.data.edges.filter(edge => {
            const id = `${edge.source}-${edge.target}-${edge.edge_type}`;
            return id !== edgeId;
          }),
        };
        const filteredData = applyFilters(newData, state.filter);
        return {
          ...state,
          data: newData,
          filteredData,
          selectedEdges: state.selectedEdges.filter(id => id !== edgeId),
        };
      });
    },
    
    // Selection management
    selectNode: (nodeId: string, addToSelection = false) => {
      update(state => {
        let selectedNodes: string[];
        if (addToSelection) {
          selectedNodes = state.selectedNodes.includes(nodeId)
            ? state.selectedNodes.filter(id => id !== nodeId)
            : [...state.selectedNodes, nodeId];
        } else {
          selectedNodes = [nodeId];
        }
        return {
          ...state,
          selectedNodes,
          selectedEdges: addToSelection ? state.selectedEdges : [],
        };
      });
    },
    
    selectEdge: (edgeId: string, addToSelection = false) => {
      update(state => {
        let selectedEdges: string[];
        if (addToSelection) {
          selectedEdges = state.selectedEdges.includes(edgeId)
            ? state.selectedEdges.filter(id => id !== edgeId)
            : [...state.selectedEdges, edgeId];
        } else {
          selectedEdges = [edgeId];
        }
        return {
          ...state,
          selectedEdges,
          selectedNodes: addToSelection ? state.selectedNodes : [],
        };
      });
    },
    
    clearSelection: () => {
      update(state => ({
        ...state,
        selectedNodes: [],
        selectedEdges: [],
      }));
    },
    
    selectAll: () => {
      update(state => ({
        ...state,
        selectedNodes: state.filteredData.nodes.map(node => node.id),
        selectedEdges: state.filteredData.edges.map(edge => 
          `${edge.source}-${edge.target}-${edge.edge_type}`
        ),
      }));
    },
    
    // Hover management
    setHoveredNode: (nodeId: string | null) => {
      update(state => ({ ...state, hoveredNode: nodeId }));
    },
    
    setHoveredEdge: (edgeId: string | null) => {
      update(state => ({ ...state, hoveredEdge: edgeId }));
    },
    
    // Layout management
    setLayout: (layout: GraphLayout) => {
      update(state => ({ ...state, layout }));
    },
    
    updateLayoutOptions: (options: Record<string, any>) => {
      update(state => ({
        ...state,
        layout: {
          ...state.layout,
          options: { ...state.layout.options, ...options },
        },
      }));
    },
    
    // Filter management
    setFilter: (filter: GraphFilter) => {
      update(state => {
        const filteredData = applyFilters(state.data, filter);
        return {
          ...state,
          filter,
          filteredData,
        };
      });
    },
    
    updateFilter: (partial: Partial<GraphFilter>) => {
      update(state => {
        const newFilter = { ...state.filter, ...partial };
        const filteredData = applyFilters(state.data, newFilter);
        return {
          ...state,
          filter: newFilter,
          filteredData,
        };
      });
    },
    
    clearFilter: () => {
      update(state => {
        const emptyFilter: GraphFilter = {
          objectTypes: [],
          relationshipTypes: [],
          tags: [],
        };
        return {
          ...state,
          filter: emptyFilter,
          filteredData: state.data,
        };
      });
    },
    
    // View management
    setViewBox: (viewBox: GraphState['viewBox']) => {
      update(state => ({ ...state, viewBox }));
    },
    
    zoomTo: (scale: number, centerX?: number, centerY?: number) => {
      update(state => {
        const newViewBox = { ...state.viewBox };
        const oldScale = newViewBox.scale;
        const scaleRatio = scale / oldScale;
        
        if (centerX !== undefined && centerY !== undefined) {
          newViewBox.x = centerX - (centerX - newViewBox.x) * scaleRatio;
          newViewBox.y = centerY - (centerY - newViewBox.y) * scaleRatio;
        }
        
        newViewBox.scale = scale;
        
        return { ...state, viewBox: newViewBox };
      });
    },
    
    panTo: (x: number, y: number) => {
      update(state => ({
        ...state,
        viewBox: { ...state.viewBox, x, y },
      }));
    },
    
    fitToView: () => {
      update(state => {
        if (state.filteredData.nodes.length === 0) return state;
        
        // Calculate bounding box of all nodes
        let minX = Infinity, minY = Infinity;
        let maxX = -Infinity, maxY = -Infinity;
        
        state.filteredData.nodes.forEach(node => {
          // Assuming nodes have x, y positions (would be set by layout algorithm)
          const x = (node as any).x || 0;
          const y = (node as any).y || 0;
          minX = Math.min(minX, x);
          minY = Math.min(minY, y);
          maxX = Math.max(maxX, x);
          maxY = Math.max(maxY, y);
        });
        
        const padding = 50;
        const width = maxX - minX + padding * 2;
        const height = maxY - minY + padding * 2;
        
        const scaleX = state.viewBox.width / width;
        const scaleY = state.viewBox.height / height;
        const scale = Math.min(scaleX, scaleY, 2); // Max zoom of 2x
        
        return {
          ...state,
          viewBox: {
            ...state.viewBox,
            x: minX - padding,
            y: minY - padding,
            scale,
          },
        };
      });
    },
    
    // Simulation management
    startSimulation: () => {
      update(state => ({
        ...state,
        simulation: { ...state.simulation, running: true, alpha: 1 },
      }));
    },
    
    stopSimulation: () => {
      update(state => ({
        ...state,
        simulation: { ...state.simulation, running: false, alpha: 0 },
      }));
    },
    
    updateSimulation: (updates: Partial<GraphState['simulation']>) => {
      update(state => ({
        ...state,
        simulation: { ...state.simulation, ...updates },
      }));
    },
    
    // Settings management
    updateSettings: (settings: Partial<GraphState['settings']>) => {
      update(state => ({
        ...state,
        settings: { ...state.settings, ...settings },
      }));
    },
    
    updateNodeColors: (colors: Record<string, string>) => {
      update(state => ({
        ...state,
        settings: {
          ...state.settings,
          colors: {
            ...state.settings.colors,
            nodes: { ...state.settings.colors.nodes, ...colors },
          },
        },
      }));
    },
    
    updateEdgeColors: (colors: Record<string, string>) => {
      update(state => ({
        ...state,
        settings: {
          ...state.settings,
          colors: {
            ...state.settings.colors,
            edges: { ...state.settings.colors.edges, ...colors },
          },
        },
      }));
    },
    
    // State management
    setLoading: (loading: boolean) => {
      update(state => ({ ...state, isLoading: loading }));
    },
    
    setError: (error: string | null) => {
      update(state => ({ ...state, error, isLoading: false }));
    },
    
    // Utility methods
    getCurrentState: (): GraphState => {
      let currentState: GraphState = initialState;
      subscribe(state => currentState = state)();
      return currentState;
    },
    
    reset: () => {
      set(initialState);
    },
  };
};

// Helper function to apply filters to graph data
function applyFilters(data: GraphData, filter: GraphFilter): GraphData {
  let filteredNodes = data.nodes;
  let filteredEdges = data.edges;
  
  // Filter by object types
  if (filter.objectTypes.length > 0) {
    filteredNodes = filteredNodes.filter(node => 
      filter.objectTypes.includes(node.node_type)
    );
  }
  
  // Filter by tags (if nodes have tags property)
  if (filter.tags.length > 0) {
    filteredNodes = filteredNodes.filter(node => {
      const nodeTags = (node as any).tags || [];
      return filter.tags.some(tag => nodeTags.includes(tag));
    });
  }
  
  // Filter by date range (if applicable)
  if (filter.dateRange) {
    const startDate = new Date(filter.dateRange.start);
    const endDate = new Date(filter.dateRange.end);
    
    filteredNodes = filteredNodes.filter(node => {
      const nodeDate = (node as any).created_at;
      if (!nodeDate) return true;
      const date = new Date(nodeDate);
      return date >= startDate && date <= endDate;
    });
  }
  
  // Get node IDs for edge filtering
  const nodeIds = new Set(filteredNodes.map(node => node.id));
  
  // Filter edges by relationship types and connected nodes
  filteredEdges = filteredEdges.filter(edge => {
    // Must connect to filtered nodes
    if (!nodeIds.has(edge.source) || !nodeIds.has(edge.target)) {
      return false;
    }
    
    // Filter by relationship types
    if (filter.relationshipTypes.length > 0) {
      return filter.relationshipTypes.includes(edge.edge_type);
    }
    
    return true;
  });
  
  return {
    nodes: filteredNodes,
    edges: filteredEdges,
  };
}

export const graphStore = createGraphStore();

// Derived stores for specific aspects
export const graphData = derived(
  graphStore,
  $graph => $graph.data
);

export const filteredGraphData = derived(
  graphStore,
  $graph => $graph.filteredData
);

export const graphLayout = derived(
  graphStore,
  $graph => $graph.layout
);

export const graphFilter = derived(
  graphStore,
  $graph => $graph.filter
);

export const selectedNodes = derived(
  graphStore,
  $graph => $graph.selectedNodes
);

export const selectedEdges = derived(
  graphStore,
  $graph => $graph.selectedEdges
);

export const hoveredNode = derived(
  graphStore,
  $graph => $graph.hoveredNode
);

export const hoveredEdge = derived(
  graphStore,
  $graph => $graph.hoveredEdge
);

export const graphViewBox = derived(
  graphStore,
  $graph => $graph.viewBox
);

export const graphSimulation = derived(
  graphStore,
  $graph => $graph.simulation
);

export const graphSettings = derived(
  graphStore,
  $graph => $graph.settings
);

export const isGraphLoading = derived(
  graphStore,
  $graph => $graph.isLoading
);

export const graphError = derived(
  graphStore,
  $graph => $graph.error
);

export const hasSelection = derived(
  graphStore,
  $graph => $graph.selectedNodes.length > 0 || $graph.selectedEdges.length > 0
);

export const selectionInfo = derived(
  graphStore,
  $graph => ({
    nodeCount: $graph.selectedNodes.length,
    edgeCount: $graph.selectedEdges.length,
    total: $graph.selectedNodes.length + $graph.selectedEdges.length,
  })
);

export const graphStats = derived(
  filteredGraphData,
  $data => {
    const nodeTypes = new Map<string, number>();
    const edgeTypes = new Map<string, number>();
    
    $data.nodes.forEach(node => {
      nodeTypes.set(node.node_type, (nodeTypes.get(node.node_type) || 0) + 1);
    });
    
    $data.edges.forEach(edge => {
      edgeTypes.set(edge.edge_type, (edgeTypes.get(edge.edge_type) || 0) + 1);
    });
    
    return {
      totalNodes: $data.nodes.length,
      totalEdges: $data.edges.length,
      nodeTypes: Object.fromEntries(nodeTypes),
      edgeTypes: Object.fromEntries(edgeTypes),
    };
  }
);
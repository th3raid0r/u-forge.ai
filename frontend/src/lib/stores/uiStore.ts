import { writable, derived } from 'svelte/store';
import { ViewType, ThemeType } from '../types';
import type { UIState, EditorState, EditorTab, PanelLayout, WindowState } from '../types';

// UI state interface
interface UIStoreState {
  ui: UIState;
  editor: EditorState;
  layout: PanelLayout;
  window: WindowState;
  notifications: any[];
  contextMenu: {
    visible: boolean;
    x: number;
    y: number;
    items: any[];
  };
  modal: {
    visible: boolean;
    component: string | null;
    props: Record<string, any>;
  };
}

// Initial state
const initialState: UIStoreState = {
  ui: {
    sidebarVisible: true,
    aiPanelVisible: true,
    graphPanelVisible: true,
    currentView: ViewType.Welcome,
    selectedObject: null,
    searchQuery: '',
    theme: ThemeType.Dark,
  },
  editor: {
    tabs: [],
    activeTabId: null,
    unsavedChanges: false,
  },
  layout: {
    sidebar: {
      visible: true,
      width: 280,
      collapsed: false,
    },
    editor: {
      tabs: [],
      splitView: false,
    },
    aiPanel: {
      visible: true,
      width: 320,
      docked: true,
    },
    graphPanel: {
      visible: true,
      height: 300,
      layout: {
        type: 'force',
        options: {},
      },
    },
  },
  window: {
    maximized: false,
    minimized: false,
    fullscreen: false,
    focused: true,
    bounds: {
      x: 100,
      y: 100,
      width: 1400,
      height: 900,
    },
  },
  notifications: [],
  contextMenu: {
    visible: false,
    x: 0,
    y: 0,
    items: [],
  },
  modal: {
    visible: false,
    component: null,
    props: {},
  },
};

// Create the main UI store
const createUIStore = () => {
  const { subscribe, set, update } = writable<UIStoreState>(initialState);

  return {
    subscribe,
    
    // Theme management
    setTheme: (theme: ThemeType) => {
      update(state => ({
        ...state,
        ui: { ...state.ui, theme },
      }));
      
      // Apply theme to document
      document.documentElement.setAttribute('data-theme', theme);
      
      // Save to localStorage
      try {
        localStorage.setItem('theme', theme);
      } catch (error) {
        console.error('Failed to save theme preference:', error);
      }
    },
    
    toggleTheme: () => {
      const currentState = uiStore.getCurrentState();
      const newTheme = currentState.ui.theme === ThemeType.Dark ? ThemeType.Light : ThemeType.Dark;
      uiStore.setTheme(newTheme);
    },
    
    // Panel visibility
    setSidebarVisible: (visible: boolean) => {
      update(state => ({
        ...state,
        ui: { ...state.ui, sidebarVisible: visible },
        layout: {
          ...state.layout,
          sidebar: { ...state.layout.sidebar, visible },
        },
      }));
    },
    
    setAIPanelVisible: (visible: boolean) => {
      update(state => ({
        ...state,
        ui: { ...state.ui, aiPanelVisible: visible },
        layout: {
          ...state.layout,
          aiPanel: { ...state.layout.aiPanel, visible },
        },
      }));
    },
    
    setGraphPanelVisible: (visible: boolean) => {
      update(state => ({
        ...state,
        ui: { ...state.ui, graphPanelVisible: visible },
        layout: {
          ...state.layout,
          graphPanel: { ...state.layout.graphPanel, visible },
        },
      }));
    },
    
    toggleSidebar: () => {
      const currentState = uiStore.getCurrentState();
      uiStore.setSidebarVisible(!currentState.ui.sidebarVisible);
    },
    
    toggleAIPanel: () => {
      const currentState = uiStore.getCurrentState();
      uiStore.setAIPanelVisible(!currentState.ui.aiPanelVisible);
    },
    
    toggleGraphPanel: () => {
      const currentState = uiStore.getCurrentState();
      uiStore.setGraphPanelVisible(!currentState.ui.graphPanelVisible);
    },
    
    // Panel sizing
    setSidebarWidth: (width: number) => {
      update(state => ({
        ...state,
        layout: {
          ...state.layout,
          sidebar: { ...state.layout.sidebar, width },
        },
      }));
    },
    
    setAIPanelWidth: (width: number) => {
      update(state => ({
        ...state,
        layout: {
          ...state.layout,
          aiPanel: { ...state.layout.aiPanel, width },
        },
      }));
    },
    
    setGraphPanelHeight: (height: number) => {
      update(state => ({
        ...state,
        layout: {
          ...state.layout,
          graphPanel: { ...state.layout.graphPanel, height },
        },
      }));
    },
    
    // View management
    setCurrentView: (view: ViewType) => {
      update(state => ({
        ...state,
        ui: { ...state.ui, currentView: view },
      }));
    },
    
    setSelectedObject: (objectId: string | null) => {
      update(state => ({
        ...state,
        ui: { ...state.ui, selectedObject: objectId },
      }));
    },
    
    setSearchQuery: (query: string) => {
      update(state => ({
        ...state,
        ui: { ...state.ui, searchQuery: query },
      }));
    },
    
    // Editor tab management
    addTab: (tab: EditorTab) => {
      update(state => {
        const existingTab = state.editor.tabs.find(t => t.id === tab.id);
        if (existingTab) {
          // Tab already exists, just activate it
          return {
            ...state,
            editor: {
              ...state.editor,
              activeTabId: tab.id,
              tabs: state.editor.tabs.map(t => ({ ...t, active: t.id === tab.id })),
            },
          };
        }
        
        // Add new tab and activate it
        const newTabs = state.editor.tabs.map(t => ({ ...t, active: false }));
        newTabs.push({ ...tab, active: true });
        
        return {
          ...state,
          editor: {
            ...state.editor,
            tabs: newTabs,
            activeTabId: tab.id,
          },
        };
      });
    },
    
    closeTab: (tabId: string) => {
      update(state => {
        const tabs = state.editor.tabs.filter(t => t.id !== tabId);
        let activeTabId = state.editor.activeTabId;
        
        // If we closed the active tab, activate the last tab
        if (activeTabId === tabId && tabs.length > 0) {
          activeTabId = tabs[tabs.length - 1].id;
          tabs[tabs.length - 1].active = true;
        } else if (tabs.length === 0) {
          activeTabId = null;
        }
        
        return {
          ...state,
          editor: {
            ...state.editor,
            tabs,
            activeTabId,
          },
        };
      });
    },
    
    setActiveTab: (tabId: string) => {
      update(state => ({
        ...state,
        editor: {
          ...state.editor,
          activeTabId: tabId,
          tabs: state.editor.tabs.map(t => ({ ...t, active: t.id === tabId })),
        },
      }));
    },
    
    updateTab: (tabId: string, updates: Partial<EditorTab>) => {
      update(state => ({
        ...state,
        editor: {
          ...state.editor,
          tabs: state.editor.tabs.map(t => t.id === tabId ? { ...t, ...updates } : t),
        },
      }));
    },
    
    setTabDirty: (tabId: string, dirty: boolean) => {
      uiStore.updateTab(tabId, { dirty });
      update(state => ({
        ...state,
        editor: {
          ...state.editor,
          unsavedChanges: state.editor.tabs.some(t => t.dirty),
        },
      }));
    },
    
    closeAllTabs: () => {
      update(state => ({
        ...state,
        editor: {
          ...state.editor,
          tabs: [],
          activeTabId: null,
          unsavedChanges: false,
        },
      }));
    },
    
    // Window state management
    setWindowState: (windowState: Partial<WindowState>) => {
      update(state => ({
        ...state,
        window: { ...state.window, ...windowState },
      }));
    },
    
    setWindowBounds: (bounds: WindowState['bounds']) => {
      update(state => ({
        ...state,
        window: { ...state.window, bounds },
      }));
    },
    
    // Notification management
    addNotification: (notification: any) => {
      const id = `notification-${Date.now()}-${Math.random()}`;
      const notificationWithId = { ...notification, id };
      
      update(state => ({
        ...state,
        notifications: [...state.notifications, notificationWithId],
      }));
      
      // Auto-remove after duration
      if (notification.duration && notification.duration > 0) {
        setTimeout(() => {
          uiStore.removeNotification(id);
        }, notification.duration);
      }
      
      return id;
    },
    
    removeNotification: (id: string) => {
      update(state => ({
        ...state,
        notifications: state.notifications.filter(n => n.id !== id),
      }));
    },
    
    clearNotifications: () => {
      update(state => ({
        ...state,
        notifications: [],
      }));
    },
    
    // Context menu management
    showContextMenu: (x: number, y: number, items: any[]) => {
      update(state => ({
        ...state,
        contextMenu: {
          visible: true,
          x,
          y,
          items,
        },
      }));
    },
    
    hideContextMenu: () => {
      update(state => ({
        ...state,
        contextMenu: {
          ...state.contextMenu,
          visible: false,
        },
      }));
    },
    
    // Modal management
    showModal: (component: string, props: Record<string, any> = {}) => {
      update(state => ({
        ...state,
        modal: {
          visible: true,
          component,
          props,
        },
      }));
    },
    
    hideModal: () => {
      update(state => ({
        ...state,
        modal: {
          visible: false,
          component: null,
          props: {},
        },
      }));
    },
    
    // Utility methods
    getCurrentState: (): UIStoreState => {
      let currentState: UIStoreState = initialState;
      subscribe(state => currentState = state)();
      return currentState;
    },
    
    // Save/Load state
    saveUIState: () => {
      const state = uiStore.getCurrentState();
      const persistedState = {
        theme: state.ui.theme,
        sidebarVisible: state.ui.sidebarVisible,
        aiPanelVisible: state.ui.aiPanelVisible,
        graphPanelVisible: state.ui.graphPanelVisible,
        layout: state.layout,
      };
      
      try {
        localStorage.setItem('uiState', JSON.stringify(persistedState));
      } catch (error) {
        console.error('Failed to save UI state:', error);
      }
    },
    
    loadUIState: () => {
      try {
        const stored = localStorage.getItem('uiState');
        if (stored) {
          const persistedState = JSON.parse(stored);
          
          update(state => ({
            ...state,
            ui: {
              ...state.ui,
              theme: persistedState.theme || state.ui.theme,
              sidebarVisible: persistedState.sidebarVisible ?? state.ui.sidebarVisible,
              aiPanelVisible: persistedState.aiPanelVisible ?? state.ui.aiPanelVisible,
              graphPanelVisible: persistedState.graphPanelVisible ?? state.ui.graphPanelVisible,
            },
            layout: {
              ...state.layout,
              ...persistedState.layout,
            },
          }));
          
          // Apply theme
          if (persistedState.theme) {
            document.documentElement.setAttribute('data-theme', persistedState.theme);
          }
        }
      } catch (error) {
        console.error('Failed to load UI state:', error);
      }
    },
    
    reset: () => {
      set(initialState);
    },
  };
};

export const uiStore = createUIStore();

// Derived stores for specific UI aspects
export const currentTheme = derived(
  uiStore,
  $ui => $ui.ui.theme
);

export const sidebarVisible = derived(
  uiStore,
  $ui => $ui.ui.sidebarVisible
);

export const aiPanelVisible = derived(
  uiStore,
  $ui => $ui.ui.aiPanelVisible
);

export const graphPanelVisible = derived(
  uiStore,
  $ui => $ui.ui.graphPanelVisible
);

export const currentView = derived(
  uiStore,
  $ui => $ui.ui.currentView
);

export const selectedObject = derived(
  uiStore,
  $ui => $ui.ui.selectedObject
);

export const searchQuery = derived(
  uiStore,
  $ui => $ui.ui.searchQuery
);

export const editorTabs = derived(
  uiStore,
  $ui => $ui.editor.tabs
);

export const activeTab = derived(
  uiStore,
  $ui => $ui.editor.tabs.find(t => t.id === $ui.editor.activeTabId) || null
);

export const hasUnsavedEditorChanges = derived(
  uiStore,
  $ui => $ui.editor.unsavedChanges
);

export const panelLayout = derived(
  uiStore,
  $ui => $ui.layout
);

export const windowState = derived(
  uiStore,
  $ui => $ui.window
);

export const notifications = derived(
  uiStore,
  $ui => $ui.notifications
);

export const contextMenu = derived(
  uiStore,
  $ui => $ui.contextMenu
);

export const modal = derived(
  uiStore,
  $ui => $ui.modal
);

// Helper function to initialize the UI store
export const initializeUIStore = () => {
  uiStore.loadUIState();
  
  // Set up auto-save
  if (typeof window !== 'undefined') {
    window.addEventListener('beforeunload', () => {
      uiStore.saveUIState();
    });
    
    // Save state periodically
    setInterval(() => {
      uiStore.saveUIState();
    }, 30000); // Save every 30 seconds
  }
};
import { writable, derived } from 'svelte/store';
import { invoke } from '@tauri-apps/api/tauri';
import type { ProjectInfo, ProjectSettings, RecentProject, ProjectStats, ApiResponse } from '../types';

// Project state interface
interface ProjectState {
  currentProject: ProjectInfo | null;
  settings: ProjectSettings | null;
  stats: ProjectStats | null;
  isLoading: boolean;
  error: string | null;
  recentProjects: RecentProject[];
  unsavedChanges: boolean;
}

// Initial state
const initialState: ProjectState = {
  currentProject: null,
  settings: null,
  stats: null,
  isLoading: false,
  error: null,
  recentProjects: [],
  unsavedChanges: false,
};

// Create the main project store
const createProjectStore = () => {
  const { subscribe, set, update } = writable<ProjectState>(initialState);

  return {
    subscribe,
    
    // Project management
    setProject: (project: ProjectInfo) => {
      update(state => ({
        ...state,
        currentProject: project,
        error: null,
        isLoading: false,
      }));
      
      // Add to recent projects
      const recentProject: RecentProject = {
        name: project.name,
        path: project.path,
        last_opened: new Date().toISOString(),
      };
      
      update(state => {
        const filtered = state.recentProjects.filter(p => p.path !== project.path);
        return {
          ...state,
          recentProjects: [recentProject, ...filtered].slice(0, 10), // Keep only 10 recent projects
        };
      });
      
      // Save to localStorage
      projectStore.saveRecentProjects();
    },
    
    clearProject: () => {
      update(state => ({
        ...state,
        currentProject: null,
        settings: null,
        stats: null,
        unsavedChanges: false,
        error: null,
      }));
    },
    
    setLoading: (loading: boolean) => {
      update(state => ({ ...state, isLoading: loading }));
    },
    
    setError: (error: string | null) => {
      update(state => ({ ...state, error, isLoading: false }));
    },
    
    // Project settings
    setSettings: (settings: ProjectSettings) => {
      update(state => ({ ...state, settings }));
    },
    
    updateSettings: (partial: Partial<ProjectSettings>) => {
      update(state => ({
        ...state,
        settings: state.settings ? { ...state.settings, ...partial } : null,
      }));
    },
    
    // Project statistics
    setStats: (stats: ProjectStats) => {
      update(state => ({ ...state, stats }));
    },
    
    updateStats: (partial: Partial<ProjectStats>) => {
      update(state => ({
        ...state,
        stats: state.stats ? { ...state.stats, ...partial } : null,
      }));
    },
    
    // Unsaved changes tracking
    setUnsavedChanges: (hasChanges: boolean) => {
      update(state => ({ ...state, unsavedChanges: hasChanges }));
    },
    
    markSaved: () => {
      update(state => ({ ...state, unsavedChanges: false }));
    },
    
    // Recent projects management
    loadRecentProjects: () => {
      try {
        const stored = localStorage.getItem('recentProjects');
        if (stored) {
          const recentProjects = JSON.parse(stored) as RecentProject[];
          update(state => ({ ...state, recentProjects }));
        }
      } catch (error) {
        console.error('Failed to load recent projects:', error);
      }
    },
    
    saveRecentProjects: () => {
      const state = projectStore.getCurrentState();
      try {
        localStorage.setItem('recentProjects', JSON.stringify(state.recentProjects));
      } catch (error) {
        console.error('Failed to save recent projects:', error);
      }
    },
    
    removeRecentProject: (path: string) => {
      update(state => ({
        ...state,
        recentProjects: state.recentProjects.filter(p => p.path !== path),
      }));
      projectStore.saveRecentProjects();
    },
    
    clearRecentProjects: () => {
      update(state => ({ ...state, recentProjects: [] }));
      localStorage.removeItem('recentProjects');
    },
    
    // Refresh project statistics
    refreshStats: async () => {
      const state = projectStore.getCurrentState();
      if (!state.currentProject) {
        console.warn('No active project to refresh stats for');
        return;
      }

      try {
        console.log('🔄 [ProjectStore] Refreshing project stats...');
        
        const response = await invoke('get_project_stats') as ApiResponse<ProjectStats>;
        
        if (response.success && response.data) {
          projectStore.setStats(response.data);
          
          // Also update the current project with new counts
          if (state.currentProject) {
              const updatedProject: ProjectInfo = {
                  ...state.currentProject,
                  object_count: response.data.total_objects,
                  relationship_count: response.data.total_relationships,
                  last_modified: new Date().toISOString(),
              };
              projectStore.setProject(updatedProject);
          }
          
          console.log('✅ [ProjectStore] Project stats refreshed:', response.data);
        } else {
          console.error('❌ [ProjectStore] Failed to refresh stats:', response.error);
          projectStore.setError(response.error || 'Failed to refresh project statistics');
        }
      } catch (error) {
        console.error('❌ [ProjectStore] Error refreshing stats:', error);
        projectStore.setError('Error refreshing project statistics');
      }
    },
    
    // Utility methods
    getCurrentState: (): ProjectState => {
      let currentState: ProjectState = initialState;
      subscribe(state => currentState = state)();
      return currentState;
    },
    
    reset: () => {
      set(initialState);
    },
  };
};

export const projectStore = createProjectStore();

// Derived stores for specific aspects
export const currentProject = derived(
  projectStore,
  $project => $project.currentProject
);

export const projectSettings = derived(
  projectStore,
  $project => $project.settings
);

export const projectStats = derived(
  projectStore,
  $project => $project.stats
);

export const isProjectLoading = derived(
  projectStore,
  $project => $project.isLoading
);

export const projectError = derived(
  projectStore,
  $project => $project.error
);

export const hasUnsavedChanges = derived(
  projectStore,
  $project => $project.unsavedChanges
);

export const recentProjects = derived(
  projectStore,
  $project => $project.recentProjects
);

export const hasActiveProject = derived(
  projectStore,
  $project => $project.currentProject !== null
);

export const projectName = derived(
  projectStore,
  $project => $project.currentProject?.name || 'No Project'
);

export const projectPath = derived(
  projectStore,
  $project => $project.currentProject?.path || ''
);

// Helper function to initialize the store
export const initializeProjectStore = () => {
  projectStore.loadRecentProjects();
  
  // Set up auto-save for recent projects
  if (typeof window !== 'undefined') {
    window.addEventListener('beforeunload', () => {
      projectStore.saveRecentProjects();
    });
  }
};
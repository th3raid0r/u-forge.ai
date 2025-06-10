import { vitePreprocess } from '@sveltejs/vite-plugin-svelte'

export default {
  // Consult https://svelte.dev/docs#compile-time-svelte-preprocess
  // for more information about preprocessors
  preprocess: vitePreprocess(),
  
  compilerOptions: {
    // Enable run-time checks when not in production
    dev: process.env.NODE_ENV !== 'production',
    
    // Extract CSS into separate files
    css: 'injected',
    
    // Generate source maps for easier debugging
    enableSourcemap: true
  },
  
  // Vite-specific options
  vitePlugin: {
    // Include all .svelte files for processing
    include: ['src/**/*.svelte'],
    
    // Exclude node_modules and other unnecessary directories
    exclude: ['node_modules/**', 'dist/**', 'build/**'],
    
    // HMR options moved into vitePlugin
    hot: process.env.NODE_ENV === 'development' && {
      // Preserve local state on HMR updates
      preserveLocalState: true,
      
      // Turn on to see which components are being updated
      noPreserveStateKey: '@!hmr',
      
      // Prevent updating components when syntax errors are present
      noReload: false,
      
      // Try to recover from runtime errors during development
      optimistic: true
    }
  }
}
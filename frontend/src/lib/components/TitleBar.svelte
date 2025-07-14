<script lang="ts">
    import { onMount } from "svelte";
    import { appWindow } from "@tauri-apps/api/window";
    import { currentProject } from "../stores/projectStore";

    let isMaximized = false;
    let windowTitle = "u-forge.ai - Universe Forge";

    $: if ($currentProject) {
        windowTitle = `${$currentProject.name} - u-forge.ai`;
    }

    onMount(() => {
        let cleanup: (() => void) | undefined;

        const init = async () => {
            // Check initial window state
            isMaximized = await appWindow.isMaximized();

            // Listen for window state changes
            const unlistenMaximize = await appWindow.onResized(async () => {
                isMaximized = await appWindow.isMaximized();
            });

            cleanup = unlistenMaximize;
        };

        init();

        return () => {
            if (cleanup) {
                cleanup();
            }
        };
    });

    async function minimizeWindow() {
        await appWindow.minimize();
    }

    async function toggleMaximize() {
        await appWindow.toggleMaximize();
    }

    async function closeWindow() {
        await appWindow.close();
    }
</script>

<div class="titlebar" data-tauri-drag-region>
    <div class="titlebar-content">
        <div class="titlebar-icon">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
                <path d="M12 2L2 7L12 12L22 7L12 2Z" opacity="0.7" />
                <path d="M2 17L12 22L22 17" opacity="0.5" />
                <path d="M2 12L12 17L22 12" opacity="0.8" />
            </svg>
        </div>

        <div class="titlebar-title">
            {windowTitle}
        </div>

        <div class="titlebar-info">
            {#if $currentProject}
                <span class="project-info">
                    {$currentProject.object_count} objects • {$currentProject.relationship_count}
                    relationships
                </span>
            {/if}
        </div>
    </div>

    <div class="titlebar-controls">
        <button
            class="titlebar-button minimize"
            on:click={minimizeWindow}
            title="Minimize"
            data-action="minimize"
        >
            <svg width="10" height="10" viewBox="0 0 10 10">
                <line
                    x1="0"
                    y1="5"
                    x2="10"
                    y2="5"
                    stroke="currentColor"
                    stroke-width="1"
                />
            </svg>
        </button>

        <button
            class="titlebar-button maximize"
            on:click={toggleMaximize}
            title={isMaximized ? "Restore" : "Maximize"}
            data-action="maximize"
        >
            {#if isMaximized}
                <svg width="10" height="10" viewBox="0 0 10 10">
                    <rect
                        x="0"
                        y="2"
                        width="8"
                        height="8"
                        fill="none"
                        stroke="currentColor"
                        stroke-width="1"
                    />
                    <rect
                        x="2"
                        y="0"
                        width="8"
                        height="8"
                        fill="none"
                        stroke="currentColor"
                        stroke-width="1"
                    />
                </svg>
            {:else}
                <svg width="10" height="10" viewBox="0 0 10 10">
                    <rect
                        x="0"
                        y="0"
                        width="10"
                        height="10"
                        fill="none"
                        stroke="currentColor"
                        stroke-width="1"
                    />
                </svg>
            {/if}
        </button>

        <button
            class="titlebar-button close"
            on:click={closeWindow}
            title="Close"
            data-action="close"
        >
            <svg width="10" height="10" viewBox="0 0 10 10">
                <line
                    x1="0"
                    y1="0"
                    x2="10"
                    y2="10"
                    stroke="currentColor"
                    stroke-width="1"
                />
                <line
                    x1="10"
                    y1="0"
                    x2="0"
                    y2="10"
                    stroke="currentColor"
                    stroke-width="1"
                />
            </svg>
        </button>
    </div>
</div>

<style>
    .titlebar {
        position: fixed;
        top: 0;
        left: 0;
        right: 0;
        height: var(--titlebar-height);
        background: var(--bg-secondary);
        border-bottom: 1px solid var(--border-color);
        display: flex;
        align-items: center;
        justify-content: space-between;
        z-index: 1000;
        -webkit-app-region: drag;
        user-select: none;
    }

    .titlebar-content {
        display: flex;
        align-items: center;
        gap: var(--space-sm);
        padding-left: var(--space-md);
        flex: 1;
        min-width: 0; /* Allow text to truncate */
    }

    .titlebar-icon {
        color: var(--accent-color);
        display: flex;
        align-items: center;
        flex-shrink: 0;
    }

    .titlebar-title {
        font-size: var(--font-sm);
        font-weight: 500;
        color: var(--text-primary);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        flex-shrink: 1;
    }

    .titlebar-info {
        margin-left: auto;
        padding-right: var(--space-md);
    }

    .project-info {
        font-size: var(--font-xs);
        color: var(--text-muted);
        white-space: nowrap;
    }

    .titlebar-controls {
        display: flex;
        -webkit-app-region: no-drag;
    }

    .titlebar-button {
        display: flex;
        align-items: center;
        justify-content: center;
        width: 46px;
        height: var(--titlebar-height);
        border: none;
        background: transparent;
        color: var(--text-secondary);
        cursor: pointer;
        transition: background-color var(--transition-fast);
        font-size: 0; /* Hide any text content */
    }

    .titlebar-button:hover {
        background: var(--bg-tertiary);
        color: var(--text-primary);
    }

    .titlebar-button:active {
        background: var(--bg-quaternary);
    }

    .titlebar-button.close:hover {
        background: var(--error-color);
        color: white;
    }

    .titlebar-button svg {
        pointer-events: none;
    }

    /* Platform-specific styles */
    @media (display-mode: standalone) {
        .titlebar {
            /* PWA mode adjustments */
            padding-top: env(safe-area-inset-top);
        }
    }

    /* macOS style titlebar */
    @media (platform: macOS) {
        .titlebar {
            padding-left: 78px; /* Space for traffic lights */
        }

        .titlebar-controls {
            order: -1;
            padding-left: var(--space-md);
        }

        .titlebar-button {
            width: 12px;
            height: 12px;
            border-radius: 50%;
            margin-right: var(--space-xs);
        }

        .titlebar-button.close {
            background: #ff5f57;
        }

        .titlebar-button.minimize {
            background: #ffbd2e;
        }

        .titlebar-button.maximize {
            background: #28ca42;
        }

        .titlebar-button svg {
            opacity: 0;
            transition: opacity var(--transition-fast);
        }

        .titlebar-button:hover svg {
            opacity: 1;
        }
    }

    /* High contrast mode */
    @media (prefers-contrast: high) {
        .titlebar {
            border-bottom-color: var(--text-primary);
        }

        .titlebar-button {
            border: 1px solid var(--border-color);
        }
    }

    /* Reduced motion */
    @media (prefers-reduced-motion: reduce) {
        .titlebar-button {
            transition: none;
        }
    }
</style>

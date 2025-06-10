<script lang="ts">
  import { createEventDispatcher, onMount, tick } from 'svelte';
  import { invoke } from '@tauri-apps/api/tauri';
  import { uiStore } from '../stores/uiStore';
  import { projectStore } from '../stores/projectStore';
  import type { ProjectInfo, AIMessage, AIConversation } from '../types';
  
  export let currentProject: ProjectInfo | null = null;
  
  const dispatch = createEventDispatcher();
  
  let conversations: AIConversation[] = [];
  let activeConversationId: string | null = null;
  let currentMessages: AIMessage[] = [];
  let messageInput = '';
  let isTyping = false;
  let isConnected = false;
  let error: string | null = null;
  let chatContainer: HTMLElement;
  
  // AI settings
  let aiProvider = 'local'; // 'local', 'openai', 'anthropic'
  let model = 'default';
  let temperature = 0.7;
  let maxTokens = 1000;
  
  // Quick actions
  const quickActions = [
    { icon: '💡', text: 'Generate ideas', prompt: 'Help me brainstorm ideas for my worldbuilding project.' },
    { icon: '📝', text: 'Improve description', prompt: 'Help me improve the description of this object.' },
    { icon: '🔗', text: 'Suggest connections', prompt: 'What connections could this object have to other elements in my world?' },
    { icon: '🎲', text: 'Random generator', prompt: 'Generate some random elements for my campaign.' },
    { icon: '📚', text: 'Lore questions', prompt: 'Ask me questions to help develop the lore of my world.' },
    { icon: '⚔️', text: 'Create conflict', prompt: 'Help me create interesting conflicts and tensions in my world.' },
  ];
  
  onMount(() => {
    loadConversations();
    checkAIConnection();
  });
  
  async function loadConversations() {
    // Load conversations from local storage for now
    // In a real implementation, this would come from the backend
    const stored = localStorage.getItem('ai_conversations');
    if (stored) {
      try {
        conversations = JSON.parse(stored);
        if (conversations.length > 0) {
          setActiveConversation(conversations[0].id);
        }
      } catch (error) {
        console.error('Failed to load conversations:', error);
      }
    }
  }
  
  async function saveConversations() {
    try {
      localStorage.setItem('ai_conversations', JSON.stringify(conversations));
    } catch (error) {
      console.error('Failed to save conversations:', error);
    }
  }
  
  async function checkAIConnection() {
    // Mock AI connection check
    // In a real implementation, this would check if AI services are available
    isConnected = true;
  }
  
  function setActiveConversation(conversationId: string) {
    activeConversationId = conversationId;
    const conversation = conversations.find(c => c.id === conversationId);
    if (conversation) {
      currentMessages = [...conversation.messages];
      scrollToBottom();
    }
  }
  
  function createNewConversation() {
    const newConversation: AIConversation = {
      id: `conv-${Date.now()}`,
      title: 'New Conversation',
      messages: [],
      created_at: new Date().toISOString(),
      updated_at: new Date().toISOString(),
    };
    
    conversations = [newConversation, ...conversations];
    setActiveConversation(newConversation.id);
    saveConversations();
  }
  
  function deleteConversation(conversationId: string) {
    conversations = conversations.filter(c => c.id !== conversationId);
    
    if (activeConversationId === conversationId) {
      if (conversations.length > 0) {
        setActiveConversation(conversations[0].id);
      } else {
        activeConversationId = null;
        currentMessages = [];
      }
    }
    
    saveConversations();
  }
  
  async function sendMessage(content?: string) {
    const messageText = content || messageInput.trim();
    if (!messageText || isTyping) return;
    
    const userMessage: AIMessage = {
      id: `msg-${Date.now()}`,
      role: 'user',
      content: messageText,
      timestamp: new Date().toISOString(),
    };
    
    currentMessages = [...currentMessages, userMessage];
    messageInput = '';
    isTyping = true;
    error = null;
    
    await tick();
    scrollToBottom();
    
    try {
      // Simulate AI response (replace with actual AI integration)
      const aiResponse = await generateAIResponse(messageText, currentMessages);
      
      const assistantMessage: AIMessage = {
        id: `msg-${Date.now()}-ai`,
        role: 'assistant',
        content: aiResponse,
        timestamp: new Date().toISOString(),
      };
      
      currentMessages = [...currentMessages, assistantMessage];
      
      // Update conversation
      updateConversation();
      
    } catch (err) {
      console.error('AI response failed:', err);
      error = err instanceof Error ? err.message : 'Failed to get AI response';
      
      // Add error message
      const errorMessage: AIMessage = {
        id: `msg-${Date.now()}-error`,
        role: 'assistant',
        content: 'Sorry, I encountered an error. Please try again.',
        timestamp: new Date().toISOString(),
      };
      
      currentMessages = [...currentMessages, errorMessage];
    } finally {
      isTyping = false;
      await tick();
      scrollToBottom();
    }
  }
  
  async function generateAIResponse(userMessage: string, context: AIMessage[]): Promise<string> {
    // Mock AI response generation
    // In a real implementation, this would call your AI service
    const responses = [
      "That's an interesting idea! Let me help you develop it further.",
      "I can see several possibilities here. Would you like me to explore any specific aspect?",
      "Based on your worldbuilding so far, I think this could connect well with other elements.",
      "Here are some suggestions that might enhance your creative process:",
      "Let me analyze the context of your world and provide some insights.",
    ];
    
    // Simulate network delay
    await new Promise(resolve => setTimeout(resolve, 1000 + Math.random() * 2000));
    
    const randomResponse = responses[Math.floor(Math.random() * responses.length)];
    
    // Add some context-aware suggestions
    if (userMessage.toLowerCase().includes('character')) {
      return `${randomResponse}\n\nFor character development, consider:\n• Their motivations and goals\n• Relationships with other characters\n• Background and history\n• Unique traits or abilities`;
    } else if (userMessage.toLowerCase().includes('location')) {
      return `${randomResponse}\n\nFor location design, think about:\n• Geography and climate\n• Culture and inhabitants\n• History and significance\n• Connections to other places`;
    } else if (userMessage.toLowerCase().includes('plot') || userMessage.toLowerCase().includes('story')) {
      return `${randomResponse}\n\nFor plot development:\n• What conflicts drive the story?\n• How do characters change?\n• What are the stakes?\n• How does it connect to your world's themes?`;
    }
    
    return randomResponse;
  }
  
  function updateConversation() {
    if (!activeConversationId) return;
    
    const conversationIndex = conversations.findIndex(c => c.id === activeConversationId);
    if (conversationIndex === -1) return;
    
    const conversation = conversations[conversationIndex];
    conversation.messages = [...currentMessages];
    conversation.updated_at = new Date().toISOString();
    
    // Update title based on first user message
    if (conversation.messages.length >= 1 && conversation.title === 'New Conversation') {
      const firstUserMessage = conversation.messages.find(m => m.role === 'user');
      if (firstUserMessage) {
        conversation.title = firstUserMessage.content.slice(0, 50) + '...';
      }
    }
    
    conversations = [...conversations];
    saveConversations();
  }
  
  function scrollToBottom() {
    if (chatContainer) {
      chatContainer.scrollTop = chatContainer.scrollHeight;
    }
  }
  
  function handleKeyPress(event: KeyboardEvent) {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      sendMessage();
    }
  }
  
  function clearCurrentConversation() {
    if (activeConversationId) {
      const conversation = conversations.find(c => c.id === activeConversationId);
      if (conversation) {
        conversation.messages = [];
        currentMessages = [];
        conversations = [...conversations];
        saveConversations();
      }
    }
  }
  
  function formatTimestamp(timestamp: string): string {
    const date = new Date(timestamp);
    return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
  
  function toggleAISettings() {
    // Toggle AI settings panel
    console.log('Toggle AI settings');
  }
</script>

<div class="ai-panel">
  <!-- Panel Header -->
  <div class="ai-header">
    <div class="ai-title">
      <span class="ai-icon">🤖</span>
      <span>AI Assistant</span>
    </div>
    
    <div class="ai-controls">
      <button 
        class="btn icon-only" 
        on:click={createNewConversation}
        title="New Conversation"
      >
        ➕
      </button>
      
      <button 
        class="btn icon-only" 
        on:click={toggleAISettings}
        title="AI Settings"
      >
        ⚙️
      </button>
    </div>
  </div>
  
  <!-- Connection Status -->
  <div class="connection-status" class:connected={isConnected}>
    <div class="status-indicator"></div>
    <span class="status-text">
      {isConnected ? 'AI Connected' : 'AI Disconnected'}
    </span>
  </div>
  
  <!-- Conversations List (when no active conversation) -->
  {#if !activeConversationId && conversations.length > 0}
    <div class="conversations-list">
      <h4>Recent Conversations</h4>
      {#each conversations as conversation (conversation.id)}
        <button 
          class="conversation-item"
          on:click={() => setActiveConversation(conversation.id)}
        >
          <div class="conversation-title">{conversation.title}</div>
          <div class="conversation-date">
            {new Date(conversation.updated_at).toLocaleDateString()}
          </div>
          <button 
            class="conversation-delete"
            on:click|stopPropagation={() => deleteConversation(conversation.id)}
          >
            🗑️
          </button>
        </button>
      {/each}
    </div>
  {/if}
  
  <!-- Active Conversation -->
  {#if activeConversationId}
    <div class="chat-container">
      <!-- Chat Header -->
      <div class="chat-header">
        <button 
          class="btn icon-only" 
          on:click={() => activeConversationId = null}
          title="Back to conversations"
        >
          ◀
        </button>
        
        <div class="chat-title">
          {conversations.find(c => c.id === activeConversationId)?.title || 'Conversation'}
        </div>
        
        <button 
          class="btn icon-only" 
          on:click={clearCurrentConversation}
          title="Clear conversation"
        >
          🗑️
        </button>
      </div>
      
      <!-- Messages -->
      <div class="messages-container" bind:this={chatContainer}>
        {#if currentMessages.length === 0}
          <div class="welcome-message">
            <div class="welcome-icon">💭</div>
            <h4>Start a conversation</h4>
            <p>Ask me anything about your worldbuilding project!</p>
            
            <div class="quick-actions">
              <h5>Quick actions:</h5>
              <div class="action-buttons">
                {#each quickActions as action}
                  <button 
                    class="action-button"
                    on:click={() => sendMessage(action.prompt)}
                    title={action.prompt}
                  >
                    <span class="action-icon">{action.icon}</span>
                    <span class="action-text">{action.text}</span>
                  </button>
                {/each}
              </div>
            </div>
          </div>
        {:else}
          {#each currentMessages as message (message.id)}
            <div class="message" class:user={message.role === 'user'} class:assistant={message.role === 'assistant'}>
              <div class="message-header">
                <span class="message-role">
                  {message.role === 'user' ? '👤' : '🤖'}
                </span>
                <span class="message-time">
                  {formatTimestamp(message.timestamp)}
                </span>
              </div>
              <div class="message-content">
                {message.content}
              </div>
            </div>
          {/each}
          
          {#if isTyping}
            <div class="message assistant typing">
              <div class="message-header">
                <span class="message-role">🤖</span>
                <span class="message-time">typing...</span>
              </div>
              <div class="typing-indicator">
                <span></span>
                <span></span>
                <span></span>
              </div>
            </div>
          {/if}
        {/if}
      </div>
      
      <!-- Error Display -->
      {#if error}
        <div class="error-banner">
          <span class="error-icon">⚠️</span>
          <span class="error-text">{error}</span>
          <button class="error-close" on:click={() => error = null}>×</button>
        </div>
      {/if}
      
      <!-- Message Input -->
      <div class="message-input-container">
        <textarea
          bind:value={messageInput}
          on:keypress={handleKeyPress}
          placeholder="Ask me anything about your world..."
          class="message-input"
          rows="2"
          disabled={!isConnected || isTyping}
        ></textarea>
        
        <button 
          class="send-button"
          on:click={() => sendMessage()}
          disabled={!messageInput.trim() || !isConnected || isTyping}
          title="Send message (Enter)"
        >
          📤
        </button>
      </div>
    </div>
  {:else if conversations.length === 0}
    <!-- Empty State -->
    <div class="empty-state">
      <div class="empty-icon">🤖</div>
      <h3>AI Assistant</h3>
      <p>Get help with your worldbuilding, generate ideas, and explore creative possibilities.</p>
      <button class="btn primary" on:click={createNewConversation}>
        Start Conversation
      </button>
    </div>
  {/if}
</div>

<style>
  .ai-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg-secondary);
    border-left: 1px solid var(--border-color);
  }
  
  /* Panel Header */
  .ai-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--space-md);
    border-bottom: 1px solid var(--border-color);
    background: var(--bg-tertiary);
  }
  
  .ai-title {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    font-weight: 600;
    color: var(--text-primary);
  }
  
  .ai-icon {
    font-size: var(--font-lg);
  }
  
  .ai-controls {
    display: flex;
    gap: var(--space-xs);
  }
  
  /* Connection Status */
  .connection-status {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    background: var(--bg-tertiary);
    border-bottom: 1px solid var(--border-color);
  }
  
  .status-indicator {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--error-color);
    transition: background-color var(--transition-fast);
  }
  
  .connection-status.connected .status-indicator {
    background: var(--success-color);
  }
  
  .status-text {
    font-size: var(--font-xs);
    color: var(--text-muted);
  }
  
  /* Conversations List */
  .conversations-list {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-md);
  }
  
  .conversations-list h4 {
    margin: 0 0 var(--space-md) 0;
    color: var(--text-primary);
    font-size: var(--font-sm);
  }
  
  .conversation-item {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    width: 100%;
    padding: var(--space-sm);
    background: transparent;
    border: 1px solid var(--border-color);
    border-radius: var(--radius-sm);
    cursor: pointer;
    text-align: left;
    transition: all var(--transition-fast);
    margin-bottom: var(--space-sm);
    position: relative;
  }
  
  .conversation-item:hover {
    background: var(--bg-quaternary);
    border-color: var(--border-hover);
  }
  
  .conversation-title {
    font-weight: 500;
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    width: 100%;
    margin-bottom: var(--space-xs);
  }
  
  .conversation-date {
    font-size: var(--font-xs);
    color: var(--text-muted);
  }
  
  .conversation-delete {
    position: absolute;
    top: var(--space-xs);
    right: var(--space-xs);
    background: none;
    border: none;
    color: var(--text-muted);
    cursor: pointer;
    padding: var(--space-xs);
    border-radius: var(--radius-sm);
    opacity: 0;
    transition: all var(--transition-fast);
  }
  
  .conversation-item:hover .conversation-delete {
    opacity: 1;
  }
  
  .conversation-delete:hover {
    background: var(--error-color);
    color: white;
  }
  
  /* Chat Container */
  .chat-container {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  
  .chat-header {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    background: var(--bg-tertiary);
    border-bottom: 1px solid var(--border-color);
  }
  
  .chat-title {
    flex: 1;
    font-weight: 500;
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  
  /* Messages */
  .messages-container {
    flex: 1;
    overflow-y: auto;
    padding: var(--space-md);
    display: flex;
    flex-direction: column;
    gap: var(--space-md);
  }
  
  .message {
    display: flex;
    flex-direction: column;
    gap: var(--space-xs);
    max-width: 85%;
  }
  
  .message.user {
    align-self: flex-end;
    align-items: flex-end;
  }
  
  .message.assistant {
    align-self: flex-start;
    align-items: flex-start;
  }
  
  .message-header {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    font-size: var(--font-xs);
    color: var(--text-muted);
  }
  
  .message-content {
    background: var(--bg-tertiary);
    padding: var(--space-sm) var(--space-md);
    border-radius: var(--radius-md);
    color: var(--text-primary);
    line-height: 1.4;
    white-space: pre-wrap;
    word-wrap: break-word;
  }
  
  .message.user .message-content {
    background: var(--accent-color);
    color: white;
  }
  
  .message.assistant .message-content {
    background: var(--bg-quaternary);
  }
  
  /* Typing Indicator */
  .typing-indicator {
    display: flex;
    gap: 4px;
    padding: var(--space-sm) var(--space-md);
    background: var(--bg-quaternary);
    border-radius: var(--radius-md);
  }
  
  .typing-indicator span {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--text-muted);
    animation: typing 1.4s infinite;
  }
  
  .typing-indicator span:nth-child(2) {
    animation-delay: 0.2s;
  }
  
  .typing-indicator span:nth-child(3) {
    animation-delay: 0.4s;
  }
  
  @keyframes typing {
    0%, 60%, 100% {
      transform: translateY(0);
      opacity: 0.5;
    }
    30% {
      transform: translateY(-10px);
      opacity: 1;
    }
  }
  
  /* Welcome Message */
  .welcome-message {
    text-align: center;
    padding: var(--space-xl);
    color: var(--text-secondary);
  }
  
  .welcome-icon {
    font-size: 3rem;
    margin-bottom: var(--space-lg);
    opacity: 0.7;
  }
  
  .welcome-message h4 {
    margin: 0 0 var(--space-sm) 0;
    color: var(--text-primary);
  }
  
  .welcome-message p {
    margin: 0 0 var(--space-xl) 0;
    font-size: var(--font-sm);
  }
  
  /* Quick Actions */
  .quick-actions h5 {
    margin: 0 0 var(--space-md) 0;
    color: var(--text-primary);
    font-size: var(--font-sm);
  }
  
  .action-buttons {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-sm);
  }
  
  .action-button {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-xs);
    padding: var(--space-md);
    background: var(--bg-tertiary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius-sm);
    cursor: pointer;
    text-align: center;
    transition: all var(--transition-fast);
    color: var(--text-primary);
  }
  
  .action-button:hover {
    background: var(--bg-quaternary);
    border-color: var(--accent-color);
  }
  
  .action-icon {
    font-size: var(--font-lg);
  }
  
  .action-text {
    font-size: var(--font-xs);
    line-height: 1.2;
  }
  
  /* Error Banner */
  .error-banner {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    padding: var(--space-sm) var(--space-md);
    background: var(--error-color);
    color: white;
    font-size: var(--font-sm);
  }
  
  .error-text {
    flex: 1;
  }
  
  .error-close {
    background: none;
    border: none;
    color: white;
    cursor: pointer;
    padding: var(--space-xs);
    border-radius: var(--radius-sm);
  }
  
  .error-close:hover {
    background: rgba(255, 255, 255, 0.2);
  }
  
  /* Message Input */
  .message-input-container {
    display: flex;
    gap: var(--space-sm);
    padding: var(--space-md);
    border-top: 1px solid var(--border-color);
    background: var(--bg-tertiary);
  }
  
  .message-input {
    flex: 1;
    background: var(--bg-secondary);
    border: 1px solid var(--border-color);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    padding: var(--space-sm);
    font-size: var(--font-sm);
    resize: none;
    min-height: 40px;
    max-height: 120px;
  }
  
  .message-input:focus {
    outline: none;
    border-color: var(--accent-color);
  }
  
  .send-button {
    background: var(--accent-color);
    border: none;
    border-radius: var(--radius-sm);
    color: white;
    cursor: pointer;
    padding: var(--space-sm);
    transition: all var(--transition-fast);
    font-size: var(--font-md);
  }
  
  .send-button:hover:not(:disabled) {
    background: var(--accent-hover);
  }
  
  .send-button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  
  /* Empty State */
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    text-align: center;
    padding: var(--space-xl);
    color: var(--text-secondary);
  }
  
  .empty-icon {
    font-size: 4rem;
    margin-bottom: var(--space-lg);
    opacity: 0.7;
  }
  
  .empty-state h3 {
    margin: 0 0 var(--space-md) 0;
    color: var(--text-primary);
  }
  
  .empty-state p {
    margin: 0 0 var(--space-xl) 0;
    font-size: var(--font-sm);
    line-height: 1.4;
  }
  
  /* Responsive adjustments */
  @media (max-width: 768px) {
    .action-buttons {
      grid-template-columns: 1fr;
    }
    
    .message {
      max-width: 95%;
    }
  }
</style>
import QtQuick 2.12
import QtQuick.Controls 2.12
import QtQuick.Layouts 1.12
import QtQuick.Window 2.12
import "components"
import "styles"

// This must match the uri and version
// specified in the qml_module in the build.rs script.
import com.uforge.demo 1.0

ApplicationWindow {
    id: mainWindow
    width: 1400
    height: 900
    minimumWidth: 800
    minimumHeight: 600
    title: qsTr("U-Forge - Knowledge Graph Editor")
    visible: true
    color: theme.backgroundColor

    // Theme instance
    Theme {
        id: theme
    }

    // Application state
    property bool projectOpen: false
    property string currentProjectName: ""
    property string currentProjectPath: ""

    // Panel visibility states
    property bool leftPanelVisible: true
    property bool rightPanelVisible: true
    property bool bottomPanelVisible: false

    // Panel sizes
    property real leftPanelWidth: 300
    property real rightPanelWidth: 350
    property real bottomPanelHeight: 300

    // Current editing state
    property string currentNodeId: ""
    property string currentNodeType: ""

    // Demo object for backend communication (placeholder)
    DemoObject {
        id: demoObject
        counter: 0
        message: qsTr("U-Forge Knowledge Graph Editor Ready")
    }

    // Main layout structure
    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // Top toolbar
        MainToolbar {
            id: toolbar
            Layout.fillWidth: true
            Layout.preferredHeight: theme.toolbarHeight

            projectOpen: mainWindow.projectOpen
            currentProjectName: mainWindow.currentProjectName

            // Tooltip functions
            showToolTip: mainWindow.showToolTip
            hideToolTip: mainWindow.hideToolTip

            onNewProjectRequested: {
                console.log("New project requested")
                // TODO: Implement new project creation
                // For now, just simulate project creation
                mainWindow.projectOpen = true
                mainWindow.currentProjectName = "New Project"
                mainWindow.currentProjectPath = "/path/to/new/project"
                updateAllPanels()
            }

            onOpenProjectRequested: {
                console.log("Open project requested")
                // TODO: Implement project opening dialog
                // For now, just simulate project opening
                mainWindow.projectOpen = true
                mainWindow.currentProjectName = "Sample Knowledge Base"
                mainWindow.currentProjectPath = "/path/to/sample/project"
                updateAllPanels()
            }

            onSaveProjectRequested: {
                console.log("Save project requested")
                // TODO: Implement project saving
                demoObject.logCurrentState()
            }

            onToggleLeftPanelRequested: {
                mainWindow.leftPanelVisible = !mainWindow.leftPanelVisible
                toolbar.updateLeftPanelState(mainWindow.leftPanelVisible)
            }

            onToggleBottomPanelRequested: {
                mainWindow.bottomPanelVisible = !mainWindow.bottomPanelVisible
                toolbar.updateBottomPanelState(mainWindow.bottomPanelVisible)
            }

            onToggleRightPanelRequested: {
                mainWindow.rightPanelVisible = !mainWindow.rightPanelVisible
                toolbar.updateRightPanelState(mainWindow.rightPanelVisible)
            }

            onSettingsRequested: {
                console.log("Settings requested")
                // TODO: Implement settings dialog
            }
        }

        // Main content area
        RowLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 0

            // Left panel (Explorer)
            Rectangle {
                id: leftPanelContainer
                Layout.preferredWidth: mainWindow.leftPanelVisible ? mainWindow.leftPanelWidth : 0
                Layout.fillHeight: true
                Layout.minimumWidth: mainWindow.leftPanelVisible ? theme.panelMinWidth : 0
                visible: mainWindow.leftPanelVisible
                color: "transparent"

                ExplorerPanel {
                    id: explorerPanel
                    anchors.fill: parent

                    projectOpen: mainWindow.projectOpen
                    currentProjectName: mainWindow.currentProjectName

                    onNodeSelected: function(nodeId, nodeType) {
                        console.log("Node selected:", nodeId, nodeType)
                        mainWindow.currentNodeId = nodeId
                        mainWindow.currentNodeType = nodeType
                        contentEditor.loadNode(nodeId, nodeType)
                    }

                    onNodeDoubleClicked: {
                        console.log("Node double-clicked:", nodeId, nodeType)
                        mainWindow.currentNodeId = nodeId
                        mainWindow.currentNodeType = nodeType
                        contentEditor.loadNode(nodeId, nodeType)
                        // Also focus on the node in the graph view
                        graphPanel.focusOnNode(nodeId)
                    }

                    onProjectSwitchRequested: {
                        console.log("Project switch requested:", projectPath)
                        // TODO: Implement project switching
                        mainWindow.projectOpen = true
                        mainWindow.currentProjectPath = projectPath
                        updateAllPanels()
                    }

                    onRefreshProjectRequested: {
                        console.log("Project refresh requested")
                        // TODO: Implement project refresh
                        explorerPanel.refreshTree()
                    }

                    onCreateNodeRequested: {
                        console.log("Create node requested:", nodeType, "parent:", parentId)
                        // TODO: Implement node creation
                        contentEditor.createNewNodeRequested(nodeType)
                    }

                    onDeleteNodeRequested: {
                        console.log("Delete node requested:", nodeId)
                        // TODO: Implement node deletion
                        contentEditor.deleteNodeRequested(nodeId)
                    }
                }

                // Resize handle for left panel
                Rectangle {
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                    width: 4
                    color: "transparent"

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.SizeHorCursor

                        property real startX: 0
                        property real startWidth: 0

                        onPressed: function(mouse) {
                            startX = mouse.x
                            startWidth = mainWindow.leftPanelWidth
                        }

                        onPositionChanged: function(mouse) {
                            if (pressed) {
                                var newWidth = startWidth + (mouse.x - startX)
                                mainWindow.leftPanelWidth = Math.max(theme.panelMinWidth, Math.min(600, newWidth))
                            }
                        }
                    }
                }
            }

            // Center area (Content Editor + Optional Bottom Panel)
            ColumnLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                spacing: 0

                // Main content editor
                ContentEditor {
                    id: contentEditor
                    Layout.fillWidth: true
                    Layout.fillHeight: true

                    projectOpen: mainWindow.projectOpen
                    hasCurrentNode: mainWindow.currentNodeId !== ""
                    currentNodeId: mainWindow.currentNodeId
                    currentNodeType: mainWindow.currentNodeType

                    onCreateNewNodeRequested: {
                        console.log("Create new node requested:", nodeType)
                        // TODO: Implement node creation
                        // Simulate node creation
                        var newNodeId = "node-" + Date.now()
                        mainWindow.currentNodeId = newNodeId
                        mainWindow.currentNodeType = nodeType
                        contentEditor.loadNode(newNodeId, nodeType)
                        demoObject.updateMessage("Created new " + nodeType + " node: " + newNodeId)
                    }

                    onOpenNodeRequested: {
                        console.log("Open node requested:", nodeId)
                        // TODO: Implement node opening
                        explorerPanel.selectNode(nodeId)
                    }

                    onSaveNodeRequested: {
                        console.log("Save node requested")
                        // TODO: Implement node saving
                        demoObject.logCurrentState()
                        contentEditor.isDirty = false
                    }

                    onDeleteNodeRequested: {
                        console.log("Delete node requested:", nodeId)
                        // TODO: Implement node deletion
                        contentEditor.clearNode()
                        mainWindow.currentNodeId = ""
                        mainWindow.currentNodeType = ""
                    }

                    onCreateNewProjectRequested: {
                        toolbar.newProjectRequested()
                    }

                    onOpenProjectRequested: {
                        toolbar.openProjectRequested()
                    }
                }

                // Bottom panel (Knowledge Graph) - collapsible
                Rectangle {
                    id: bottomPanelContainer
                    Layout.fillWidth: true
                    Layout.preferredHeight: mainWindow.bottomPanelVisible ? mainWindow.bottomPanelHeight : 0
                    Layout.minimumHeight: mainWindow.bottomPanelVisible ? theme.panelMinHeight : 0
                    visible: mainWindow.bottomPanelVisible
                    color: "transparent"

                    KnowledgeGraphPanel {
                        id: graphPanel
                        anchors.fill: parent

                        projectOpen: mainWindow.projectOpen
                        isCollapsed: !mainWindow.bottomPanelVisible

                        onNodeClicked: {
                            console.log("Graph node clicked:", nodeId, nodeType)
                            mainWindow.currentNodeId = nodeId
                            mainWindow.currentNodeType = nodeType
                            contentEditor.loadNode(nodeId, nodeType)
                            explorerPanel.selectNode(nodeId)
                        }

                        onNodeDoubleClicked: {
                            console.log("Graph node double-clicked:", nodeId, nodeType)
                            mainWindow.currentNodeId = nodeId
                            mainWindow.currentNodeType = nodeType
                            contentEditor.loadNode(nodeId, nodeType)
                            explorerPanel.selectNode(nodeId)
                        }

                        onEdgeClicked: {
                            console.log("Graph edge clicked:", sourceId, "->", targetId, "type:", edgeType)
                            // TODO: Implement edge editing
                        }

                        onNodeCreationRequested: {
                            console.log("Node creation requested at position:", position)
                            // TODO: Implement node creation at specific position
                            contentEditor.createNewNodeRequested("Note")
                        }

                        onLayoutChangeRequested: {
                            console.log("Layout change requested:", layoutType)
                            // TODO: Implement layout change
                            graphPanel.applyLayout(layoutType)
                        }

                        onExportGraphRequested: {
                            console.log("Graph export requested")
                            // TODO: Implement graph export
                        }
                    }

                    // Resize handle for bottom panel
                    Rectangle {
                        anchors.top: parent.top
                        anchors.left: parent.left
                        anchors.right: parent.right
                        height: 4
                        color: "transparent"

                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.SizeVerCursor

                            property real startY: 0
                            property real startHeight: 0

                            onPressed: function(mouse) {
                                startY = mouse.y
                                startHeight = mainWindow.bottomPanelHeight
                            }

                            onPositionChanged: function(mouse) {
                                if (pressed) {
                                    var newHeight = startHeight - (mouse.y - startY)
                                    mainWindow.bottomPanelHeight = Math.max(theme.panelMinHeight, Math.min(400, newHeight))
                                }
                            }
                        }
                    }
                }
            }

            // Right panel (AI Agent)
            Rectangle {
                id: rightPanelContainer
                Layout.preferredWidth: mainWindow.rightPanelVisible ? mainWindow.rightPanelWidth : 0
                Layout.fillHeight: true
                Layout.minimumWidth: mainWindow.rightPanelVisible ? theme.panelMinWidth : 0
                visible: mainWindow.rightPanelVisible
                color: "transparent"

                AIAgentPanel {
                    id: aiAgentPanel
                    anchors.fill: parent

                    projectOpen: mainWindow.projectOpen

                    onMessageSubmitted: {
                        console.log("AI message submitted:", message, "type:", agentType)
                        // TODO: Implement AI message processing
                        demoObject.updateMessage("AI Query: " + message)
                        demoObject.logCurrentState()
                    }

                    onConversationCleared: {
                        console.log("AI conversation cleared")
                        // TODO: Implement conversation clearing
                    }

                    onAgentSettingsRequested: {
                        console.log("AI agent settings requested")
                        // TODO: Implement AI settings dialog
                    }

                    onKnowledgeGraphQueryRequested: {
                        console.log("Knowledge graph query requested:", query)
                        // TODO: Implement knowledge graph querying
                        graphPanel.setLoading(true)
                        // Simulate query processing
                        Qt.callLater(function() {
                            graphPanel.setLoading(false)
                            aiAgentPanel.addBotResponse("I found several relevant connections in your knowledge graph related to: " + query)
                        })
                    }

                    onNodeContextRequested: {
                        console.log("Node context requested:", nodeId)
                        // TODO: Implement node context retrieval
                        explorerPanel.selectNode(nodeId)
                        contentEditor.loadNode(nodeId, "Unknown")
                    }
                }

                // Resize handle for right panel
                Rectangle {
                    anchors.left: parent.left
                    anchors.top: parent.top
                    anchors.bottom: parent.bottom
                    width: 4
                    color: "transparent"

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.SizeHorCursor

                        property real startX: 0
                        property real startWidth: 0

                        onPressed: function(mouse) {
                            startX = mouse.x
                            startWidth = mainWindow.rightPanelWidth
                        }

                        onPositionChanged: function(mouse) {
                            if (pressed) {
                                var newWidth = startWidth - (mouse.x - startX)
                                mainWindow.rightPanelWidth = Math.max(theme.panelMinWidth, Math.min(600, newWidth))
                            }
                        }
                    }
                }
            }
        }
    }

    // Keyboard shortcuts
    Shortcut {
        sequence: "Ctrl+N"
        onActivated: toolbar.newProjectRequested()
    }

    Shortcut {
        sequence: "Ctrl+O"
        onActivated: toolbar.openProjectRequested()
    }

    Shortcut {
        sequence: "Ctrl+S"
        onActivated: toolbar.saveProjectRequested()
    }

    Shortcut {
        sequence: "Ctrl+Shift+E"
        onActivated: toolbar.toggleLeftPanelRequested()
    }

    Shortcut {
        sequence: "Ctrl+`"
        onActivated: toolbar.toggleBottomPanelRequested()
    }

    Shortcut {
        sequence: "Ctrl+Shift+A"
        onActivated: toolbar.toggleRightPanelRequested()
    }

    Shortcut {
        sequence: "Ctrl+,"
        onActivated: toolbar.settingsRequested()
    }

    Shortcut {
        sequence: "F1"
        onActivated: aiAgentPanel.focusInput()
    }

    Shortcut {
        sequence: "Escape"
        onActivated: {
            // Clear current selections
            mainWindow.currentNodeId = ""
            mainWindow.currentNodeType = ""
            contentEditor.clearNode()
        }
    }

    // Helper functions
    function updateAllPanels() {
        explorerPanel.updateProjectState(mainWindow.projectOpen, mainWindow.currentProjectName)
        contentEditor.setProjectState(mainWindow.projectOpen)
        graphPanel.setProjectState(mainWindow.projectOpen)
        aiAgentPanel.setProjectState(mainWindow.projectOpen)
        toolbar.updateProjectState(mainWindow.projectOpen, mainWindow.currentProjectName)
    }

    function loadProject(projectPath, projectName) {
        mainWindow.projectOpen = true
        mainWindow.currentProjectPath = projectPath
        mainWindow.currentProjectName = projectName
        updateAllPanels()

        // Simulate loading project data
        demoObject.updateMessage("Loaded project: " + projectName)
        demoObject.logCurrentState()
    }

    function closeProject() {
        mainWindow.projectOpen = false
        mainWindow.currentProjectPath = ""
        mainWindow.currentProjectName = ""
        mainWindow.currentNodeId = ""
        mainWindow.currentNodeType = ""
        updateAllPanels()

        demoObject.updateMessage("Project closed")
        demoObject.logCurrentState()
    }

    // Initialize with demo state
    Component.onCompleted: {
        console.log("U-Forge Knowledge Graph Editor initialized")

        // Initialize toolbar state
        toolbar.updateLeftPanelState(mainWindow.leftPanelVisible)
        toolbar.updateBottomPanelState(mainWindow.bottomPanelVisible)
        toolbar.updateRightPanelState(mainWindow.rightPanelVisible)

        // Log initial state
        demoObject.logCurrentState()
    }

    // Global tooltip manager
    CustomToolTip {
        id: globalToolTip
        anchors.fill: parent
    }

    // Global tooltip functions
    function showToolTip(x, y, text) {
        globalToolTip.text = text
        globalToolTip.showAt(x, y)
    }

    function hideToolTip() {
        globalToolTip.hide()
    }

    // Window state management
    onClosing: {
        console.log("Application closing...")
        // TODO: Implement proper cleanup and save state
        if (mainWindow.projectOpen && contentEditor.isDirty) {
            // TODO: Show save dialog
            console.log("Warning: Unsaved changes detected")
        }
    }
}

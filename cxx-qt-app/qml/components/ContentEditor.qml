import QtQuick 2.12
import QtQuick.Controls 2.12
import QtQuick.Layouts 1.12
import "../styles"

Rectangle {
    id: contentEditor
    color: theme.editorColor
    border.color: theme.borderColor
    border.width: 1

    // Theme instance
    Theme {
        id: theme
    }

    // Editor state
    property bool projectOpen: false
    property bool hasCurrentNode: false
    property string currentNodeId: ""
    property string currentNodeType: ""
    property bool isYamlMode: false
    property bool isDirty: false

    // Signals for backend communication
    signal createNewNodeRequested(string nodeType)
    signal openNodeRequested(string nodeId)
    signal saveNodeRequested()
    signal deleteNodeRequested(string nodeId)
    signal createNewProjectRequested()
    signal openProjectRequested()

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: theme.paddingNormal
        spacing: 0

        // Header with mode toggle and actions
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 40
            color: theme.panelHeaderColor
            border.color: theme.borderColor
            border.width: 1
            radius: theme.borderRadius

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: theme.paddingNormal
                anchors.rightMargin: theme.paddingNormal
                spacing: theme.paddingMedium

                // Title
                Text {
                    text: contentEditor.hasCurrentNode ?
                          `Node Editor - ${contentEditor.currentNodeType}` :
                          "Content Editor"
                    color: theme.primaryTextColor
                    font.pixelSize: theme.fontSizeMedium
                    font.bold: true
                    Layout.fillWidth: true
                }

                // Mode toggle (only visible when editing a node)
                RowLayout {
                    visible: contentEditor.hasCurrentNode
                    spacing: 2

                    Button {
                        text: "Fields"
                        font.pixelSize: theme.fontSizeSmall
                        checkable: true
                        checked: !contentEditor.isYamlMode
                        enabled: contentEditor.hasCurrentNode

                        background: Rectangle {
                            color: parent.checked ? theme.primaryColor :
                                   (parent.hovered ? theme.hoverColor : theme.surfaceColor)
                            border.color: theme.borderColor
                            border.width: 1
                            radius: theme.borderRadius
                        }

                        contentItem: Text {
                            text: parent.text
                            color: theme.primaryTextColor
                            font: parent.font
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }

                        onClicked: contentEditor.isYamlMode = false
                    }

                    Button {
                        text: "YAML"
                        font.pixelSize: theme.fontSizeSmall
                        checkable: true
                        checked: contentEditor.isYamlMode
                        enabled: contentEditor.hasCurrentNode

                        background: Rectangle {
                            color: parent.checked ? theme.primaryColor :
                                   (parent.hovered ? theme.hoverColor : theme.surfaceColor)
                            border.color: theme.borderColor
                            border.width: 1
                            radius: theme.borderRadius
                        }

                        contentItem: Text {
                            text: parent.text
                            color: theme.primaryTextColor
                            font: parent.font
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }

                        onClicked: contentEditor.isYamlMode = true
                    }
                }

                // Action buttons (only visible when editing a node)
                RowLayout {
                    visible: contentEditor.hasCurrentNode
                    spacing: theme.paddingSmall

                    Button {
                        text: "Save"
                        font.pixelSize: theme.fontSizeSmall
                        enabled: contentEditor.isDirty

                        background: Rectangle {
                            color: parent.enabled ?
                                   (parent.hovered ? theme.primaryHoverColor : theme.primaryColor) :
                                   theme.surfaceColor
                            border.color: theme.borderColor
                            border.width: 1
                            radius: theme.borderRadius
                            opacity: parent.enabled ? 1.0 : 0.5
                        }

                        contentItem: Text {
                            text: parent.text
                            color: theme.primaryTextColor
                            font: parent.font
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }

                        onClicked: contentEditor.saveNodeRequested()
                    }

                    Button {
                        text: "Delete"
                        font.pixelSize: theme.fontSizeSmall

                        background: Rectangle {
                            color: parent.hovered ? theme.errorColor : theme.surfaceColor
                            border.color: theme.errorColor
                            border.width: 1
                            radius: theme.borderRadius
                        }

                        contentItem: Text {
                            text: parent.text
                            color: theme.primaryTextColor
                            font: parent.font
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }

                        onClicked: contentEditor.deleteNodeRequested(contentEditor.currentNodeId)
                    }
                }
            }
        }

        // Main content area
        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.topMargin: theme.paddingNormal

            // No project state
            Rectangle {
                id: noProjectView
                anchors.fill: parent
                color: "transparent"
                visible: !contentEditor.projectOpen

                ColumnLayout {
                    anchors.centerIn: parent
                    spacing: theme.paddingLarge
                    width: Math.min(parent.width * 0.6, 400)

                    Text {
                        text: "Welcome to U-Forge"
                        font.pixelSize: theme.fontSizeXLarge
                        font.bold: true
                        color: theme.primaryTextColor
                        Layout.alignment: Qt.AlignCenter
                    }

                    Text {
                        text: "Get started by creating a new project or opening an existing one."
                        font.pixelSize: theme.fontSizeNormal
                        color: theme.secondaryTextColor
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                        horizontalAlignment: Text.AlignHCenter
                    }

                    RowLayout {
                        spacing: theme.paddingMedium
                        Layout.alignment: Qt.AlignCenter

                        Button {
                            text: "Create New Project"
                            font.pixelSize: theme.fontSizeMedium

                            background: Rectangle {
                                color: parent.hovered ? theme.primaryHoverColor : theme.primaryColor
                                radius: theme.borderRadius
                            }

                            contentItem: Text {
                                text: parent.text
                                color: "white"
                                font: parent.font
                                horizontalAlignment: Text.AlignHCenter
                                verticalAlignment: Text.AlignVCenter
                            }

                            onClicked: contentEditor.createNewProjectRequested()
                        }

                        Button {
                            text: "Open Project"
                            font.pixelSize: theme.fontSizeMedium

                            background: Rectangle {
                                color: parent.hovered ? theme.hoverColor : theme.surfaceColor
                                border.color: theme.borderColor
                                border.width: 1
                                radius: theme.borderRadius
                            }

                            contentItem: Text {
                                text: parent.text
                                color: theme.primaryTextColor
                                font: parent.font
                                horizontalAlignment: Text.AlignHCenter
                                verticalAlignment: Text.AlignVCenter
                            }

                            onClicked: contentEditor.openProjectRequested()
                        }
                    }
                }
            }

            // Project open but no node selected
            Rectangle {
                id: noNodeView
                anchors.fill: parent
                color: "transparent"
                visible: contentEditor.projectOpen && !contentEditor.hasCurrentNode

                ColumnLayout {
                    anchors.centerIn: parent
                    spacing: theme.paddingLarge
                    width: Math.min(parent.width * 0.6, 400)

                    Text {
                        text: "Create or Select a Node"
                        font.pixelSize: theme.fontSizeLarge
                        font.bold: true
                        color: theme.primaryTextColor
                        Layout.alignment: Qt.AlignCenter
                    }

                    Text {
                        text: "Start building your knowledge graph by creating a new node or selecting an existing one from the Explorer panel."
                        font.pixelSize: theme.fontSizeNormal
                        color: theme.secondaryTextColor
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                        horizontalAlignment: Text.AlignHCenter
                    }

                    // Quick create buttons for common node types
                    ColumnLayout {
                        spacing: theme.paddingSmall
                        Layout.alignment: Qt.AlignCenter

                        Text {
                            text: "Quick Create:"
                            font.pixelSize: theme.fontSizeSmall
                            color: theme.mutedTextColor
                            Layout.alignment: Qt.AlignCenter
                        }

                        RowLayout {
                            spacing: theme.paddingSmall
                            Layout.alignment: Qt.AlignCenter

                            Button {
                                text: "Document"
                                font.pixelSize: theme.fontSizeSmall
                                onClicked: contentEditor.createNewNodeRequested("Document")

                                background: Rectangle {
                                    color: parent.hovered ? theme.hoverColor : theme.surfaceColor
                                    border.color: theme.borderColor
                                    border.width: 1
                                    radius: theme.borderRadius
                                }

                                contentItem: Text {
                                    text: parent.text
                                    color: theme.primaryTextColor
                                    font: parent.font
                                    horizontalAlignment: Text.AlignHCenter
                                    verticalAlignment: Text.AlignVCenter
                                }
                            }

                            Button {
                                text: "Concept"
                                font.pixelSize: theme.fontSizeSmall
                                onClicked: contentEditor.createNewNodeRequested("Concept")

                                background: Rectangle {
                                    color: parent.hovered ? theme.hoverColor : theme.surfaceColor
                                    border.color: theme.borderColor
                                    border.width: 1
                                    radius: theme.borderRadius
                                }

                                contentItem: Text {
                                    text: parent.text
                                    color: theme.primaryTextColor
                                    font: parent.font
                                    horizontalAlignment: Text.AlignHCenter
                                    verticalAlignment: Text.AlignVCenter
                                }
                            }

                            Button {
                                text: "Note"
                                font.pixelSize: theme.fontSizeSmall
                                onClicked: contentEditor.createNewNodeRequested("Note")

                                background: Rectangle {
                                    color: parent.hovered ? theme.hoverColor : theme.surfaceColor
                                    border.color: theme.borderColor
                                    border.width: 1
                                    radius: theme.borderRadius
                                }

                                contentItem: Text {
                                    text: parent.text
                                    color: theme.primaryTextColor
                                    font: parent.font
                                    horizontalAlignment: Text.AlignHCenter
                                    verticalAlignment: Text.AlignVCenter
                                }
                            }
                        }
                    }
                }
            }

            // Field-based editing mode
            ScrollView {
                id: fieldEditor
                anchors.fill: parent
                visible: contentEditor.hasCurrentNode && !contentEditor.isYamlMode
                ScrollBar.horizontal.policy: ScrollBar.AsNeeded
                ScrollBar.vertical.policy: ScrollBar.AsNeeded

                ColumnLayout {
                    width: fieldEditor.width
                    spacing: theme.paddingMedium

                    // Node metadata section
                    GroupBox {
                        Layout.fillWidth: true
                        title: "Node Information"
                        font.pixelSize: theme.fontSizeMedium

                        background: Rectangle {
                            color: theme.surfaceColor
                            border.color: theme.borderColor
                            border.width: 1
                            radius: theme.borderRadius
                        }

                        label: Text {
                            text: parent.title
                            color: theme.primaryTextColor
                            font: parent.font
                        }

                        GridLayout {
                            anchors.fill: parent
                            columns: 2
                            rowSpacing: theme.paddingSmall
                            columnSpacing: theme.paddingMedium

                            Text {
                                text: "ID:"
                                color: theme.secondaryTextColor
                                font.pixelSize: theme.fontSizeSmall
                            }
                            TextField {
                                Layout.fillWidth: true
                                text: contentEditor.currentNodeId
                                readOnly: true
                                font.pixelSize: theme.fontSizeSmall
                                color: theme.mutedTextColor
                                background: Rectangle {
                                    color: theme.backgroundColor
                                    border.color: theme.borderColor
                                    border.width: 1
                                    radius: theme.borderRadius
                                }
                            }

                            Text {
                                text: "Type:"
                                color: theme.secondaryTextColor
                                font.pixelSize: theme.fontSizeSmall
                            }
                            ComboBox {
                                Layout.fillWidth: true
                                model: ["Document", "Concept", "Note", "Entity", "Custom"]
                                currentIndex: {
                                    var types = ["Document", "Concept", "Note", "Entity", "Custom"]
                                    return types.indexOf(contentEditor.currentNodeType)
                                }
                                font.pixelSize: theme.fontSizeSmall
                                // TODO: Connect to backend for type change
                                onCurrentTextChanged: {
                                    contentEditor.isDirty = true
                                    // Backend integration: Update node type
                                }
                            }
                        }
                    }

                    // Dynamic fields section (placeholder)
                    GroupBox {
                        Layout.fillWidth: true
                        title: "Properties"
                        font.pixelSize: theme.fontSizeMedium

                        background: Rectangle {
                            color: theme.surfaceColor
                            border.color: theme.borderColor
                            border.width: 1
                            radius: theme.borderRadius
                        }

                        label: Text {
                            text: parent.title
                            color: theme.primaryTextColor
                            font: parent.font
                        }

                        ColumnLayout {
                            anchors.fill: parent
                            spacing: theme.paddingSmall

                            // Title field
                            RowLayout {
                                Layout.fillWidth: true
                                Text {
                                    text: "Title:"
                                    color: theme.secondaryTextColor
                                    font.pixelSize: theme.fontSizeSmall
                                    Layout.preferredWidth: 80
                                }
                                TextField {
                                    Layout.fillWidth: true
                                    placeholderText: "Enter node title..."
                                    font.pixelSize: theme.fontSizeSmall
                                    color: theme.primaryTextColor
                                    onTextChanged: contentEditor.isDirty = true
                                    background: Rectangle {
                                        color: theme.backgroundColor
                                        border.color: theme.borderColor
                                        border.width: 1
                                        radius: theme.borderRadius
                                    }
                                }
                            }

                            // Content field
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: theme.paddingTiny

                                Text {
                                    text: "Content:"
                                    color: theme.secondaryTextColor
                                    font.pixelSize: theme.fontSizeSmall
                                }
                                ScrollView {
                                    Layout.fillWidth: true
                                    Layout.preferredHeight: 200
                                    TextArea {
                                        placeholderText: "Enter node content..."
                                        font.pixelSize: theme.fontSizeSmall
                                        color: theme.primaryTextColor
                                        wrapMode: TextArea.Wrap
                                        onTextChanged: contentEditor.isDirty = true
                                        background: Rectangle {
                                            color: theme.backgroundColor
                                            border.color: theme.borderColor
                                            border.width: 1
                                            radius: theme.borderRadius
                                        }
                                    }
                                }
                            }

                            // Tags field
                            RowLayout {
                                Layout.fillWidth: true
                                Text {
                                    text: "Tags:"
                                    color: theme.secondaryTextColor
                                    font.pixelSize: theme.fontSizeSmall
                                    Layout.preferredWidth: 80
                                }
                                TextField {
                                    Layout.fillWidth: true
                                    placeholderText: "tag1, tag2, tag3..."
                                    font.pixelSize: theme.fontSizeSmall
                                    color: theme.primaryTextColor
                                    onTextChanged: contentEditor.isDirty = true
                                    background: Rectangle {
                                        color: theme.backgroundColor
                                        border.color: theme.borderColor
                                        border.width: 1
                                        radius: theme.borderRadius
                                    }
                                }
                            }

                        }
                    }

                    // Relationships section (placeholder)
                    GroupBox {
                        Layout.fillWidth: true
                        title: "Relationships"
                        font.pixelSize: theme.fontSizeMedium

                        background: Rectangle {
                            color: theme.surfaceColor
                            border.color: theme.borderColor
                            border.width: 1
                            radius: theme.borderRadius
                        }

                        label: Text {
                            text: parent.title
                            color: theme.primaryTextColor
                            font: parent.font
                        }

                        ColumnLayout {
                            anchors.fill: parent
                            spacing: theme.paddingSmall

                            Text {
                                text: "Connected nodes and relationships will appear here."
                                color: theme.mutedTextColor
                                font.pixelSize: theme.fontSizeSmall
                                font.italic: true
                            }

                            Button {
                                text: "Add Relationship"
                                enabled: false // TODO: Enable when backend is connected
                                font.pixelSize: theme.fontSizeSmall
                                Layout.alignment: Qt.AlignLeft

                                background: Rectangle {
                                    color: parent.enabled ?
                                           (parent.hovered ? theme.primaryHoverColor : theme.primaryColor) :
                                           theme.surfaceColor
                                    border.color: theme.borderColor
                                    border.width: 1
                                    radius: theme.borderRadius
                                    opacity: parent.enabled ? 1.0 : 0.5
                                }

                                contentItem: Text {
                                    text: parent.text
                                    color: theme.primaryTextColor
                                    font: parent.font
                                    horizontalAlignment: Text.AlignHCenter
                                    verticalAlignment: Text.AlignVCenter
                                }
                            }
                        }
                    }
                }
            }

            // YAML editing mode
            Rectangle {
                id: yamlEditor
                anchors.fill: parent
                visible: contentEditor.hasCurrentNode && contentEditor.isYamlMode
                color: theme.backgroundColor
                border.color: theme.borderColor
                border.width: 1
                radius: theme.borderRadius

                ScrollView {
                    anchors.fill: parent
                    anchors.margins: theme.paddingSmall

                    TextArea {
                        id: yamlTextArea
                        font.family: theme.monospaceFontFamily
                        font.pixelSize: theme.fontSizeSmall
                        color: theme.primaryTextColor
                        selectByMouse: true
                        wrapMode: TextArea.Wrap

                        // Placeholder YAML content
                        text: `# Node: ${contentEditor.currentNodeId}
# Type: ${contentEditor.currentNodeType}
id: "${contentEditor.currentNodeId}"
type: "${contentEditor.currentNodeType}"
title: "Sample Node Title"
content: |
  This is the content of the node.
  Multiple lines are supported.
tags:
  - sample
  - node
  - placeholder
created_at: "2024-01-01T00:00:00Z"
updated_at: "2024-01-01T00:00:00Z"
relationships:
  - target: "node-456"
    type: "relates_to"
    description: "Related concept"
metadata:
  author: "user"
  version: 1`

                        onTextChanged: contentEditor.isDirty = true

                        background: Rectangle {
                            color: theme.backgroundColor
                        }
                    }
                }
            }
        }
    }

    // Methods for external control
    function loadNode(nodeId, nodeType) {
        contentEditor.currentNodeId = nodeId
        contentEditor.currentNodeType = nodeType
        contentEditor.hasCurrentNode = true
        contentEditor.isDirty = false
        // TODO: Load actual node data from backend
    }

    function clearNode() {
        contentEditor.currentNodeId = ""
        contentEditor.currentNodeType = ""
        contentEditor.hasCurrentNode = false
        contentEditor.isDirty = false
    }

    function setProjectState(isOpen) {
        contentEditor.projectOpen = isOpen
        if (!isOpen) {
            clearNode()
        }
    }
}

import QtQuick 2.12
import QtQuick.Controls 2.12
import QtQuick.Layouts 1.12
import "../styles"

Rectangle {
    id: explorerPanel
    color: "#252526"
    border.color: "#3e3e42"
    border.width: 1

    // Theme instance
    Theme {
        id: theme
    }

    // Panel state
    property bool projectOpen: false
    property string currentProjectName: ""
    property bool isCollapsed: false
    property real minimumWidth: 200

    // Signals for main window communication
    signal nodeSelected(string nodeId, string nodeType)
    signal nodeDoubleClicked(string nodeId, string nodeType)
    signal projectSwitchRequested(string projectPath)
    signal refreshProjectRequested()
    signal createNodeRequested(string nodeType, string parentId)
    signal deleteNodeRequested(string nodeId)

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 4
        spacing: 0

        // Header
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 32
            color: "#252526"
            border.color: "#3e3e42"
            border.width: 1
            radius: 4

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: 8
                anchors.rightMargin: 4
                spacing: 4

                Text {
                    text: "EXPLORER"
                    font.pixelSize: 11
                    font.bold: true
                    color: "#cccccc"
                    Layout.fillWidth: true
                }

                // Refresh button
                ToolButton {
                    width: 20
                    height: 20
                    enabled: explorerPanel.projectOpen

                    background: Rectangle {
                        color: refreshArea.containsMouse ? "#2a2d2e" : "transparent"
                        border.color: "transparent"
                        radius: 2
                        opacity: parent.enabled ? 1.0 : 0.5
                    }

                    contentItem: Text {
                        text: "↻"
                        color: "#cccccc"
                        font.pixelSize: 11
                        horizontalAlignment: Text.AlignHCenter
                        verticalAlignment: Text.AlignVCenter
                    }

                    MouseArea {
                        id: refreshArea
                        anchors.fill: parent
                        hoverEnabled: true
                        enabled: parent.enabled
                        onClicked: {
                            console.log("Refresh project")
                            // TODO: Implement refresh logic
                        }
                        // Tooltips temporarily disabled
                        // onEntered: {
                        //     var pos = mapToItem(explorerPanel.parent, parent.x + parent.width/2, parent.y)
                        //     explorerPanel.parent.showToolTip(pos.x, pos.y, "Refresh project")
                        // }
                        // onExited: explorerPanel.parent.hideToolTip()
                    }
                }

                // Collapse button
                ToolButton {
                    width: 20
                    height: 20

                    background: Rectangle {
                        color: collapseArea.containsMouse ? "#2a2d2e" : "transparent"
                        border.color: "transparent"
                        radius: 2
                    }

                    contentItem: Text {
                        text: "<"
                        color: "#cccccc"
                        font.pixelSize: 11
                        horizontalAlignment: Text.AlignHCenter
                        verticalAlignment: Text.AlignVCenter
                    }

                    MouseArea {
                        id: collapseArea
                        anchors.fill: parent
                        hoverEnabled: true
                        onClicked: {
                            console.log("Collapse panel requested")
                        }
                        // Tooltips temporarily disabled
                        // onEntered: {
                        //     var pos = mapToItem(explorerPanel.parent, parent.x + parent.width/2, parent.y)
                        //     explorerPanel.parent.showToolTip(pos.x, pos.y, "Collapse panel")
                        // }
                        // onExited: explorerPanel.parent.hideToolTip()
                    }
                }
            }
        }

        // Main content area
        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.topMargin: 4

            // No project state
            Rectangle {
                id: noProjectView
                anchors.fill: parent
                color: "transparent"
                visible: !explorerPanel.projectOpen

                ColumnLayout {
                    anchors.centerIn: parent
                    anchors.margins: 12
                    spacing: 12
                    width: parent.width - 24

                    Text {
                        text: "No Project Open"
                        font.pixelSize: 14
                        font.bold: true
                        color: "#9d9d9d"
                        Layout.alignment: Qt.AlignCenter
                        horizontalAlignment: Text.AlignHCenter
                    }

                    Text {
                        text: "Open a project to browse its knowledge graph structure."
                        font.pixelSize: 11
                        color: "#6a6a6a"
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                        horizontalAlignment: Text.AlignHCenter
                    }
                }
            }

            // Project tree view (placeholder)
            Rectangle {
                anchors.fill: parent
                visible: explorerPanel.projectOpen
                color: "transparent"

                ColumnLayout {
                    anchors.fill: parent
                    spacing: 4

                    Text {
                        text: "📁 Documents"
                        font.pixelSize: 13
                        color: "#cccccc"
                        Layout.fillWidth: true
                        leftPadding: 8

                        MouseArea {
                            anchors.fill: parent
                            onClicked: explorerPanel.nodeSelected("documents", "folder")
                        }
                    }

                    Text {
                        text: "    📄 Meeting Notes"
                        font.pixelSize: 13
                        color: "#cccccc"
                        Layout.fillWidth: true
                        leftPadding: 16

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            onEntered: parent.color = "#4fc3f7"
                            onExited: parent.color = "#cccccc"
                            onClicked: explorerPanel.nodeSelected("doc-001", "Document")
                            onDoubleClicked: explorerPanel.nodeDoubleClicked("doc-001", "Document")
                        }
                    }

                    Text {
                        text: "    📄 Project Spec"
                        font.pixelSize: 13
                        color: "#cccccc"
                        Layout.fillWidth: true
                        leftPadding: 16

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            onEntered: parent.color = "#4fc3f7"
                            onExited: parent.color = "#cccccc"
                            onClicked: explorerPanel.nodeSelected("doc-002", "Document")
                            onDoubleClicked: explorerPanel.nodeDoubleClicked("doc-002", "Document")
                        }
                    }

                    Text {
                        text: "📁 Concepts"
                        font.pixelSize: 13
                        color: "#cccccc"
                        Layout.fillWidth: true
                        leftPadding: 8

                        MouseArea {
                            anchors.fill: parent
                            onClicked: explorerPanel.nodeSelected("concepts", "folder")
                        }
                    }

                    Text {
                        text: "    💡 Machine Learning"
                        font.pixelSize: 13
                        color: "#cccccc"
                        Layout.fillWidth: true
                        leftPadding: 16

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            onEntered: parent.color = "#66bb6a"
                            onExited: parent.color = "#cccccc"
                            onClicked: explorerPanel.nodeSelected("concept-001", "Concept")
                            onDoubleClicked: explorerPanel.nodeDoubleClicked("concept-001", "Concept")
                        }
                    }

                    Text {
                        text: "    💡 Software Architecture"
                        font.pixelSize: 13
                        color: "#cccccc"
                        Layout.fillWidth: true
                        leftPadding: 16

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            onEntered: parent.color = "#66bb6a"
                            onExited: parent.color = "#cccccc"
                            onClicked: explorerPanel.nodeSelected("concept-002", "Concept")
                            onDoubleClicked: explorerPanel.nodeDoubleClicked("concept-002", "Concept")
                        }
                    }

                    Text {
                        text: "📁 Notes"
                        font.pixelSize: 13
                        color: "#cccccc"
                        Layout.fillWidth: true
                        leftPadding: 8

                        MouseArea {
                            anchors.fill: parent
                            onClicked: explorerPanel.nodeSelected("notes", "folder")
                        }
                    }

                    Text {
                        text: "    📝 Quick Thought"
                        font.pixelSize: 13
                        color: "#cccccc"
                        Layout.fillWidth: true
                        leftPadding: 16

                        MouseArea {
                            anchors.fill: parent
                            hoverEnabled: true
                            onEntered: parent.color = "#ffa726"
                            onExited: parent.color = "#cccccc"
                            onClicked: explorerPanel.nodeSelected("note-001", "Note")
                            onDoubleClicked: explorerPanel.nodeDoubleClicked("note-001", "Note")
                        }
                    }

                    Item {
                        Layout.fillHeight: true
                    }
                }
            }
        }
    }

    // Public methods
    function updateProjectState(isOpen, projectName) {
        explorerPanel.projectOpen = isOpen
        explorerPanel.currentProjectName = projectName || ""
    }

    function selectNode(nodeId) {
        // TODO: Find and select node in tree view
        console.log("Selecting node:", nodeId)
    }

    function refreshTree() {
        // TODO: Refresh tree model with current project data
        console.log("Refreshing tree")
    }

    function expandToNode(nodeId) {
        // TODO: Expand tree path to show specific node
        console.log("Expanding to node:", nodeId)
    }
}

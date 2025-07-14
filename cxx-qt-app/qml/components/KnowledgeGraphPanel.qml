import QtQuick 2.12
import QtQuick.Controls 2.12
import QtQuick.Layouts 1.12
import "../styles"

Rectangle {
    id: graphPanel
    color: "#0c0c0c"
    border.color: "#3e3e42"
    border.width: 1

    // Theme instance
    Theme {
        id: theme
    }

    // Panel state
    property bool projectOpen: false
    property bool isCollapsed: true
    property real minimumHeight: 100
    property bool isLoading: false
    property int nodeCount: 0
    property int edgeCount: 0

    // Graph interaction state
    property string selectedNodeId: ""
    property real zoomLevel: 1.0
    property point panOffset: Qt.point(0, 0)
    property bool showNodeLabels: true
    property bool showEdgeLabels: false
    property string layoutMode: "force"

    // Signals for main window communication
    signal nodeClicked(string nodeId, string nodeType)
    signal nodeDoubleClicked(string nodeId, string nodeType)
    signal edgeClicked(string sourceId, string targetId, string edgeType)
    signal nodeCreationRequested(point position)
    signal layoutChangeRequested(string layoutType)
    signal exportGraphRequested()

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // Header with controls
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 35
            color: "#252526"
            border.color: "#3e3e42"
            border.width: 1

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: 8
                anchors.rightMargin: 4
                spacing: 4

                Text {
                    text: "KNOWLEDGE GRAPH"
                    font.pixelSize: 11
                    font.bold: true
                    color: "#cccccc"
                }

                Rectangle {
                    width: 1
                    height: parent.height * 0.6
                    color: "#3e3e42"
                    Layout.leftMargin: 4
                }

                // Node/Edge count display
                Text {
                    text: `${graphPanel.nodeCount} nodes, ${graphPanel.edgeCount} edges`
                    font.pixelSize: 10
                    color: "#9d9d9d"
                    visible: graphPanel.projectOpen && !graphPanel.isLoading
                }

                // Loading indicator
                BusyIndicator {
                    width: 16
                    height: 16
                    running: graphPanel.isLoading
                    visible: graphPanel.isLoading
                }

                Item { Layout.fillWidth: true }

                // Graph controls
                RowLayout {
                    spacing: 2

                    Button {
                        text: "Labels"
                        font.pixelSize: 10
                        checkable: true
                        checked: graphPanel.showNodeLabels
                        enabled: graphPanel.projectOpen

                        background: Rectangle {
                            color: parent.checked ? "#094771" :
                                   (parent.hovered ? "#2a2d2e" : "transparent")
                            radius: 4
                        }

                        contentItem: Text {
                            text: parent.text
                            color: "#cccccc"
                            font: parent.font
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }

                        onClicked: graphPanel.showNodeLabels = checked
                    }

                    Button {
                        text: "Export"
                        font.pixelSize: 10
                        enabled: graphPanel.projectOpen && !graphPanel.isLoading

                        background: Rectangle {
                            color: parent.hovered ? "#2a2d2e" : "transparent"
                            radius: 4
                        }

                        contentItem: Text {
                            text: parent.text
                            color: "#cccccc"
                            font: parent.font
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }

                        onClicked: graphPanel.exportGraphRequested()
                    }

                    Text {
                        text: `${Math.round(graphPanel.zoomLevel * 100)}%`
                        font.pixelSize: 10
                        color: "#9d9d9d"
                        Layout.preferredWidth: 35
                        horizontalAlignment: Text.AlignHCenter
                    }

                    // Collapse button
                    ToolButton {
                        text: "⌄"
                        font.pixelSize: 11

                        background: Rectangle {
                            color: collapseGraphArea.containsMouse ? "#2a2d2e" : "transparent"
                            border.color: "transparent"
                            radius: 2
                        }

                        contentItem: Text {
                            text: parent.text
                            color: "#cccccc"
                            font: parent.font
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }

                        MouseArea {
                            id: collapseGraphArea
                            anchors.fill: parent
                            hoverEnabled: true
                            onClicked: {
                                console.log("Collapse graph panel")
                                // TODO: Implement collapse logic
                            }
                            // Tooltips temporarily disabled
                            // onEntered: {
                            //     var pos = mapToItem(graphPanel.parent, parent.x + parent.width/2, parent.y)
                            //     graphPanel.parent.showToolTip(pos.x, pos.y, "Collapse panel")
                            // }
                            // onExited: graphPanel.parent.hideToolTip()
                        }
                    }
                }
            }
        }

        // Main graph visualization area
        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true

            // No project state
            Rectangle {
                id: noProjectView
                anchors.fill: parent
                color: "transparent"
                visible: !graphPanel.projectOpen

                ColumnLayout {
                    anchors.centerIn: parent
                    spacing: 12

                    Text {
                        text: "📊"
                        font.pixelSize: 48
                        color: "#6a6a6a"
                        Layout.alignment: Qt.AlignCenter
                    }

                    Text {
                        text: "Knowledge Graph Visualization"
                        font.pixelSize: 14
                        font.bold: true
                        color: "#9d9d9d"
                        Layout.alignment: Qt.AlignCenter
                    }

                    Text {
                        text: "Open a project to visualize its knowledge graph structure."
                        font.pixelSize: 11
                        color: "#6a6a6a"
                        Layout.alignment: Qt.AlignCenter
                    }
                }
            }

            // Graph canvas
            Rectangle {
                id: graphCanvas
                anchors.fill: parent
                visible: graphPanel.projectOpen
                color: "#1e1e1e"
                clip: true

                // Simple placeholder graph visualization
                Canvas {
                    id: graphVisualization
                    anchors.fill: parent

                    property var nodes: [
                        {x: 150, y: 100, type: "Document", title: "Meeting Notes", color: "#4fc3f7"},
                        {x: 350, y: 150, type: "Concept", title: "ML", color: "#66bb6a"},
                        {x: 250, y: 250, type: "Note", title: "Ideas", color: "#ffa726"},
                        {x: 450, y: 200, type: "Entity", title: "John", color: "#ab47bc"}
                    ]

                    property var edges: [
                        {from: 0, to: 1},
                        {from: 1, to: 2},
                        {from: 0, to: 3}
                    ]

                    onPaint: {
                        var ctx = context
                        ctx.clearRect(0, 0, width, height)

                        // Draw edges
                        ctx.strokeStyle = "#666666"
                        ctx.lineWidth = 2
                        for (var i = 0; i < edges.length; i++) {
                            var edge = edges[i]
                            var fromNode = nodes[edge.from]
                            var toNode = nodes[edge.to]

                            ctx.beginPath()
                            ctx.moveTo(fromNode.x, fromNode.y)
                            ctx.lineTo(toNode.x, toNode.y)
                            ctx.stroke()
                        }

                        // Draw nodes
                        for (var j = 0; j < nodes.length; j++) {
                            var node = nodes[j]

                            // Node circle
                            ctx.fillStyle = node.color
                            ctx.beginPath()
                            ctx.arc(node.x, node.y, 20, 0, 2 * Math.PI)
                            ctx.fill()

                            // Node border
                            ctx.strokeStyle = graphPanel.selectedNodeId === j.toString() ? "white" : "transparent"
                            ctx.lineWidth = 3
                            ctx.stroke()

                            // Node label
                            if (graphPanel.showNodeLabels) {
                                ctx.fillStyle = "white"
                                ctx.font = "12px Arial"
                                ctx.textAlign = "center"
                                ctx.fillText(node.title, node.x, node.y + 35)
                            }
                        }
                    }

                    MouseArea {
                        anchors.fill: parent
                        acceptedButtons: Qt.LeftButton | Qt.RightButton

                        onClicked: {
                            // Simple hit detection for nodes
                            for (var i = 0; i < parent.nodes.length; i++) {
                                var node = parent.nodes[i]
                                var dx = mouse.x - node.x
                                var dy = mouse.y - node.y
                                var distance = Math.sqrt(dx * dx + dy * dy)

                                if (distance <= 20) {
                                    graphPanel.selectedNodeId = i.toString()
                                    graphPanel.nodeClicked(i.toString(), node.type)
                                    parent.requestPaint()
                                    return
                                }
                            }

                            // Clear selection if clicked on empty space
                            graphPanel.selectedNodeId = ""
                            parent.requestPaint()
                        }

                        onDoubleClicked: {
                            // Simple hit detection for double-click
                            for (var i = 0; i < parent.nodes.length; i++) {
                                var node = parent.nodes[i]
                                var dx = mouse.x - node.x
                                var dy = mouse.y - node.y
                                var distance = Math.sqrt(dx * dx + dy * dy)

                                if (distance <= 20) {
                                    graphPanel.nodeDoubleClicked(i.toString(), node.type)
                                    return
                                }
                            }
                        }

                        onWheel: {
                            var zoomFactor = 1.0 + (wheel.angleDelta.y > 0 ? 0.1 : -0.1)
                            var newZoom = graphPanel.zoomLevel * zoomFactor
                            graphPanel.zoomLevel = Math.max(0.2, Math.min(3.0, newZoom))
                        }
                    }
                }

                // Loading overlay
                Rectangle {
                    anchors.fill: parent
                    color: "#80000000"
                    visible: graphPanel.isLoading

                    ColumnLayout {
                        anchors.centerIn: parent
                        spacing: 12

                        BusyIndicator {
                            Layout.alignment: Qt.AlignCenter
                            running: graphPanel.isLoading
                        }

                        Text {
                            text: "Loading knowledge graph..."
                            font.pixelSize: 14
                            color: "#cccccc"
                            Layout.alignment: Qt.AlignCenter
                        }
                    }
                }
            }
        }
    }

    // Update node and edge counts when project opens
    Component.onCompleted: {
        if (projectOpen) {
            nodeCount = 4
            edgeCount = 3
        }
    }

    // Public methods
    function updateGraphData(nodes, edges) {
        graphPanel.nodeCount = nodes ? nodes.length : 0
        graphPanel.edgeCount = edges ? edges.length : 0
        graphPanel.isLoading = false
        if (graphVisualization) {
            graphVisualization.requestPaint()
        }
    }

    function setProjectState(isOpen) {
        graphPanel.projectOpen = isOpen
        if (!isOpen) {
            graphPanel.selectedNodeId = ""
            graphPanel.nodeCount = 0
            graphPanel.edgeCount = 0
        } else {
            graphPanel.nodeCount = 4
            graphPanel.edgeCount = 3
        }
    }

    function focusOnNode(nodeId) {
        graphPanel.selectedNodeId = nodeId
        if (graphVisualization) {
            graphVisualization.requestPaint()
        }
    }

    function setLoading(loading) {
        graphPanel.isLoading = loading
    }

    function applyLayout(layoutType) {
        graphPanel.layoutMode = layoutType
        console.log("Applied layout:", layoutType)
    }
}

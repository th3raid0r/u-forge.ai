import QtQuick 2.12
import QtQuick.Controls 2.12
import QtQuick.Layouts 1.12
import "../styles"

Rectangle {
    id: toolbar
    height: 35
    color: toolbar.toolbarColor
    border.color: toolbar.borderColor
    border.width: 1

    // Hard-coded theme values
    property color toolbarColor: "#3c3c3c"
    property color borderColor: "#3e3e42"
    property color primaryTextColor: "#cccccc"
    property color mutedTextColor: "#6a6a6a"
    property color hoverColor: "#2a2d2e"
    property color selectionColor: "#094771"
    property color successColor: "#4caf50"
    property int fontSizeSmall: 11
    property int paddingSmall: 4
    property int paddingMedium: 12
    property int borderRadius: 4

    // Toolbar properties
    property bool projectOpen: false
    property string currentProjectName: ""

    // Signals for main window communication
    signal newProjectRequested()
    signal openProjectRequested()
    signal saveProjectRequested()
    signal toggleLeftPanelRequested()
    signal toggleBottomPanelRequested()
    signal toggleRightPanelRequested()
    signal settingsRequested()

    // Tooltip functions
    property var showToolTip: null
    property var hideToolTip: null

    RowLayout {
        anchors.fill: parent
        anchors.leftMargin: toolbar.paddingSmall
        anchors.rightMargin: toolbar.paddingSmall
        spacing: toolbar.paddingSmall

        // Left section - File operations
        RowLayout {
            spacing: 2

            ToolButton {
                id: newButton
                text: "New"
                font.pixelSize: toolbar.fontSizeSmall

                background: Rectangle {
                    color: newButtonArea.containsMouse ? toolbar.hoverColor : "transparent"
                    border.color: "transparent"
                    radius: 2
                }

                contentItem: Text {
                    text: parent.text
                    color: toolbar.primaryTextColor
                    font: parent.font
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                }

                MouseArea {
                    id: newButtonArea
                    anchors.fill: parent
                    hoverEnabled: true
                    onClicked: toolbar.newProjectRequested()
                    onEntered: {
                        if (toolbar.showToolTip) {
                            var pos = mapToItem(toolbar.parent, parent.width/2, 0)
                            toolbar.showToolTip(pos.x, pos.y, "Create new project (Ctrl+N)")
                        }
                    }
                    onExited: {
                        if (toolbar.hideToolTip) {
                            toolbar.hideToolTip()
                        }
                    }
                }
            }

            ToolButton {
                id: openButton
                text: "Open"
                font.pixelSize: toolbar.fontSizeSmall

                background: Rectangle {
                    color: openButtonArea.containsMouse ? toolbar.hoverColor : "transparent"
                    border.color: "transparent"
                    radius: 2
                }

                contentItem: Text {
                    text: parent.text
                    color: toolbar.primaryTextColor
                    font: parent.font
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                }

                MouseArea {
                    id: openButtonArea
                    anchors.fill: parent
                    hoverEnabled: true
                    onClicked: toolbar.openProjectRequested()
                    onEntered: {
                        if (toolbar.showToolTip) {
                            var pos = mapToItem(toolbar.parent, parent.width/2, 0)
                            toolbar.showToolTip(pos.x, pos.y, "Open existing project (Ctrl+O)")
                        }
                    }
                    onExited: {
                        if (toolbar.hideToolTip) {
                            toolbar.hideToolTip()
                        }
                    }
                }
            }

            ToolButton {
                id: saveButton
                text: "Save"
                font.pixelSize: toolbar.fontSizeSmall
                enabled: toolbar.projectOpen

                background: Rectangle {
                    color: saveButtonArea.containsMouse ? toolbar.hoverColor : "transparent"
                    border.color: "transparent"
                    radius: 2
                    opacity: parent.enabled ? 1.0 : 0.5
                }

                contentItem: Text {
                    text: parent.text
                    color: toolbar.primaryTextColor
                    font: parent.font
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                }

                MouseArea {
                    id: saveButtonArea
                    anchors.fill: parent
                    hoverEnabled: true
                    enabled: parent.enabled
                    onClicked: toolbar.saveProjectRequested()
                    onEntered: {
                        if (toolbar.showToolTip) {
                            var pos = mapToItem(toolbar.parent, parent.width/2, 0)
                            toolbar.showToolTip(pos.x, pos.y, "Save current project (Ctrl+S)")
                        }
                    }
                    onExited: {
                        if (toolbar.hideToolTip) {
                            toolbar.hideToolTip()
                        }
                    }
                }
            }

            Rectangle {
                width: 1
                height: parent.height * 0.6
                color: toolbar.borderColor
                Layout.leftMargin: toolbar.paddingSmall
                Layout.rightMargin: toolbar.paddingSmall
            }
        }

        // Center section - Project name and view toggles
        RowLayout {
            Layout.fillWidth: true
            spacing: toolbar.paddingMedium

            // Project name display
            Text {
                text: toolbar.projectOpen ? toolbar.currentProjectName : "No Project Open"
                color: toolbar.projectOpen ? toolbar.primaryTextColor : toolbar.mutedTextColor
                font.pixelSize: toolbar.fontSizeSmall
                font.italic: !toolbar.projectOpen
                Layout.fillWidth: true
                elide: Text.ElideMiddle
            }

            // Panel toggles
            RowLayout {
                spacing: 2

                ToolButton {
                    id: leftPanelToggle
                    text: "Explorer"
                    font.pixelSize: toolbar.fontSizeSmall
                    checkable: true
                    checked: true

                    background: Rectangle {
                        color: parent.checked ? toolbar.selectionColor :
                               (explorerButtonArea.containsMouse ? toolbar.hoverColor : "transparent")
                        border.color: "transparent"
                        radius: 2
                    }

                    contentItem: Text {
                        text: parent.text
                        color: toolbar.primaryTextColor
                        font: parent.font
                        horizontalAlignment: Text.AlignHCenter
                        verticalAlignment: Text.AlignVCenter
                    }

                    MouseArea {
                        id: explorerButtonArea
                        anchors.fill: parent
                        hoverEnabled: true
                        onClicked: {
                            parent.checked = !parent.checked
                            toolbar.toggleLeftPanelRequested()
                        }
                        onEntered: {
                            if (toolbar.showToolTip) {
                                var pos = mapToItem(toolbar.parent, parent.width/2, 0)
                                toolbar.showToolTip(pos.x, pos.y, "Toggle Explorer Panel (Ctrl+Shift+E)")
                            }
                        }
                        onExited: {
                            if (toolbar.hideToolTip) {
                                toolbar.hideToolTip()
                            }
                        }
                    }
                }

                ToolButton {
                    text: "⚙"
                    font.pixelSize: toolbar.fontSizeSmall
                    checkable: true
                    checked: false

                    background: Rectangle {
                        color: parent.checked ? toolbar.selectionColor :
                               (graphButtonArea.containsMouse ? toolbar.hoverColor : "transparent")
                        border.color: "transparent"
                        radius: 2
                    }

                    contentItem: Text {
                        text: parent.text
                        color: toolbar.primaryTextColor
                        font: parent.font
                        horizontalAlignment: Text.AlignHCenter
                        verticalAlignment: Text.AlignVCenter
                    }

                    MouseArea {
                        id: graphButtonArea
                        anchors.fill: parent
                        hoverEnabled: true
                        onClicked: {
                            parent.checked = !parent.checked
                            toolbar.toggleBottomPanelRequested()
                        }
                        onEntered: {
                            if (toolbar.showToolTip) {
                                var pos = mapToItem(toolbar.parent, parent.width/2, 0)
                                toolbar.showToolTip(pos.x, pos.y, "Toggle Knowledge Graph Panel (Ctrl+`)")
                            }
                        }
                        onExited: {
                            if (toolbar.hideToolTip) {
                                toolbar.hideToolTip()
                            }
                        }
                    }
                }

                ToolButton {
                    text: "🤖"
                    font.pixelSize: toolbar.fontSizeSmall
                    checkable: true
                    checked: true

                    background: Rectangle {
                        color: parent.checked ? toolbar.selectionColor :
                               (aiButtonArea.containsMouse ? toolbar.hoverColor : "transparent")
                        border.color: "transparent"
                        radius: 2
                    }

                    contentItem: Text {
                        text: parent.text
                        color: toolbar.primaryTextColor
                        font: parent.font
                        horizontalAlignment: Text.AlignHCenter
                        verticalAlignment: Text.AlignVCenter
                    }

                    MouseArea {
                        id: aiButtonArea
                        anchors.fill: parent
                        hoverEnabled: true
                        onClicked: {
                            parent.checked = !parent.checked
                            toolbar.toggleRightPanelRequested()
                        }
                        onEntered: {
                            if (toolbar.showToolTip) {
                                var pos = mapToItem(toolbar.parent, parent.width/2, 0)
                                toolbar.showToolTip(pos.x, pos.y, "Toggle AI Agent Panel (Ctrl+Shift+A)")
                            }
                        }
                        onExited: {
                            if (toolbar.hideToolTip) {
                                toolbar.hideToolTip()
                            }
                        }
                    }
                }
            }
        }

        // Right section - Settings and status
        RowLayout {
            spacing: 2

            Rectangle {
                width: 1
                height: parent.height * 0.6
                color: toolbar.borderColor
                Layout.leftMargin: toolbar.paddingSmall
                Layout.rightMargin: toolbar.paddingSmall
            }

            ToolButton {
                id: settingsButton
                text: "Settings"
                font.pixelSize: toolbar.fontSizeSmall

                background: Rectangle {
                    color: settingsButtonArea.containsMouse ? toolbar.hoverColor : "transparent"
                    border.color: "transparent"
                    radius: 2
                }

                contentItem: Text {
                    text: parent.text
                    color: toolbar.primaryTextColor
                    font: parent.font
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                }

                MouseArea {
                    id: settingsButtonArea
                    anchors.fill: parent
                    hoverEnabled: true
                    onClicked: toolbar.settingsRequested()
                    onEntered: {
                        if (toolbar.showToolTip) {
                            var pos = mapToItem(toolbar.parent, parent.width/2, 0)
                            toolbar.showToolTip(pos.x, pos.y, "Open Settings (Ctrl+,)")
                        }
                    }
                    onExited: {
                        if (toolbar.hideToolTip) {
                            toolbar.hideToolTip()
                        }
                    }
                }
            }

            // Status indicator
            Rectangle {
                width: 8
                height: 8
                radius: 4
                color: toolbar.projectOpen ? toolbar.successColor : toolbar.mutedTextColor
                Layout.rightMargin: toolbar.paddingSmall

                property string tooltipText: toolbar.projectOpen ? "Project loaded" : "No project"

                MouseArea {
                    id: statusMouseArea
                    anchors.fill: parent
                    hoverEnabled: true

                    onEntered: {
                        if (toolbar.showToolTip) {
                            var pos = mapToItem(toolbar.parent, parent.x + parent.width/2, parent.y)
                            toolbar.showToolTip(pos.x, pos.y, parent.tooltipText)
                        }
                    }
                    onExited: {
                        if (toolbar.hideToolTip) {
                            toolbar.hideToolTip()
                        }
                    }
                }
            }
        }
    }

    // Methods to update panel toggle states from main window
    function updateLeftPanelState(visible) {
        leftPanelToggle.checked = visible
    }

    function updateBottomPanelState(visible) {
        graphButtonArea.parent.checked = visible
    }

    function updateRightPanelState(visible) {
        aiButtonArea.parent.checked = visible
    }

    function updateProjectState(isOpen, projectName) {
        toolbar.projectOpen = isOpen
        toolbar.currentProjectName = projectName || ""
    }
}

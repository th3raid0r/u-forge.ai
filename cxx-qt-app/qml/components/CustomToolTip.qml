import QtQuick 2.12

// Custom ToolTip implementation to avoid binding loop issues
Item {
    id: root

    property string text: ""
    property bool show: false
    property int delay: 500
    property int timeout: 5000

    visible: false
    z: 999999

    // Global positioning - will be positioned relative to window
    anchors.fill: parent

    Rectangle {
        id: tooltip

        visible: root.show && root.text.length > 0

        color: "#2d2d30"
        border.color: "#3e3e42"
        border.width: 1
        radius: 4

        width: textContent.implicitWidth + 16
        height: textContent.implicitHeight + 12

        // Will be positioned by the show function
        x: 0
        y: 0

        Text {
            id: textContent
            anchors.centerIn: parent
            text: root.text
            color: "#cccccc"
            font.pixelSize: 11
            font.family: "Segoe UI"
            wrapMode: Text.WordWrap
            width: Math.min(implicitWidth, 200)
        }

        // Fade in/out animation
        opacity: root.show ? 1.0 : 0.0
        Behavior on opacity {
            NumberAnimation {
                duration: 150
                easing.type: Easing.InOutQuad
            }
        }

        // Drop shadow
        Rectangle {
            anchors.fill: parent
            anchors.topMargin: 2
            anchors.leftMargin: 2
            color: "#00000060"
            radius: parent.radius
            z: -1
        }
    }

    // Show timer with delay
    Timer {
        id: showTimer
        interval: root.delay
        onTriggered: tooltip.visible = true
    }

    // Auto-hide timer
    Timer {
        id: hideTimer
        interval: root.timeout
        onTriggered: root.show = false
    }

    onShowChanged: {
        if (show) {
            showTimer.start()
            if (timeout > 0) {
                hideTimer.restart()
            }
        } else {
            showTimer.stop()
            hideTimer.stop()
        }
    }

    // Position tooltip relative to mouse or target
    function showAt(globalX, globalY) {
        var windowWidth = parent.width
        var windowHeight = parent.height
        var tooltipWidth = tooltip.width
        var tooltipHeight = tooltip.height

        // Position horizontally
        var x = globalX - tooltipWidth / 2
        if (x < 8) x = 8
        if (x + tooltipWidth > windowWidth - 8) x = windowWidth - tooltipWidth - 8
        tooltip.x = x

        // Position vertically (prefer above, fall back to below)
        var y = globalY - tooltipHeight - 8
        if (y < 8) y = globalY + 8
        tooltip.y = y

        root.show = true
    }

    function hide() {
        root.show = false
    }
}

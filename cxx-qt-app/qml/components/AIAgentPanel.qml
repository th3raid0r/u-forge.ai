import QtQuick 2.12
import QtQuick.Controls 2.12
import QtQuick.Layouts 1.12
import "../styles"

Rectangle {
    id: agentPanel
    color: "#252526"
    border.color: "#3e3e42"
    border.width: 1

    // Theme instance
    Theme {
        id: theme
    }

    // Panel state
    property bool projectOpen: false
    property bool isCollapsed: false
    property real minimumWidth: 200
    property bool isProcessing: false
    property string currentAgentName: "U-Forge Assistant"
    property string currentAgentModel: "gpt-4"

    // Chat state
    property int messageCount: 0
    property bool hasActiveConversation: false

    // Signals for main window communication
    signal messageSubmitted(string message, string agentType)
    signal conversationCleared()
    signal agentSettingsRequested()
    signal knowledgeGraphQueryRequested(string query)
    signal nodeContextRequested(string nodeId)

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // Header with agent info and controls
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: 40
            color: "#252526"
            border.color: "#3e3e42"
            border.width: 1

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: 8
                anchors.rightMargin: 4
                spacing: 4

                // Agent indicator
                Rectangle {
                    width: 12
                    height: 12
                    radius: 6
                    color: agentPanel.isProcessing ? "#ff9800" : "#4caf50"

                    SequentialAnimation on opacity {
                        running: agentPanel.isProcessing
                        loops: Animation.Infinite
                        NumberAnimation { to: 0.3; duration: 500 }
                        NumberAnimation { to: 1.0; duration: 500 }
                    }
                }

                Text {
                    text: "AI AGENT"
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

                Text {
                    text: agentPanel.currentAgentName
                    font.pixelSize: 10
                    color: "#9d9d9d"
                    Layout.fillWidth: true
                    elide: Text.ElideRight
                }

                // Agent controls
                RowLayout {
                    spacing: 2

                    Button {
                        text: "Clear"
                        font.pixelSize: 10
                        enabled: agentPanel.hasActiveConversation

                        background: Rectangle {
                            color: parent.hovered ? "#2a2d2e" : "transparent"
                            radius: 4
                            opacity: parent.enabled ? 1.0 : 0.5
                        }

                        contentItem: Text {
                            text: parent.text
                            color: "#cccccc"
                            font: parent.font
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }

                        onClicked: {
                            conversationModel.clear()
                            agentPanel.messageCount = 0
                            agentPanel.hasActiveConversation = false
                            agentPanel.conversationCleared()
                        }
                    }

                    // Collapse button
                    ToolButton {
                        text: "»"
                        font.pixelSize: 11

                        background: Rectangle {
                            color: collapseAIArea.containsMouse ? "#2a2d2e" : "transparent"
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
                            id: collapseAIArea
                            anchors.fill: parent
                            hoverEnabled: true
                            onClicked: {
                                console.log("Collapse AI panel")
                                // TODO: Implement collapse logic
                            }
                            // Tooltips temporarily disabled
                            // onEntered: {
                            //     var pos = mapToItem(aiAgentPanel.parent, parent.x + parent.width/2, parent.y)
                            //     aiAgentPanel.parent.showToolTip(pos.x, pos.y, "Collapse panel")
                            // }
                            // onExited: aiAgentPanel.parent.hideToolTip()
                        }
                    }
                }
            }
        }

        // Main chat area
        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true
            Layout.topMargin: 4

            // No project state
            Rectangle {
                id: noProjectView
                anchors.fill: parent
                anchors.margins: 12
                color: "transparent"
                visible: !agentPanel.projectOpen

                ColumnLayout {
                    anchors.centerIn: parent
                    spacing: 16
                    width: Math.min(parent.width * 0.8, 300)

                    Text {
                        text: "🤖"
                        font.pixelSize: 48
                        color: "#6a6a6a"
                        Layout.alignment: Qt.AlignCenter
                    }

                    Text {
                        text: "AI Assistant Ready"
                        font.pixelSize: 14
                        font.bold: true
                        color: "#9d9d9d"
                        Layout.alignment: Qt.AlignCenter
                    }

                    Text {
                        text: "Open a project to start chatting with your AI assistant about your knowledge graph."
                        font.pixelSize: 11
                        color: "#6a6a6a"
                        wrapMode: Text.WordWrap
                        Layout.fillWidth: true
                        horizontalAlignment: Text.AlignHCenter
                    }
                }
            }

            // Chat interface (when project is open)
            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 4
                spacing: 4
                visible: agentPanel.projectOpen

                // Welcome message or conversation history
                ScrollView {
                    id: chatScrollView
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    ScrollBar.horizontal.policy: ScrollBar.AsNeeded
                    ScrollBar.vertical.policy: ScrollBar.AsNeeded
                    clip: true

                    ListView {
                        id: conversationView
                        model: conversationModel
                        delegate: messageDelegate
                        spacing: 4
                        verticalLayoutDirection: ListView.TopToBottom

                        // Auto-scroll to bottom when new messages arrive
                        onCountChanged: {
                            Qt.callLater(function() {
                                conversationView.positionViewAtEnd()
                            })
                        }
                    }
                }

                // Input area
                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: Math.max(inputField.contentHeight + 16, 60)
                    color: "#1e1e1e"
                    border.color: "#3e3e42"
                    border.width: 1
                    radius: 4

                    RowLayout {
                        anchors.fill: parent
                        anchors.margins: 4
                        spacing: 4

                        // Message input
                        ScrollView {
                            Layout.fillWidth: true
                            Layout.fillHeight: true
                            ScrollBar.vertical.policy: ScrollBar.AsNeeded

                            TextArea {
                                id: inputField
                                placeholderText: agentPanel.isProcessing ?
                                    "AI is thinking..." :
                                    "Ask me anything about your knowledge graph..."
                                font.pixelSize: 11
                                color: "#cccccc"
                                wrapMode: TextArea.Wrap
                                selectByMouse: true
                                enabled: !agentPanel.isProcessing

                                background: Rectangle {
                                    color: "transparent"
                                }

                                Keys.onPressed: {
                                    if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
                                        if (event.modifiers & Qt.ControlModifier) {
                                            // Ctrl+Enter for new line
                                            inputField.insert(inputField.cursorPosition, "\n")
                                        } else {
                                            // Enter to send
                                            event.accepted = true
                                            sendButton.clicked()
                                        }
                                    }
                                }
                            }
                        }

                        // Send button
                        Button {
                            id: sendButton
                            text: agentPanel.isProcessing ? "..." : "Send"
                            enabled: inputField.text.trim() !== "" && !agentPanel.isProcessing
                            font.pixelSize: 11

                            background: Rectangle {
                                color: parent.enabled ?
                                    (parent.hovered ? "#1177bb" : "#0e639c") :
                                    "#252526"
                                border.color: "#3e3e42"
                                border.width: 1
                                radius: 4
                                opacity: parent.enabled ? 1.0 : 0.5
                            }

                            contentItem: Text {
                                text: parent.text
                                color: "white"
                                font: parent.font
                                horizontalAlignment: Text.AlignHCenter
                                verticalAlignment: Text.AlignVCenter
                            }

                            onClicked: {
                                if (inputField.text.trim() !== "") {
                                    var message = inputField.text.trim()

                                    // Add user message to conversation
                                    conversationModel.append({
                                        "sender": "user",
                                        "message": message,
                                        "timestamp": new Date().toLocaleTimeString(),
                                        "isUser": true
                                    })

                                    // Clear input and set processing state
                                    inputField.text = ""
                                    agentPanel.isProcessing = true
                                    agentPanel.hasActiveConversation = true
                                    agentPanel.messageCount++

                                    // Emit signal for backend processing
                                    agentPanel.messageSubmitted(message, "general")

                                    // Simulate AI response (placeholder)
                                    responseTimer.message = message
                                    responseTimer.start()
                                }
                            }
                        }
                    }
                }

                // Quick action buttons
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 4
                    visible: agentPanel.projectOpen && !agentPanel.isProcessing

                    Button {
                        text: "Analyze Graph"
                        font.pixelSize: 10
                        enabled: agentPanel.projectOpen

                        background: Rectangle {
                            color: parent.hovered ? "#2a2d2e" : "#252526"
                            border.color: "#3e3e42"
                            border.width: 1
                            radius: 4
                        }

                        contentItem: Text {
                            text: parent.text
                            color: "#cccccc"
                            font: parent.font
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }

                        onClicked: {
                            inputField.text = "Can you analyze my knowledge graph structure and tell me what patterns you see?"
                            sendButton.clicked()
                        }
                    }

                    Button {
                        text: "Find Connections"
                        font.pixelSize: 10
                        enabled: agentPanel.projectOpen

                        background: Rectangle {
                            color: parent.hovered ? "#2a2d2e" : "#252526"
                            border.color: "#3e3e42"
                            border.width: 1
                            radius: 4
                        }

                        contentItem: Text {
                            text: parent.text
                            color: "#cccccc"
                            font: parent.font
                            horizontalAlignment: Text.AlignHCenter
                            verticalAlignment: Text.AlignVCenter
                        }

                        onClicked: {
                            inputField.text = "What are some interesting connections between concepts in my knowledge graph?"
                            sendButton.clicked()
                        }
                    }
                }
            }
        }
    }

    // Conversation model
    ListModel {
        id: conversationModel

        Component.onCompleted: {
            // Add welcome message when project is loaded
            if (agentPanel.projectOpen) {
                append({
                    "sender": "assistant",
                    "message": "Hello! I'm your U-Forge AI assistant. I can help you explore and understand your knowledge graph. What would you like to know?",
                    "timestamp": new Date().toLocaleTimeString(),
                    "isUser": false
                })
            }
        }
    }

    // Message delegate
    Component {
        id: messageDelegate
        Rectangle {
            width: conversationView.width
            height: messageContent.height + 16
            color: "transparent"

            RowLayout {
                anchors.fill: parent
                anchors.margins: 4
                spacing: 4

                // Avatar/Icon
                Rectangle {
                    Layout.preferredWidth: 24
                    Layout.preferredHeight: 24
                    Layout.alignment: Qt.AlignTop
                    color: model.isUser ? "#0e639c" : "#4fc3f7"
                    radius: 12

                    Text {
                        anchors.centerIn: parent
                        text: model.isUser ? "👤" : "🤖"
                        font.pixelSize: 12
                        color: "white"
                    }
                }

                // Message content
                Rectangle {
                    Layout.fillWidth: true
                    Layout.minimumHeight: messageText.height + 16
                    color: model.isUser ? "#0e639c" : "#252526"
                    border.color: model.isUser ? "transparent" : "#3e3e42"
                    border.width: 1
                    radius: 4

                    ColumnLayout {
                        id: messageContent
                        anchors.fill: parent
                        anchors.margins: 8
                        spacing: 2

                        Text {
                            id: messageText
                            Layout.fillWidth: true
                            text: model.message
                            font.pixelSize: 11
                            color: model.isUser ? "white" : "#cccccc"
                            wrapMode: Text.WordWrap
                        }

                        Text {
                            text: model.timestamp
                            font.pixelSize: 9
                            color: model.isUser ? "#ffffff80" : "#6a6a6a"
                            Layout.alignment: Qt.AlignRight
                        }
                    }
                }

                // Spacer for alignment
                Item {
                    Layout.preferredWidth: model.isUser ? 24 : 0
                    Layout.preferredHeight: 1
                }
            }
        }
    }

    // Timer for simulating AI responses (placeholder)
    Timer {
        id: responseTimer
        interval: 1500 + Math.random() * 2000 // Random delay between 1.5-3.5 seconds
        property string message: ""

        onTriggered: {
            // Generate a placeholder response
            var responses = [
                "That's an interesting question! Based on your knowledge graph, I can see several patterns that might be relevant to your inquiry.",
                "I've analyzed your request and found some connections in your knowledge graph that might be helpful.",
                "Great question! Let me help you explore that concept within the context of your existing knowledge.",
                "I notice you're asking about something that connects to several other concepts in your graph. Here's what I found:",
                "Thanks for that question! I can see how this relates to your existing knowledge structure."
            ]

            var response = responses[Math.floor(Math.random() * responses.length)]
            response += "\n\n(This is a placeholder response. In the full implementation, I would analyze your actual knowledge graph and provide contextual insights.)"

            conversationModel.append({
                "sender": "assistant",
                "message": response,
                "timestamp": new Date().toLocaleTimeString(),
                "isUser": false
            })

            agentPanel.isProcessing = false
        }
    }

    // Public methods
    function setProjectState(isOpen) {
        agentPanel.projectOpen = isOpen

        if (isOpen && conversationModel.count === 0) {
            conversationModel.append({
                "sender": "assistant",
                "message": "Hello! I'm your U-Forge AI assistant. I can help you explore and understand your knowledge graph. What would you like to know?",
                "timestamp": new Date().toLocaleTimeString(),
                "isUser": false
            })
        }
    }

    function addBotResponse(message) {
        conversationModel.append({
            "sender": "assistant",
            "message": message,
            "timestamp": new Date().toLocaleTimeString(),
            "isUser": false
        })
        agentPanel.isProcessing = false
    }

    function setProcessingState(processing) {
        agentPanel.isProcessing = processing
    }

    function updateAgentInfo(name, model) {
        agentPanel.currentAgentName = name
        agentPanel.currentAgentModel = model
    }

    function focusInput() {
        inputField.forceActiveFocus()
    }
}

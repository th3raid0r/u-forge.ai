import QtQuick 2.12

QtObject {
    id: theme

    // Color Palette - VSCode Dark Theme inspired
    property color backgroundColor: "#1e1e1e"
    property color surfaceColor: "#252526"
    property color elevatedSurfaceColor: "#2d2d30"
    property color borderColor: "#3e3e42"
    property color dividerColor: "#414141"

    // Text Colors
    property color primaryTextColor: "#cccccc"
    property color secondaryTextColor: "#9d9d9d"
    property color mutedTextColor: "#6a6a6a"
    property color accentTextColor: "#4fc3f7"

    // Interactive Colors
    property color primaryColor: "#0e639c"
    property color primaryHoverColor: "#1177bb"
    property color primaryActiveColor: "#094771"
    property color accentColor: "#4fc3f7"
    property color accentHoverColor: "#29b6f6"

    // State Colors
    property color successColor: "#4caf50"
    property color warningColor: "#ff9800"
    property color errorColor: "#f44336"
    property color infoColor: "#2196f3"

    // Panel Specific Colors
    property color toolbarColor: "#3c3c3c"
    property color panelHeaderColor: "#252526"
    property color treeViewColor: "#252526"
    property color editorColor: "#1e1e1e"
    property color terminalColor: "#0c0c0c"

    // Selection Colors
    property color selectionColor: "#094771"
    property color hoverColor: "#2a2d2e"
    property color focusColor: "#007acc"

    // Typography
    property int fontSizeSmall: 11
    property int fontSizeNormal: 13
    property int fontSizeMedium: 14
    property int fontSizeLarge: 16
    property int fontSizeXLarge: 18

    property string fontFamily: "Segoe UI"
    property string monospaceFontFamily: "Consolas"

    // Spacing
    property int paddingTiny: 2
    property int paddingSmall: 4
    property int paddingNormal: 8
    property int paddingMedium: 12
    property int paddingLarge: 16
    property int paddingXLarge: 24

    // Layout
    property int borderRadius: 4
    property int borderWidth: 1
    property int panelMinWidth: 200
    property int panelMinHeight: 100
    property int toolbarHeight: 35
    property int statusBarHeight: 22

    // Animation
    property int animationDuration: 150
    property int animationDurationLong: 250

    // Component Specific
    property int buttonHeight: 28
    property int inputHeight: 24
    property int treeItemHeight: 22
    property int tabHeight: 35

    // Shadows
    property color shadowColor: "#00000040"
    property int shadowRadius: 4
    property int shadowOffset: 2
}

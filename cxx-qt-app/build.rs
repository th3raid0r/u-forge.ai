use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new()
        // Link Qt's Network library
        // - Qt Core is always linked
        // - Qt Gui is linked by enabling the qt_gui Cargo feature of cxx-qt-lib.
        // - Qt Qml is linked by enabling the qt_qml Cargo feature of cxx-qt-lib.
        // - Qt Qml requires linking Qt Network on macOS
        .qt_module("Network")
        .qml_module(QmlModule {
            uri: "com.uforge.demo",
            rust_files: &["src/demo_object.rs"],
            qml_files: &[
                "qml/main.qml",
                "qml/styles/Theme.qml",
                "qml/components/MainToolbar.qml",
                "qml/components/ContentEditor.qml",
                "qml/components/ExplorerPanel.qml",
                "qml/components/KnowledgeGraphPanel.qml",
                "qml/components/AIAgentPanel.qml",
                "qml/components/CustomToolTip.qml",
            ],
            ..Default::default()
        })
        .build();
}

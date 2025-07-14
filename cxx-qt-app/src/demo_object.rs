/// The bridge definition for our QObject
#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        /// An alias to the QString type
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        // The QObject definition
        // We tell CXX-Qt that we want a QObject class with the name DemoObject
        // based on the Rust struct DemoObjectRust.
        #[qobject]
        #[qml_element]
        #[qproperty(i32, counter)]
        #[qproperty(QString, message)]
        #[namespace = "demo"]
        type DemoObject = super::DemoObjectRust;
    }

    extern "RustQt" {
        // Declare the invokable methods we want to expose on the QObject
        #[qinvokable]
        #[cxx_name = "incrementCounter"]
        fn increment_counter(self: Pin<&mut DemoObject>);

        #[qinvokable]
        #[cxx_name = "resetCounter"]
        fn reset_counter(self: Pin<&mut DemoObject>);

        #[qinvokable]
        #[cxx_name = "updateMessage"]
        fn update_message(self: Pin<&mut DemoObject>, new_message: &QString);

        #[qinvokable]
        #[cxx_name = "logCurrentState"]
        fn log_current_state(self: &DemoObject);
    }
}

use core::pin::Pin;
use cxx_qt_lib::QString;

/// The Rust struct for the QObject
#[derive(Default)]
pub struct DemoObjectRust {
    counter: i32,
    message: QString,
}

impl qobject::DemoObject {
    /// Increment the counter Q_PROPERTY
    pub fn increment_counter(mut self: Pin<&mut Self>) {
        let current = *self.counter();
        self.as_mut().set_counter(current + 1);

        // Update message with current counter value
        let new_message = QString::from(&format!("Counter is now: {}", current + 1));
        self.set_message(new_message);
    }

    /// Reset the counter to zero
    pub fn reset_counter(mut self: Pin<&mut Self>) {
        self.as_mut().set_counter(0);
        self.set_message(QString::from("Counter reset to 0"));
    }

    /// Update the message property
    pub fn update_message(self: Pin<&mut Self>, new_message: &QString) {
        self.set_message(new_message.clone());
    }

    /// Log the current state to console
    pub fn log_current_state(&self) {
        println!(
            "Demo Object State - Counter: {}, Message: '{}'",
            self.counter(),
            self.message()
        );
    }
}

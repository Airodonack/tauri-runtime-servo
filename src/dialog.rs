//! The page's `alert()`, `confirm()` and `prompt()`: Servo hands them to the
//! embedder as `EmbedderControl::SimpleDialog`, and the runtime shows each in
//! a GTK message dialog over the webview's window.
//!
//! The dialog is shown without a nested main loop (`show`, then the answer
//! in `connect_response`): the request arrives inside `Servo::spin_event_loop`,
//! and a modal `run()` there would spin tao's loop, and with it Servo,
//! re-entrantly.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use servo::{EmbedderControlId, SimpleDialog};

/// The dialogs on screen, so the engine can take one back
/// (`hide_embedder_control`, e.g. when the page navigates away).
#[derive(Default)]
pub struct OpenDialogs(RefCell<Vec<(EmbedderControlId, gtk::MessageDialog)>>);

impl OpenDialogs {
    pub fn show(
        self: &Rc<Self>,
        parent: Option<&gtk::ApplicationWindow>,
        id: EmbedderControlId,
        dialog: SimpleDialog,
    ) {
        let (kind, message_type, buttons, message) = match &dialog {
            SimpleDialog::Alert(alert) => (
                "alert",
                gtk::MessageType::Info,
                gtk::ButtonsType::Ok,
                alert.message(),
            ),
            SimpleDialog::Confirm(confirm) => (
                "confirm",
                gtk::MessageType::Question,
                gtk::ButtonsType::OkCancel,
                confirm.message(),
            ),
            SimpleDialog::Prompt(prompt) => (
                "prompt",
                gtk::MessageType::Question,
                gtk::ButtonsType::OkCancel,
                prompt.message(),
            ),
        };
        log::info!("showing the page's {kind} dialog");
        let gtk_dialog = gtk::MessageDialog::new(
            parent,
            gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
            message_type,
            buttons,
            message,
        );
        // The text is the page's: say so, so it can't pass for the app's own.
        gtk_dialog.set_title("The page says");
        let entry = if let SimpleDialog::Prompt(prompt) = &dialog {
            let entry = gtk::Entry::new();
            entry.set_text(prompt.current_value());
            entry.set_activates_default(true);
            gtk_dialog.content_area().add(&entry);
            gtk_dialog.set_default_response(gtk::ResponseType::Ok);
            Some(entry)
        } else {
            None
        };

        let pending = RefCell::new(Some(dialog));
        let open = Rc::downgrade(self);
        gtk_dialog.connect_response(move |gtk_dialog, response| {
            // Dropping a Servo dialog unanswered dismisses it, so a window
            // closed under the dialog still releases the page's script.
            if let Some(dialog) = pending.borrow_mut().take() {
                let accepted = response == gtk::ResponseType::Ok;
                match dialog {
                    SimpleDialog::Alert(alert) => alert.confirm(),
                    SimpleDialog::Confirm(confirm) if accepted => confirm.confirm(),
                    SimpleDialog::Confirm(confirm) => confirm.dismiss(),
                    SimpleDialog::Prompt(mut prompt) if accepted => {
                        if let Some(entry) = &entry {
                            prompt.set_current_value(entry.text().as_str());
                        }
                        prompt.confirm()
                    }
                    SimpleDialog::Prompt(prompt) => prompt.dismiss(),
                }
            }
            if let Some(open) = open.upgrade() {
                open.0.borrow_mut().retain(|(open_id, _)| *open_id != id);
            }
            // SAFETY: the dialog is ours and nothing uses it after this.
            unsafe { gtk_dialog.destroy() };
        });
        gtk_dialog.show_all();
        self.0.borrow_mut().push((id, gtk_dialog));
    }

    /// Takes a dialog back unanswered; returns whether it was one of ours.
    pub fn hide(&self, id: EmbedderControlId) -> bool {
        let dialog = {
            let mut open = self.0.borrow_mut();
            let index = open.iter().position(|(open_id, _)| *open_id == id);
            index.map(|index| open.remove(index).1)
        };
        match dialog {
            Some(dialog) => {
                dialog.response(gtk::ResponseType::Cancel);
                true
            }
            None => false,
        }
    }
}

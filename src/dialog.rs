//! The page's `alert()`, `confirm()` and `prompt()`: Servo hands them to the
//! embedder as `EmbedderControl::SimpleDialog`, and the runtime shows each in
//! a GTK message dialog over the webview's window.
//!
//! The dialog is shown without a nested main loop (`show`, then the answer
//! in `connect_response`): the request arrives inside `Servo::spin_event_loop`,
//! and a modal `run()` there would spin tao's loop, and with it Servo,
//! re-entrantly.
//!
//! Under WebDriver the same dialogs are its user prompts: the newest one
//! open is the current prompt, and accepting or dismissing it answers the
//! GTK dialog as its buttons would (see `webdriver`).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use servo::{EmbedderControlId, SimpleDialog, WebDriverUserPrompt, WebDriverUserPromptAction};

/// A dialog on screen.
struct OpenDialog {
    id: EmbedderControlId,
    gtk_dialog: gtk::MessageDialog,
    kind: WebDriverUserPrompt,
    message: String,
    /// The prompt's text field.
    entry: Option<gtk::Entry>,
}

/// The dialogs on screen, oldest first, so the engine can take one back
/// (`hide_embedder_control`, e.g. when the page navigates away) and
/// WebDriver can answer the newest.
#[derive(Default)]
pub struct OpenDialogs(RefCell<Vec<OpenDialog>>);

impl OpenDialogs {
    pub fn show(
        self: &Rc<Self>,
        parent: Option<&gtk::ApplicationWindow>,
        id: EmbedderControlId,
        dialog: SimpleDialog,
    ) {
        let (kind, message_type, buttons) = match &dialog {
            SimpleDialog::Alert(_) => (
                WebDriverUserPrompt::Alert,
                gtk::MessageType::Info,
                gtk::ButtonsType::Ok,
            ),
            SimpleDialog::Confirm(_) => (
                WebDriverUserPrompt::Confirm,
                gtk::MessageType::Question,
                gtk::ButtonsType::OkCancel,
            ),
            SimpleDialog::Prompt(_) => (
                WebDriverUserPrompt::Prompt,
                gtk::MessageType::Question,
                gtk::ButtonsType::OkCancel,
            ),
        };
        let message = dialog.message().to_owned();
        log::info!("showing the page's {kind:?} dialog");
        let gtk_dialog = gtk::MessageDialog::new(
            parent,
            gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
            message_type,
            buttons,
            &message,
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
        let response_entry = entry.clone();
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
                        if let Some(entry) = &response_entry {
                            prompt.set_current_value(entry.text().as_str());
                        }
                        prompt.confirm()
                    }
                    SimpleDialog::Prompt(prompt) => prompt.dismiss(),
                }
            }
            if let Some(open) = open.upgrade() {
                open.0.borrow_mut().retain(|open| open.id != id);
            }
            // SAFETY: the dialog is ours and nothing uses it after this.
            unsafe { gtk_dialog.destroy() };
        });
        gtk_dialog.show_all();
        self.0.borrow_mut().push(OpenDialog {
            id,
            gtk_dialog,
            kind,
            message,
            entry,
        });
    }

    /// Takes a dialog back unanswered; returns whether it was one of ours.
    pub fn hide(&self, id: EmbedderControlId) -> bool {
        let dialog = {
            let open = self.0.borrow();
            open.iter()
                .find(|open| open.id == id)
                .map(|open| open.gtk_dialog.clone())
        };
        // Outside the borrow: the response handler removes the dialog.
        match dialog {
            Some(dialog) => {
                dialog.response(gtk::ResponseType::Cancel);
                true
            }
            None => false,
        }
    }

    /// The newest dialog's kind, as WebDriver's current user prompt.
    pub fn current_prompt(&self) -> Option<WebDriverUserPrompt> {
        self.0.borrow().last().map(|open| open.kind)
    }

    /// The newest dialog's message.
    pub fn current_message(&self) -> Option<String> {
        self.0.borrow().last().map(|open| open.message.clone())
    }

    /// Puts `text` in the newest dialog's field, if it is a prompt.
    pub fn set_current_text(&self, text: &str) {
        if let Some(entry) = self.0.borrow().last().and_then(|open| open.entry.as_ref()) {
            entry.set_text(text);
        }
    }

    /// Answers the newest dialog as its OK or Cancel button would (or leaves
    /// it open, for `Ignore`) and returns its message; `Err` with none open.
    pub fn respond(&self, action: WebDriverUserPromptAction) -> Result<String, ()> {
        let (gtk_dialog, message) = {
            let open = self.0.borrow();
            let open = open.last().ok_or(())?;
            (open.gtk_dialog.clone(), open.message.clone())
        };
        // Outside the borrow: the response handler removes the dialog.
        match action {
            WebDriverUserPromptAction::Accept => gtk_dialog.response(gtk::ResponseType::Ok),
            WebDriverUserPromptAction::Dismiss => gtk_dialog.response(gtk::ResponseType::Cancel),
            WebDriverUserPromptAction::Ignore => {}
        }
        Ok(message)
    }
}

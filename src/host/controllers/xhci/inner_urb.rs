use futures::channel::oneshot::Sender;
use xhci::ring::trb::event::CommandCompletion;

use crate::usb::operations::CompleteAction;

pub type XHCICommandCallbackValue = Sender<CommandCompletion>;

#[derive(Debug)]
pub enum XHCICompleteAction {
    CommandCallback(XHCICommandCallbackValue),
    STANDARD(CompleteAction),
}

impl From<CompleteAction> for XHCICompleteAction {
    fn from(value: CompleteAction) -> Self {
        Self::STANDARD(value)
    }
}

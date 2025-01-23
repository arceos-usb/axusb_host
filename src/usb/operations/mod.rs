pub mod configurations;
pub mod isoch;
use core::{cell::SyncUnsafeCell, fmt::Debug};

use alloc::sync::Arc;
use async_lock::{Mutex, OnceCell, RwLock};
use bulk::BulkTransfer;
use control::ControlTransfer;
use futures::channel::oneshot::Sender;
use interrupt::InterruptTransfer;
use isoch::IsochTransfer;
use nosy::{sync::Notifier, Listen, Listener, Sink};
use num_derive::FromPrimitive;

use crate::host::device::ConfigureSemaphore;

use super::standards::TopologyRoute;

pub mod bulk;
pub mod control;
pub mod interrupt;

#[derive(Default)]
pub struct USBRequest {
    pub extra_action: ExtraAction,
    pub operation: RequestedOperation,
    pub complete_action: CompleteAction,
}

impl USBRequest {
    pub fn is_control(&self) -> bool {
        match self.operation {
            RequestedOperation::Control(_) => true,
            _ => false,
        }
    }
}

impl Debug for USBRequest {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("USBRequest")
            .field("operation", &self.operation)
            .finish()
    }
}

pub type ChannelNumber = u16;

#[derive(Debug, Default)]
pub enum ExtraAction {
    #[default]
    NOOP,
    KeepFill(ChannelNumber),
}

type ValueResult = Result<RequestResult, u8>;
pub type CallbackValue = Sender<ValueResult>; //todo: change this into a oneshot channel
pub type KeepCallbackValue = Notifier<ValueResult>;

pub fn construct_keep_callback_listener() -> (
    nosy::Notifier<
        Result<RequestResult, u8>,
        Arc<dyn Listener<Result<RequestResult, u8>> + Send + Sync>,
    >,
    Sink<Result<RequestResult, u8>>,
) {
    let notifier = KeepCallbackValue::new();
    let sink = Sink::new();
    notifier.listen(sink.listener());
    (notifier, sink)
}

#[derive(Default)]
pub enum CompleteAction {
    #[default]
    NOOP,
    SimpleResponse(CallbackValue),
    KeepResponse(KeepCallbackValue),
    DropSem(ConfigureSemaphore),
}

#[derive(Debug, Default)]
pub enum RequestedOperation {
    Control(ControlTransfer),
    Bulk(BulkTransfer),
    Interrupt(InterruptTransfer),
    Isoch(IsochTransfer),
    InitializeDevice(TopologyRoute),
    #[default]
    NOOP,
}

/// The direction of the data transfer.
#[derive(Copy, Clone, Debug, Ord, PartialOrd, Eq, PartialEq, Hash)]
pub enum Direction {
    /// Out (Write Data)
    Out = 0,
    /// In (Read Data)
    In = 1,
}
///copy from xhci crate
#[derive(Copy, Clone, Debug, Ord, PartialOrd, Eq, PartialEq, Hash, FromPrimitive)]
#[repr(u8)]
pub enum RequestResult {
    /// Indicates that the Completion Code field has not been updated by the TRB producer.
    Invalid = 0,
    /// Indicates successful completion of the TRB operation.
    Success = 1,
    /// Indicates that the Host Controller is unable to keep up the reception of incoming data
    /// (overrun) or is unable to supply data fast enough during transmission (underrun).
    DataBufferError = 2,
    /// Asserted when "babbling" is detected during the transaction generated by this TRB.
    BabbleDetectedError = 3,
    /// Asserted in the case where the host did not receive a valid response from the device.
    UsbTransactionError = 4,
    /// Asserted when a TRB parameter error condition is detected in a TRB.
    TrbError = 5,
    /// Asserted when a Stall condition is detected for a TRB.
    StallError = 6,
    /// Asserted by a Configure Endpoint Command or an Address Device Command if there are not
    /// adequate xHC resources available to successfully complete the command.
    ResourceError = 7,
    /// Asserted by a Configure Endpoint Command if periodic endpoints are declared and the xHC is
    /// not able to allocate the required Bandwidth.
    BandwidthError = 8,
    /// Asserted if a adding one more device would result in the host controller to exceed the
    /// maximum Number of Device Slots for this implementation.
    NoSlotsAvailableError = 9,
    /// Asserted if an invalid Stream Context Type value is detected.
    InvalidStreamTypeError = 10,
    /// Asserted if a command is issued to a Device Slot that is in the Disabled state.
    SlotNotEnabledError = 11,
    /// Asserted if a doorbell is rung for an endpoint that is in the Disabled state.
    EndpointNotEnabledError = 12,
    /// Asserted if the number of bytes received was less than the TD Transfer Size.
    ShortPacket = 13,
    /// Asserted in a Transfer Event TRB if the Transfer Ring is empty when an enabled Isoch
    /// endpoint is scheduled to transmit data.
    RingUnderrun = 14,
    /// Asserted in a Transfer Event TRB if the Transfer Ring is empty when an enabled Isoch
    /// endpoint is scheduled to receive data.
    RingOverrun = 15,
    /// Asserted by a Force Event command if the target VF's Event Ring is full.
    VfEventRingFullError = 16,
    /// Asserted by a command if a Context parameter is invalid.
    ParameterError = 17,
    /// Asserted during an Isoch transfer if the TD exceeds the bandwidth allocated to the
    /// endpoint.
    BandwidthOverrunError = 18,
    /// Asserted if a command is issued to transition from an illegal context state.
    ContextStateError = 19,
    /// Asserted if the xHC was unable to complete a periodic data transfer associated within the
    /// ESIT, because it did not receive a PING_RESPONSE in time.
    NoPingResponseError = 20,
    /// Asserted if the Event Ring is full, the xHC is unable to post an Event to the ring.
    EventRingFullError = 21,
    /// Asserted if the xHC detects a problem with a device that does not allow it to be
    /// successfully accessed.
    IncompatibleDeviceError = 22,
    /// Asserted if the xHC was unable to service a Isochronous endpoint within the Interval time.
    MissedServiceError = 23,
    /// Asserted in a Command Completion Event due to a Command Stop operation.
    CommandRingStopped = 24,
    /// Asserted in a Command Completion Event of an aborted command if the command was terminated
    /// by a Command Abort (CA) operation.
    CommandAborted = 25,
    /// Asserted in a Transfer Event if the transfer was terminated by a Stop Endpoint Command.
    Stopped = 26,
    /// Asserted in a Transfer Event if the transfer was terminated by a Stop Endpoint Command and
    /// the Transfer Event TRB Transfer Length field is invalid.
    StoppedLengthInvalid = 27,
    /// Asserted in a Transfer Event if the transfer was terminated by a Stop Endpoint Command, and
    /// the transfer was stopped after Short Packet conditions were met, but before the end of the
    /// TD was reached.
    StoppedShortPacket = 28,
    /// Asserted by the Evaluate Context Command if the proposed Max Exit Latency would not allow
    /// the periodic endpoints of the Device Slot to be scheduled.
    MaxExitLatencyTooLargeError = 29,
    /// Asserted if the data buffer defined by an Isoch TD on an IN endpoint is less than the Max
    /// ESIT Payload in size and the device attempts to send more data than it can hold.
    IsochBufferOverrun = 31,
    /// Asserted if the xHC internal event overrun condition.
    EventLostError = 32,
    /// May be reported by an event when other error codes do not apply.
    UndefinedError = 33,
    /// Asserted if an invalid Stream ID is received.
    InvalidStreamIdError = 34,
    /// Asserted by a Configure Endpoint Command if periodic endpoints are declared and the xHC is
    /// not able to allocate the required Bandwidth due to a Secondary Bandwidth Domain.
    SecondaryBandwidthError = 35,
    /// Asserted if an error is detected on a USB2 protocol endpoint for a split transaction.
    SplitTransactionError = 36,
}

#[cfg(feature = "backend-xhci")]
mod xhci_impl {
    use num_traits::FromPrimitive;
    use xhci::ring::trb::transfer::Direction;

    use super::RequestResult;

    impl From<xhci::ring::trb::event::CompletionCode> for RequestResult {
        fn from(value: xhci::ring::trb::event::CompletionCode) -> Self {
            RequestResult::from_u8(value as u8).unwrap()
        }
    }

    impl From<super::Direction> for Direction {
        fn from(value: super::Direction) -> Self {
            match value {
                super::Direction::Out => Direction::Out,
                super::Direction::In => Direction::In,
            }
        }
    }
}

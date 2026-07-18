//! # IRQ Dispatch
//!
//! This module provides a simple interrupt dispatch mechanism.
//! Each hardware interrupt vector (32-47) has a static handler function
//! that is called when the corresponding interrupt fires.
//!
//! ## Architecture
//!
//! ```text
//! Hardware IRQ → IO-APIC → LAPIC → CPU → IDT → static handler → dispatch_call → user handler
//! ```
//!
//! The dispatch is done through a static array of handler function pointers.
//! Each vector has a registered handler that gets called on the corresponding
//! interrupt.

/// Type alias for IRQ handler functions.
/// Takes no arguments, returns nothing. The handler is responsible for
/// acknowledging the interrupt (sending EOI to LAPIC).
pub type IrqHandler = fn();

/// Maximum number of IRQ handlers (vectors 32-47 = 16 hardware IRQ lines).
const MAX_IRQ_HANDLERS: usize = 16;

/// Offset: hardware IRQ 0 maps to vector 32.
pub const IRQ_VECTOR_OFFSET: u8 = 32;

/// The handler table. Each entry corresponds to IRQ n (vector = n + 32).
static mut HANDLER_TABLE: [Option<IrqHandler>; MAX_IRQ_HANDLERS] = [None; MAX_IRQ_HANDLERS];

/// Register a handler for a hardware IRQ.
///
/// # Arguments
/// * `irq` - The hardware IRQ number (0-15)
/// * `handler` - The function to call when this IRQ fires
///
/// # Panics
/// Panics if `irq` is out of range.
pub fn register(irq: usize, handler: IrqHandler) {
    assert!(
        irq < MAX_IRQ_HANDLERS,
        "IRQ number {} out of range (max {})",
        irq,
        MAX_IRQ_HANDLERS
    );
    unsafe {
        HANDLER_TABLE[irq] = Some(handler);
    }
}

/// Dispatch an IRQ to its registered handler.
///
/// Called by the static interrupt handler functions (vector 32-47).
/// Sends EOI to LAPIC after the handler returns.
///
/// # Arguments
/// * `vector` - The interrupt vector number (32-47)
///
/// # Safety
/// Must be called from interrupt context.
pub unsafe fn dispatch(vector: u8) {
    let irq = (vector - IRQ_VECTOR_OFFSET) as usize;

    if irq < MAX_IRQ_HANDLERS {
        if let Some(handler) = HANDLER_TABLE[irq] {
            handler();
        }
    }

    // Always send EOI to LAPIC, even if no handler was registered
    crate::interrupts::lapic::send_eoi();
}

/// Check if a vector is a hardware IRQ vector (32-47).
#[inline]
pub fn is_hardware_irq(vector: u8) -> bool {
    vector >= IRQ_VECTOR_OFFSET && vector < IRQ_VECTOR_OFFSET + MAX_IRQ_HANDLERS as u8
}

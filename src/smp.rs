//! xxx
//!
use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use spin::{Mutex, Once};

/// global
static GLOBAL_VIRT_IPI: Once<Mutex<VirtIPI>> = Once::new();

/// smps
pub fn init_virt_ipi(bsp: usize, cap: usize) {
    GLOBAL_VIRT_IPI.call_once(|| Mutex::new(VirtIPI::new(bsp, cap)));
}

/// fn get
pub fn receive_message(hart_id: usize) -> Message {
    GLOBAL_VIRT_IPI
        .get()
        .unwrap()
        .lock()
        .receive_message(hart_id)
}

/// send
pub fn send_message(msg: Message) {
    GLOBAL_VIRT_IPI.get().unwrap().lock().send_message(msg);
}

/// broadcast
pub fn broadcast_message(msg: Message) {
    GLOBAL_VIRT_IPI.get().unwrap().lock().broadcast_message(msg)
}

/// ipi
#[derive(Debug)]
pub struct VirtIPI {
    bsp: usize,
    messages: Vec<VecDeque<Message>>,
}

impl VirtIPI {
    /// new
    pub fn new(bsp: usize, cap: usize) -> Self {
        let mut vec = Vec::with_capacity(cap);
        for _ in 0..cap {
            vec.push(VecDeque::new());
        }
        Self {
            bsp,
            messages: vec,
        }
    }
    /// get_message
    pub fn receive_message(&mut self, hart_id: usize) -> Message {
        self.messages[hart_id].pop_front().unwrap()
    }
    /// push
    pub fn send_message(&mut self, msg: Message) {
        assert_ne!(msg.dest, self.bsp);
        self.messages[msg.dest].push_back(msg);
    }
    /// message's dest should be bsp
    pub fn broadcast_message(&mut self, msg: Message) {
        assert_eq!(msg.dest, self.bsp);
        for (i, que) in self.messages.iter_mut().enumerate() {
            if i == self.bsp {
                continue;
            }
            let mut msg = msg.clone();
            msg.dest = i;
            que.push_back(msg.clone());
        }
    }
}

/// msg
#[derive(Debug, Clone)]
pub struct Message {
    /// dest cpu
    pub dest: usize,
    /// signal
    pub signal: Signal,
    /// args
    pub args: Vec<usize>,
}

impl Message {
    /// new
    pub fn new(dest: usize, signal: Signal, args: Vec<usize>) -> Self {
        Self { dest, signal, args }
    }
}

/// sig
#[derive(Debug, Clone)]
pub enum Signal {
    /// s
    Start,
}

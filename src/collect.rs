use std::sync::mpsc::{self, channel};

pub struct Collector<T> {
    sender: mpsc::Sender<T>,
    receiver: mpsc::Receiver<T>,
}

pub struct Sender<T>(mpsc::Sender<T>);

impl<T> Collector<T> {
    pub fn new() -> Collector<T> {
        let (sender, receiver) = channel();
        Collector { sender, receiver }
    }
    /// Used to send from other tasks
    pub fn sender(&self) -> Sender<T> {
        Sender(self.sender.clone())
    }
    pub fn add(&self, value: T) {
        self.sender
            .send(value)
            .expect("can always send in an unbounded channel")
    }
    pub fn list(self) -> Vec<T> {
        self.receiver.try_iter().collect()
    }
}

impl<T> Sender<T> {
    pub fn add(&self, value: T) {
        self.0.send(value).ok();
    }
}

use hashbrown::*;
use ockam_message::message::{Address, AddressType, Message, MessageType, Receiver, Route, Sender};
use ockam_router::router::Direction;
use std::ops::DerefMut;
use std::sync::{Arc, Mutex};
use ockam_system::commands::commands::{OckamCommand, RouterCommand};

pub struct WorkerManager {
    tx: std::sync::mpsc::Sender<OckamCommand>,
    rx: std::sync::mpsc::Receiver<OckamCommand>,
    router_tx: std::sync::mpsc::Sender<OckamCommand>,
    workers: hashbrown::HashMap<String, Arc<Mutex<dyn Receiver + 'static + Send>>>,
    sender_ref: Option<Arc<Mutex<Sender>>>
}

impl Sender for WorkerManager {
    fn send(&mut self, m: Message) -> bool {
        unimplemented!()
    }
}

impl WorkerManager {
    pub fn new(
        tx: std::sync::mpsc::Sender<OckamCommand>,
        rx: std::sync::mpsc::Receiver<OckamCommand>,
        router_tx: std::sync::mpsc::Sender<OckamCommand>,
    ) -> WorkerManager {
        router_tx.send(OckamCommand::Router(RouterCommand::Register(
            AddressType::Worker,
            tx.clone(),
        )));
        WorkerManager {
            tx,
            rx,
            router_tx,
            workers: hashbrown::HashMap::new(),
            sender_ref: None,
        }
    }

    //pub fn initialize()

    // pub fn register(
    //     &mut self,
    //     a: Address,
    //     r: Arc<Mutex<dyn Receiver + 'static + Send>>,
    // ) -> Result<Sender, String> {
    //     self.workers.insert(a.as_string(), r);
    //
    // }

    pub fn poll(&mut self) -> bool {
        //    pub fn poll(wm: Arc<Mutex<WorkerManager>>) -> bool {
        let mut keep_going = true;
        let mut got = true;
        while got {
            got = false;
            if let Ok(c) = self.rx.try_recv() {
                got = true;
                match c {
                    // OckamCommand::worker(WorkerCommand::SendMessage(m)) => {
                    //     self.handle_send(m).unwrap();
                    // }
                    // OckamCommand::worker(WorkerCommand::ReceiveMessage(m)) => {
                    //     if let MessageType::Payload = m.message_type {
                    //         println!(
                    //             "worker received: {}",
                    //             str::from_utf8(&m.message_body).unwrap()
                    //         );
                    //     }
                    //     self.handle_receive(m).unwrap();
                    // }
                    // OckamCommand::worker(WorkerCommand::Stop) => {
                    //     keep_going = false;
                    // }
                    _ => println!("worker got bad message"),
                }
            }
        }
        keep_going
    }
}

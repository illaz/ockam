use std::io::Write;

use crate::config::{AddonKind, Config};
use crate::node::Node;
use crate::worker::Worker;

use ockam_message::message::RouterAddress;

use attohttpc::post;

pub fn run(config: Config) {
    let (mut node, router_tx) = Node::new(&config);

    let worker_addr = RouterAddress::worker_router_address_from_str("01242020").unwrap();
    let worker = Worker::new(worker_addr, router_tx, config.clone(), |w, msg| {
        match w.config().addon() {
            Some(AddonKind::InfluxDb(url, db)) => {
                let payload = String::from_utf8(msg.message_body);
                if payload.is_err() {
                    eprintln!("invalid message body for influx");
                    return;
                }

                match post(format!("{}write?db={}", url.into_string(), db))
                    .text(payload.unwrap())
                    .send()
                {
                    Ok(resp) => {
                        if let Err(e) = resp.error_for_status() {
                            eprintln!("bad influx HTTP response: {}", e);
                        }
                    }
                    Err(e) => println!("failed to send to influxdb: {}", e),
                }
            }
            None => {
                let mut out = std::io::stdout();
                out.write_all(msg.message_body.as_ref())
                    .expect("failed to write message to stdout");
                out.flush().expect("failed to flush stdout");
            }
        }
    });
    // add the worker and run the node to poll its various internal components
    node.add_worker(worker);
    node.run();
}

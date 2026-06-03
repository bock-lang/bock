#![allow(
    unused_variables,
    unused_imports,
    unused_parens,
    dead_code,
    non_upper_case_globals
)]

mod bock_runtime;

use crate::bock_runtime::*;
#[derive(Clone)]
pub enum MessageType {
    Text,
    Image,
    System,
    Ack,
}

#[derive(Clone)]
pub struct Message {
    pub id: i64,
    pub sender: String,
    pub msg_type: MessageType,
    pub content: String,
    pub timestamp: i64,
}

pub fn type_tag(t: MessageType) -> String {
    match t {
        MessageType::Text => "TEXT".to_string(),
        MessageType::Image => "IMAGE".to_string(),
        MessageType::System => "SYSTEM".to_string(),
        MessageType::Ack => "ACK".to_string(),
    }
}

pub fn encode(msg: Message) -> String {
    let tag = type_tag(msg.msg_type);
    format!("{}|{}|{}|{}|{}", tag, msg.id, msg.sender, msg.timestamp, msg.content)
}

pub fn decode(raw: String) -> Result<Message, String> {
    if !((((raw).chars().count() as i64) > 0_i64)) {
        return Err("empty input".to_string());
    }
    let msg_type = if (raw).starts_with(&("TEXT|".to_string()) as &str) { MessageType::Text } else { if (raw).starts_with(&("IMAGE|".to_string()) as &str) { MessageType::Image } else { if (raw).starts_with(&("SYSTEM|".to_string()) as &str) { MessageType::System } else { if (raw).starts_with(&("ACK|".to_string()) as &str) { MessageType::Ack } else { /* unsupported */ } } } };
    Ok(Message { id: 0_i64, sender: "decoded".to_string(), msg_type: msg_type, content: raw, timestamp: 0_i64 })
}

pub fn is_system_message(msg: Message) -> bool {
    match msg.msg_type {
        MessageType::System => true,
        _ => false,
    }
}

pub fn filter_by_sender(msgs: Vec<Message>, sender: String) -> Vec<Message> {
    msgs.filter(|m: _| (m.sender == sender))
}

pub fn filter_by_type(msgs: Vec<Message>, msg_type: MessageType) -> Vec<Message> {
    msgs.filter(|m: _| match m.msg_type {
        MessageType::Text => match msg_type {
            MessageType::Text => true,
            _ => false,
        },
        MessageType::Image => match msg_type {
            MessageType::Image => true,
            _ => false,
        },
        MessageType::System => match msg_type {
            MessageType::System => true,
            _ => false,
        },
        MessageType::Ack => match msg_type {
            MessageType::Ack => true,
            _ => false,
        },
    })
}

pub trait Serializable {
    fn serialize(&self) -> String;
}

impl Serializable for Message {
    fn serialize(&self) -> String {
        let tag = type_tag(self.msg_type);
        format!("[{}] {}@{}: {}", tag, self.sender, self.timestamp, self.content)
    }
}

pub trait Channel {
    fn send(&self, msg: Message) -> ();

    fn receive(&self) -> Message;
}

pub fn dispatch(msgs: Vec<Message>, channel: &impl Channel) -> () {
    for msg in msgs {
        channel.send(msg)
    }
}

#[derive(Clone)]
pub struct StubChannel {
}

impl Channel for StubChannel {
    fn send(&self, msg: Message) -> () {
        println!("{}", format!("  [channel] sent: {}", msg.content))
    }

    fn receive(&self) -> Message {
        Message { id: 0_i64, sender: "stub".to_string(), msg_type: MessageType::Text, content: "".to_string(), timestamp: 0_i64 }
    }
}

fn main() {
    println!("{}", "=== Chat Protocol Demo ===".to_string());
    println!("{}", "".to_string());
    let messages: Vec<Message> = vec![Message { id: 1_i64, sender: "alice".to_string(), msg_type: MessageType::Text, content: "Hello everyone!".to_string(), timestamp: 1000_i64 }, Message { id: 2_i64, sender: "bob".to_string(), msg_type: MessageType::Text, content: "Hi Alice!".to_string(), timestamp: 1001_i64 }, Message { id: 3_i64, sender: "system".to_string(), msg_type: MessageType::System, content: "bob joined the chat".to_string(), timestamp: 999_i64 }, Message { id: 4_i64, sender: "alice".to_string(), msg_type: MessageType::Image, content: "photo.png".to_string(), timestamp: 1002_i64 }, Message { id: 5_i64, sender: "bob".to_string(), msg_type: MessageType::Ack, content: "ack:1".to_string(), timestamp: 1003_i64 }];
    println!("{}", "--- Encoded Messages ---".to_string());
    for msg in messages {
        let encoded = encode(msg);
        println!("{}", format!("  {}", encoded))
    }
    println!("{}", "".to_string());
    println!("{}", "--- Decode Round-Trip ---".to_string());
    let test_raw = "TEXT|1|alice|1000|Hello everyone!".to_string();
    let decoded = decode(test_raw);
    match decoded {
        Ok(msg) => println!("{}", format!("  Decoded OK: type={}, content={}", type_tag(msg.msg_type), msg.content)),
        Err(e) => println!("{}", format!("  Decode error: {}", e)),
    }
    let bad_raw = "UNKNOWN|data".to_string();
    let bad_decoded = decode(bad_raw);
    match bad_decoded {
        Ok(_) => println!("{}", "  Unexpected success".to_string()),
        Err(e) => println!("{}", format!("  Expected error: {}", e)),
    }
    let alice_msgs = filter_by_sender(messages.clone(), "alice".to_string());
    println!("{}", "".to_string());
    println!("{}", format!("--- Alice's Messages ({}) ---", ((alice_msgs).len() as i64)));
    for msg in alice_msgs {
        println!("{}", format!("  {}", msg.serialize()))
    }
    let system_msgs = filter_by_type(messages.clone(), MessageType::System);
    println!("{}", "".to_string());
    println!("{}", format!("--- System Messages ({}) ---", ((system_msgs).len() as i64)));
    for msg in system_msgs {
        println!("{}", format!("  {}", msg.serialize()))
    }
    let first = (messages).first().cloned();
    match first {
        Some(msg) => {
            let is_sys = is_system_message(msg);
            println!("{}", "".to_string());
            println!("{}", format!("Is first message system? {}", is_sys))
        }
        None => {
        }
    }
    println!("{}", "".to_string());
    println!("{}", "--- Channel Dispatch ---".to_string());
    let ch = StubChannel {  };
    {
        let __channel = ch;
        dispatch(messages.clone(), &__channel)
    }
    println!("{}", "".to_string());
    println!("{}", "=== Done ===".to_string())
}

import type { BockOption, BockResult } from "./_bock_runtime.js";
import { __bockChannelNew, __bockSpawn } from "./_bock_runtime.js";
export type MessageType = MessageType_Text | MessageType_Image | MessageType_System | MessageType_Ack;

interface MessageType_Text { readonly _tag: "Text"; }
const MessageType_Text: MessageType_Text = Object.freeze({ _tag: "Text" as const });
interface MessageType_Image { readonly _tag: "Image"; }
const MessageType_Image: MessageType_Image = Object.freeze({ _tag: "Image" as const });
interface MessageType_System { readonly _tag: "System"; }
const MessageType_System: MessageType_System = Object.freeze({ _tag: "System" as const });
interface MessageType_Ack { readonly _tag: "Ack"; }
const MessageType_Ack: MessageType_Ack = Object.freeze({ _tag: "Ack" as const });

export class Message {
  id: number;
  sender: string;
  msg_type: MessageType;
  content: string;
  timestamp: number;
  constructor({ id, sender, msg_type, content, timestamp }: { id: number; sender: string; msg_type: MessageType; content: string; timestamp: number }) {
    this.id = id;
    this.sender = sender;
    this.msg_type = msg_type;
    this.content = content;
    this.timestamp = timestamp;
  }
}

export function typeTag(t: MessageType): string {
  return (() => {
    switch (t._tag) {
      case "Text": {
        return "TEXT";
        break;
      }
      case "Image": {
        return "IMAGE";
        break;
      }
      case "System": {
        return "SYSTEM";
        break;
      }
      case "Ack": {
        return "ACK";
        break;
      }
    }
  })();
}

export function encode(msg: Message): string {
  const tag = typeTag(msg.msg_type);
  return `${tag}|${msg.id}|${msg.sender}|${msg.timestamp}|${msg.content}`;
}

export function decode(raw: string): BockResult<Message, string> {
  if (!(([...(raw)].length > 0))) {
    return { _tag: "Err" as const, _0: "empty input" };
  }
  const msgType = ((raw).startsWith("TEXT|") ? MessageType_Text : ((raw).startsWith("IMAGE|") ? MessageType_Image : ((raw).startsWith("SYSTEM|") ? MessageType_System : ((raw).startsWith("ACK|") ? MessageType_Ack : /* unsupported */))));
  return { _tag: "Ok" as const, _0: new Message({ id: 0, sender: "decoded", msg_type: msgType, content: raw, timestamp: 0 }) };
}

export function isSystemMessage(msg: Message): boolean {
  return (() => {
    const __match1 = msg.msg_type;
    switch (__match1._tag) {
      case "System": {
        return true;
        break;
      }
      default: {
        return false;
        break;
      }
    }
  })();
}

export function filterBySender(msgs: Array<Message>, sender: string): Array<Message> {
  return msgs.filter(msgs, (m) => (m.sender === sender));
}

export function filterByType(msgs: Array<Message>, msgType: MessageType): Array<Message> {
  return msgs.filter(msgs, (m) => (() => {
    const __match3 = m.msg_type;
    switch (__match3._tag) {
      case "Text": {
        return (() => {
          switch (msgType._tag) {
            case "Text": {
              return true;
              break;
            }
            default: {
              return false;
              break;
            }
          }
        })();
        break;
      }
      case "Image": {
        return (() => {
          switch (msgType._tag) {
            case "Image": {
              return true;
              break;
            }
            default: {
              return false;
              break;
            }
          }
        })();
        break;
      }
      case "System": {
        return (() => {
          switch (msgType._tag) {
            case "System": {
              return true;
              break;
            }
            default: {
              return false;
              break;
            }
          }
        })();
        break;
      }
      case "Ack": {
        return (() => {
          switch (msgType._tag) {
            case "Ack": {
              return true;
              break;
            }
            default: {
              return false;
              break;
            }
          }
        })();
        break;
      }
    }
  })());
}

export interface Serializable {
  serialize(self: Serializable): string;
}

export interface Message extends Serializable {
  serialize(self: Message): string;
}
// impl Serializable for Message
Message.prototype.serialize = function(self: Message): string {
  const tag = typeTag(self.msg_type);
  return `[${tag}] ${self.sender}@${self.timestamp}: ${self.content}`;
};

export interface Channel {
  send(msg: Message): void;
  receive(): Message;
}

export function dispatch(msgs: Array<Message>, { channel }: { channel: Channel }): void {
  for (const msg of msgs) {
    return channel.send(msg);
  }
}

export class StubChannel {}

export interface StubChannel extends Channel {
  send(msg: Message): void;
  receive(): Message;
}
// impl Channel for StubChannel
StubChannel.prototype.send = function(msg: Message): void {
  return console.log(`  [channel] sent: ${msg.content}`);
};
StubChannel.prototype.receive = function(): Message {
  return new Message({ id: 0, sender: "stub", msg_type: MessageType_Text, content: "", timestamp: 0 });
};

function main() {
  console.log("=== Chat Protocol Demo ===");
  console.log("");
  const messages: Array<Message> = [new Message({ id: 1, sender: "alice", msg_type: MessageType_Text, content: "Hello everyone!", timestamp: 1000 }), new Message({ id: 2, sender: "bob", msg_type: MessageType_Text, content: "Hi Alice!", timestamp: 1001 }), new Message({ id: 3, sender: "system", msg_type: MessageType_System, content: "bob joined the chat", timestamp: 999 }), new Message({ id: 4, sender: "alice", msg_type: MessageType_Image, content: "photo.png", timestamp: 1002 }), new Message({ id: 5, sender: "bob", msg_type: MessageType_Ack, content: "ack:1", timestamp: 1003 })];
  console.log("--- Encoded Messages ---");
  for (const msg of messages) {
    const encoded = encode(msg);
    return console.log(`  ${encoded}`);
  }
  console.log("");
  console.log("--- Decode Round-Trip ---");
  const testRaw = "TEXT|1|alice|1000|Hello everyone!";
  const decoded = decode(testRaw);
  switch (decoded._tag) {
    case "Ok": {
      const msg = decoded._0;
      return console.log(`  Decoded OK: type=${typeTag(msg.msg_type)}, content=${msg.content}`);
      break;
    }
    case "Err": {
      const e = decoded._0;
      return console.log(`  Decode error: ${e}`);
      break;
    }
  }
  const badRaw = "UNKNOWN|data";
  const badDecoded = decode(badRaw);
  switch (badDecoded._tag) {
    case "Ok": {
      const _ = badDecoded._0;
      return console.log("  Unexpected success");
      break;
    }
    case "Err": {
      const e = badDecoded._0;
      return console.log(`  Expected error: ${e}`);
      break;
    }
  }
  const aliceMsgs = filterBySender(messages, "alice");
  console.log("");
  console.log(`--- Alice's Messages (${(aliceMsgs).length}) ---`);
  for (const msg of aliceMsgs) {
    return console.log(`  ${msg.serialize(msg)}`);
  }
  const systemMsgs = filterByType(messages, MessageType_System);
  console.log("");
  console.log(`--- System Messages (${(systemMsgs).length}) ---`);
  for (const msg of systemMsgs) {
    return console.log(`  ${msg.serialize(msg)}`);
  }
  const first = (<T,>(__r: ReadonlyArray<T>): BockOption<T> => __r.length > 0 ? { _tag: "Some" as const, _0: __r[0] } : { _tag: "None" as const })(messages);
  switch (first._tag) {
    case "Some": {
      const msg = first._0;
      const isSys = isSystemMessage(msg);
      console.log("");
      return console.log(`Is first message system? ${isSys}`);
      break;
    }
    case "None": {
      break;
    }
  }
  console.log("");
  console.log("--- Channel Dispatch ---");
  const ch = new StubChannel();
  {
    const __channel: Channel = ch;
    dispatch(messages, { channel: __channel });
  }
  console.log("");
  return console.log("=== Done ===");
}
export { MessageType_Ack, MessageType_Image, MessageType_System, MessageType_Text };
main();
//# sourceMappingURL=main.ts.map

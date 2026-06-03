from __future__ import annotations
from _bock_runtime import *
from typing import Union
from abc import ABC, abstractmethod
from dataclasses import dataclass

@dataclass(frozen=True)
class MessageType_Text:
    _tag: str = "Text"

@dataclass(frozen=True)
class MessageType_Image:
    _tag: str = "Image"

@dataclass(frozen=True)
class MessageType_System:
    _tag: str = "System"

@dataclass(frozen=True)
class MessageType_Ack:
    _tag: str = "Ack"
MessageType = Union[MessageType_Text, MessageType_Image, MessageType_System, MessageType_Ack]

@dataclass
class Message(Serializable):
    id: int
    sender: str
    msg_type: MessageType
    content: str
    timestamp: int

    def serialize(self) -> str:
        tag = type_tag(self.msg_type)
        return f"[{tag}] {self.sender}@{self.timestamp}: {self.content}"

def type_tag(t: MessageType) -> str:
    return (lambda __v: "TEXT" if isinstance(__v, MessageType_Text) else ("IMAGE" if isinstance(__v, MessageType_Image) else ("SYSTEM" if isinstance(__v, MessageType_System) else ("ACK"))))(t)

def encode(msg: Message) -> str:
    tag = type_tag(msg.msg_type)
    return f"{tag}|{msg.id}|{msg.sender}|{msg.timestamp}|{msg.content}"

def decode(raw: str) -> _BockOk | _BockErr:
    if not ((len(raw) > 0)):
        return _BockErr("empty input")
    msg_type = (MessageType_Text() if (raw).startswith("TEXT|") else (MessageType_Image() if (raw).startswith("IMAGE|") else (MessageType_System() if (raw).startswith("SYSTEM|") else (MessageType_Ack() if (raw).startswith("ACK|") else # unsupported))))
    return _BockOk(Message(id=0, sender="decoded", msg_type=msg_type, content=raw, timestamp=0))

def is_system_message(msg: Message) -> bool:
    return (lambda __v: True if isinstance(__v, MessageType_System) else (False))(msg.msg_type)

def filter_by_sender(msgs: list[Message], sender: str) -> list[Message]:
    return msgs.filter(lambda m: (m.sender == sender))

def filter_by_type(msgs: list[Message], msg_type: MessageType) -> list[Message]:
    return msgs.filter(lambda m: (lambda __v: (lambda __v: True if isinstance(__v, MessageType_Text) else (False))(msg_type) if isinstance(__v, MessageType_Text) else ((lambda __v: True if isinstance(__v, MessageType_Image) else (False))(msg_type) if isinstance(__v, MessageType_Image) else ((lambda __v: True if isinstance(__v, MessageType_System) else (False))(msg_type) if isinstance(__v, MessageType_System) else ((lambda __v: True if isinstance(__v, MessageType_Ack) else (False))(msg_type)))))(m.msg_type))

# trait Serializable
class Serializable:
    def serialize(self) -> str:
        pass

class Channel(ABC):
    @abstractmethod
    def send(self, msg: Message) -> None:
        ...

    @abstractmethod
    def receive(self) -> Message:
        ...

def dispatch(msgs: list[Message], *, channel: Channel) -> None:
    for msg in msgs:
        return channel.send(msg)

class StubChannel(Channel):

    def send(self, msg: Message) -> None:
        return print(f"  [channel] sent: {msg.content}")

    def receive(self) -> Message:
        return Message(id=0, sender="stub", msg_type=MessageType_Text(), content="", timestamp=0)

def main():
    print("=== Chat Protocol Demo ===")
    print("")
    messages: list[Message] = [Message(id=1, sender="alice", msg_type=MessageType_Text(), content="Hello everyone!", timestamp=1000), Message(id=2, sender="bob", msg_type=MessageType_Text(), content="Hi Alice!", timestamp=1001), Message(id=3, sender="system", msg_type=MessageType_System(), content="bob joined the chat", timestamp=999), Message(id=4, sender="alice", msg_type=MessageType_Image(), content="photo.png", timestamp=1002), Message(id=5, sender="bob", msg_type=MessageType_Ack(), content="ack:1", timestamp=1003)]
    print("--- Encoded Messages ---")
    for msg in messages:
        encoded = encode(msg)
        return print(f"  {encoded}")
    print("")
    print("--- Decode Round-Trip ---")
    test_raw = "TEXT|1|alice|1000|Hello everyone!"
    decoded = decode(test_raw)
    match decoded:
        case _BockOk(msg):
            return print(f"  Decoded OK: type={type_tag(msg.msg_type)}, content={msg.content}")
        case _BockErr(e):
            return print(f"  Decode error: {e}")
    bad_raw = "UNKNOWN|data"
    bad_decoded = decode(bad_raw)
    match bad_decoded:
        case _BockOk(_):
            return print("  Unexpected success")
        case _BockErr(e):
            return print(f"  Expected error: {e}")
    alice_msgs = filter_by_sender(messages, "alice")
    print("")
    print(f"--- Alice's Messages ({len(alice_msgs)}) ---")
    for msg in alice_msgs:
        return print(f"  {msg.serialize()}")
    system_msgs = filter_by_type(messages, MessageType_System())
    print("")
    print(f"--- System Messages ({len(system_msgs)}) ---")
    for msg in system_msgs:
        return print(f"  {msg.serialize()}")
    first = (lambda __r: _BockSome(__r[0]) if len(__r) > 0 else _bock_none)(messages)
    match first:
        case _BockSome(msg):
            is_sys = is_system_message(msg)
            print("")
            return print(f"Is first message system? {is_sys}")
        case _BockNone():
            pass
    print("")
    print("--- Channel Dispatch ---")
    ch = StubChannel()
    __channel_h1: Channel = ch
    dispatch(messages, channel=__channel_h1)
    print("")
    return print("=== Done ===")
if __name__ == "__main__":
    main()

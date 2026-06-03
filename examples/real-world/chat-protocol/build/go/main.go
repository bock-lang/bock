package main

import (
	"fmt"
	"strings"
	"unicode/utf8"
)

type MessageType interface {
	isMessageType()
}

type MessageTypeText struct{}

func (MessageTypeText) isMessageType() {}

type MessageTypeImage struct{}

func (MessageTypeImage) isMessageType() {}

type MessageTypeSystem struct{}

func (MessageTypeSystem) isMessageType() {}

type MessageTypeAck struct{}

func (MessageTypeAck) isMessageType() {}

type Message struct {
	Id	int64
	Sender	string
	MsgType	MessageType
	Content	string
	Timestamp	int64
}

func TypeTag(t MessageType) string {
	return func() string { switch t.(type) { case MessageTypeText: return "TEXT"; case MessageTypeImage: return "IMAGE"; case MessageTypeSystem: return "SYSTEM"; case MessageTypeAck: return "ACK"; }; panic("unreachable") }()
}

func Encode(msg Message) string {
	tag := TypeTag(msg.MsgType)
	return fmt.Sprintf("%v|%v|%v|%v|%v", tag, msg.Id, msg.Sender, msg.Timestamp, msg.Content)
}

func Decode(raw string) __bockResult {
	if !((int64(utf8.RuneCountInString(raw)) > 0)) {
		return __bockErr("empty input")
	}
	msgType := func() __bockResult { if strings.HasPrefix(raw, "TEXT|") { return MessageTypeText{} } else { return func() __bockResult { if strings.HasPrefix(raw, "IMAGE|") { return MessageTypeImage{} } else { return func() __bockResult { if strings.HasPrefix(raw, "SYSTEM|") { return MessageTypeSystem{} } else { return func() __bockResult { if strings.HasPrefix(raw, "ACK|") { return MessageTypeAck{} } else { return /* unsupported */ } }() } }() } }() } }()
	return __bockOk(Message{Id: 0, Sender: "decoded", MsgType: msgType, Content: raw, Timestamp: 0})
}

func IsSystemMessage(msg Message) bool {
	return func() bool { switch msg.MsgType.(type) { case MessageTypeSystem: return true; default: return false; }; panic("unreachable") }()
}

func FilterBySender(msgs []Message, sender string) []Message {
	return msgs.filter(func(m interface{}) bool { return (m.Sender == sender) })
}

func FilterByType(msgs []Message, msgType MessageType) []Message {
	return msgs.filter(func(m interface{}) interface{} { return func() []Message { switch m.MsgType.(type) { case MessageTypeText: return func() []Message { switch msgType.(type) { case MessageTypeText: return true; default: return false; }; panic("unreachable") }(); case MessageTypeImage: return func() []Message { switch msgType.(type) { case MessageTypeImage: return true; default: return false; }; panic("unreachable") }(); case MessageTypeSystem: return func() []Message { switch msgType.(type) { case MessageTypeSystem: return true; default: return false; }; panic("unreachable") }(); case MessageTypeAck: return func() []Message { switch msgType.(type) { case MessageTypeAck: return true; default: return false; }; panic("unreachable") }(); }; panic("unreachable") }() })
}

type Serializable interface {
	Serialize() string
}

func (self Message) Serialize() string {
	tag := TypeTag(self.MsgType)
	return fmt.Sprintf("[%v] %v@%v: %v", tag, self.Sender, self.Timestamp, self.Content)
}

type Channel interface {
	Send(Message)
	Receive() Message
}

func Dispatch(msgs []Message, channel Channel) {
	for _, msg := range msgs {
		channel.Send(msg)
	}
}

type StubChannel struct {
}

func (s StubChannel) Send(msg Message) {
	fmt.Println(fmt.Sprintf("  [channel] sent: %v", msg.Content))
}

func (s StubChannel) Receive() Message {
	return Message{Id: 0, Sender: "stub", MsgType: MessageTypeText{}, Content: "", Timestamp: 0}
}

func main() {
	fmt.Println("=== Chat Protocol Demo ===")
	fmt.Println("")
	var messages []Message = []Message{Message{Id: 1, Sender: "alice", MsgType: MessageTypeText{}, Content: "Hello everyone!", Timestamp: 1000}, Message{Id: 2, Sender: "bob", MsgType: MessageTypeText{}, Content: "Hi Alice!", Timestamp: 1001}, Message{Id: 3, Sender: "system", MsgType: MessageTypeSystem{}, Content: "bob joined the chat", Timestamp: 999}, Message{Id: 4, Sender: "alice", MsgType: MessageTypeImage{}, Content: "photo.png", Timestamp: 1002}, Message{Id: 5, Sender: "bob", MsgType: MessageTypeAck{}, Content: "ack:1", Timestamp: 1003}}
	fmt.Println("--- Encoded Messages ---")
	for _, msg := range messages {
		encoded := Encode(msg)
		fmt.Println(fmt.Sprintf("  %v", encoded))
	}
	fmt.Println("")
	fmt.Println("--- Decode Round-Trip ---")
	testRaw := "TEXT|1|alice|1000|Hello everyone!"
	decoded := Decode(testRaw)
	__res := decoded
	if __res.tag == "Ok" { msg := __res.v; _ = msg; 
		fmt.Println(fmt.Sprintf("  Decoded OK: type=%v, content=%v", TypeTag(msg.MsgType), msg.Content))
	} else { e := __res.v; _ = e; 
		fmt.Println(fmt.Sprintf("  Decode error: %v", e))
	}
	badRaw := "UNKNOWN|data"
	badDecoded := Decode(badRaw)
	__res := badDecoded
	if __res.tag == "Ok" { 
		fmt.Println("  Unexpected success")
	} else { e := __res.v; _ = e; 
		fmt.Println(fmt.Sprintf("  Expected error: %v", e))
	}
	aliceMsgs := FilterBySender(messages, "alice")
	fmt.Println("")
	fmt.Println(fmt.Sprintf("--- Alice's Messages (%v) ---", int64(len(aliceMsgs))))
	for _, msg := range aliceMsgs {
		fmt.Println(fmt.Sprintf("  %v", msg.Serialize()))
	}
	systemMsgs := FilterByType(messages, MessageTypeSystem{})
	fmt.Println("")
	fmt.Println(fmt.Sprintf("--- System Messages (%v) ---", int64(len(systemMsgs))))
	for _, msg := range systemMsgs {
		fmt.Println(fmt.Sprintf("  %v", msg.Serialize()))
	}
	first := func(__r []Message) __bockOption { if len(__r) > 0 { return __bockSome(__r[0]) }; return __bockNone }(messages)
	__opt := first
	if __opt.tag == "Some" { msg := __opt.v; _ = msg; 
		isSys := IsSystemMessage(msg)
		fmt.Println("")
		fmt.Println(fmt.Sprintf("Is first message system? %v", isSys))
	} else { 
		// empty
	}
	fmt.Println("")
	fmt.Println("--- Channel Dispatch ---")
	ch := StubChannel{}
	{
		__channel := ch
		_ = __channel
		Dispatch(messages, __channel)
	}
	fmt.Println("")
	fmt.Println("=== Done ===")
}

package main

// ── Bock concurrency runtime ──
type __bockChannel struct {
	q chan interface{}
}

func __bockChannelNew() (*__bockChannel, *__bockChannel) {
	c := &__bockChannel{q: make(chan interface{}, 1024)}
	return c, c
}
func (c *__bockChannel) send(v interface{}) { c.q <- v }
func (c *__bockChannel) recv() interface{}  { return <-c.q }
func (c *__bockChannel) close()              {}

// __bockSpawn launches the passed channel-returning async computation.
// In practice the Go async-fn lowerer already wraps bodies in goroutines,
// so this is the identity on a receive channel.
func __bockSpawn(ch interface{}) interface{} { return ch }

// ── Bock Optional runtime ──
type __bockOption struct {
	tag string
	v   interface{}
}

func __bockSome(v interface{}) __bockOption { return __bockOption{tag: "Some", v: v} }

var __bockNone = __bockOption{tag: "None"}

// ── Bock Result runtime ──
type __bockResult struct {
	tag string
	v   interface{}
}

func __bockOk(v interface{}) __bockResult { return __bockResult{tag: "Ok", v: v} }

func __bockErr(v interface{}) __bockResult { return __bockResult{tag: "Err", v: v} }

// ── Bock numeric payload helpers ──
func __bockAsInt64(v interface{}) int64 {
	switch n := v.(type) {
	case int64:
		return n
	case int:
		return int64(n)
	case int32:
		return int64(n)
	case float64:
		return int64(n)
	default:
		return 0
	}
}

func __bockAsFloat64(v interface{}) float64 {
	switch n := v.(type) {
	case float64:
		return n
	case float32:
		return float64(n)
	case int64:
		return float64(n)
	case int:
		return float64(n)
	default:
		return 0
	}
}

package main

type Ordering interface {
	isOrdering()
}

type OrderingLess struct{}

func (OrderingLess) isOrdering() {}

type OrderingEqual struct{}

func (OrderingEqual) isOrdering() {}

type OrderingGreater struct{}

func (OrderingGreater) isOrdering() {}

type Equatable interface {
	Eq(interface{}, /* Self */) bool
}

type Comparable interface {
	Compare(interface{}, /* Self */) Ordering
}

type Key struct {
	Value	int64
}

func (k Key) Compare(self interface{}, other Key) Ordering {
	return func() interface{} { if (self.Value < other.Value) { return less } else { return func() interface{} { if (self.Value == other.Value) { return equal } else { return greater } }() } }()
}

func (k Key) Eq(self interface{}, other Key) bool {
	return (self.Value == other.Value)
}

func Key(value int64) Key {
	return Key{Value: value}
}

func Max[T Comparable](a T, b T) T {
	return func() interface{} { switch a.Compare(a, b) { case Greater: return a default: return b } return nil }()
}

func Min[T Comparable](a T, b T) T {
	return func() interface{} { switch a.Compare(a, b) { case Less: return a default: return b } return nil }()
}

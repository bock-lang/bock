package main

type From interface {
	From(T) /* Self */
}

type Into interface {
	Into(interface{}) T
}

type TryFrom interface {
	TryFrom(T) Result[/* Self */, ConvertError]
}

type Displayable interface {
	ToString(interface{}) string
}

type ConvertError struct {
	Message	string
}

func ConvertError(message string) ConvertError {
	return ConvertError{Message: message}
}

type Celsius struct {
	Degrees	float64
}

type Fahrenheit struct {
	Degrees	float64
}

func (f Fahrenheit) From(value Celsius) Fahrenheit {
	return Fahrenheit{Degrees: ((value.Degrees * 1.8) + 32.0)}
}
